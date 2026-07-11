//! Alarm monitoring engine
//!
//! Polls enabled rules every `data_fetch_interval` seconds, reads current
//! values from SHM and creates/resolves alerts accordingly.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aether_shm_bridge::{
    PointWatchEvent, PointWatchEventListener, SubscriptionBitmap, bitmap_path_for_consumer,
};
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::db;
use crate::state::AppState;

pub async fn run_monitor(state: Arc<AppState>, shutdown: CancellationToken) {
    let interval = Duration::from_secs(state.config.data_fetch_interval);
    info!(
        "Alarm monitor started (interval={}s)",
        state.config.data_fetch_interval
    );

    // Mark as running
    {
        let mut ms = state.monitor_status.write().await;
        ms.running = true;
    }

    let (listener, mut event_rx) =
        PointWatchEventListener::new(&state.config.point_watch_socket, shutdown.clone());
    let listener_task = tokio::spawn(async move {
        if let Err(error) = listener.run().await {
            warn!(
                "Alarm PointWatch listener unavailable; polling fallback remains active: {error}"
            );
        }
    });
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut events_open = true;

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("Alarm monitor shutting down");
                break;
            }
            _ = ticker.tick() => {
                check_all_rules(&state).await;
            }
            event = event_rx.recv(), if events_open => {
                match event {
                    Some(event) => check_event_batch(&state, &mut event_rx, event, &shutdown).await,
                    None => events_open = false,
                }
            }
        }
    }

    let _ = tokio::time::timeout(Duration::from_secs(2), listener_task).await;

    let mut ms = state.monitor_status.write().await;
    ms.running = false;
}

pub async fn run_alarm_count_broadcaster(state: Arc<AppState>, shutdown: CancellationToken) {
    info!("Alarm count broadcast task started (interval=30s)");
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                send_alarm_count_broadcast(&state).await;
            }
        }
    }
}

async fn check_all_rules(state: &Arc<AppState>) {
    let rules = match db::get_all_enabled_rules(&state.db).await {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to load enabled rules: {}", e);
            return;
        },
    };

    if rules.is_empty() {
        debug!("No enabled rules to check");
        reconcile_point_watch_subscriptions(state, &rules);
        state.monitor_status.write().await.last_check_time = Some(Utc::now().timestamp());
        return;
    }

    debug!("Checking {} enabled rules", rules.len());
    reconcile_point_watch_subscriptions(state, &rules);
    process_rules(state, rules).await;

    state.monitor_status.write().await.last_check_time = Some(Utc::now().timestamp());
}

async fn process_rules(state: &Arc<AppState>, rules: Vec<crate::models::AlertRule>) {
    if rules.is_empty() {
        return;
    }

    // Process all rules concurrently
    let tasks: Vec<_> = rules
        .into_iter()
        .map(|rule| {
            let state = Arc::clone(state);
            tokio::spawn(async move {
                check_single_rule(state, rule).await;
            })
        })
        .collect();

    futures::future::join_all(tasks).await;
}

async fn check_event_batch(
    state: &Arc<AppState>,
    event_rx: &mut tokio::sync::mpsc::Receiver<PointWatchEvent>,
    first: PointWatchEvent,
    shutdown: &CancellationToken,
) {
    let mut slots = HashSet::new();
    if let Ok(slot) = usize::try_from(first.slot_index()) {
        slots.insert(slot);
    }
    tokio::select! {
        _ = shutdown.cancelled() => return,
        _ = tokio::time::sleep(Duration::from_millis(state.config.point_watch_debounce_ms)) => {}
    }
    while let Ok(event) = event_rx.try_recv() {
        if let Ok(slot) = usize::try_from(event.slot_index()) {
            slots.insert(slot);
        }
    }
    if slots.is_empty() {
        return;
    }

    let rules = match db::get_all_enabled_rules(&state.db).await {
        Ok(rules) => rules,
        Err(error) => {
            error!("Failed to load enabled rules for PointWatch event: {error}");
            return;
        },
    };
    let matching = rules
        .into_iter()
        .filter(|rule| match state.live_values.watched_slot(rule) {
            Ok(Some(slot)) => slots.contains(&slot),
            Ok(None) => false,
            Err(error) => {
                warn!(
                    "Cannot resolve PointWatch slot for rule '{}': {error}",
                    rule.rule_name
                );
                false
            },
        })
        .collect::<Vec<_>>();
    if !matching.is_empty() {
        debug!("PointWatch woke {} alarm rule(s)", matching.len());
        process_rules(state, matching).await;
    }
}

fn reconcile_point_watch_subscriptions(state: &AppState, rules: &[crate::models::AlertRule]) {
    let bitmap_path = bitmap_path_for_consumer(Path::new(&state.config.shm_path), "alarm");
    let bitmap = match SubscriptionBitmap::open(&bitmap_path) {
        Ok(bitmap) => bitmap,
        Err(error) => {
            debug!(
                "Alarm PointWatch bitmap not available at {}: {error}",
                bitmap_path.display()
            );
            return;
        },
    };
    bitmap.clear_all();
    for rule in rules {
        match state.live_values.watched_slot(rule) {
            Ok(Some(slot)) => bitmap.set_watched(slot),
            Ok(None) => {},
            Err(error) => warn!(
                "Cannot subscribe alarm rule '{}' to PointWatch: {error}",
                rule.rule_name
            ),
        }
    }
    debug!(
        "Alarm PointWatch subscriptions reconciled: {} slot(s)",
        bitmap.subscription_count()
    );
}

async fn check_single_rule(state: Arc<AppState>, rule: crate::models::AlertRule) {
    let current_value = match state.live_values.read_rule(&rule) {
        Ok(Some(sample)) => sample.value(),
        Ok(None) => {
            debug!(
                "No live SHM data for rule '{}' at logical_key={} point_id={}",
                rule.rule_name,
                rule.logical_key(),
                rule.point_id
            );
            return;
        },
        Err(e) => {
            warn!(
                retryable = e.is_retryable(),
                "SHM read failed for rule '{}': {}", rule.rule_name, e
            );
            return;
        },
    };

    let is_triggered = rule.evaluate(current_value);

    let existing_alert = match db::get_alert_by_rule_id(&state.db, rule.id).await {
        Ok(a) => a,
        Err(e) => {
            error!(
                "DB error checking alert for rule '{}': {}",
                rule.rule_name, e
            );
            return;
        },
    };

    if is_triggered {
        if let Some(alert) = existing_alert {
            // Already active – just update current value
            if let Err(e) = db::update_alert_value(&state.db, alert.id, current_value).await {
                error!("Failed to update alert value: {}", e);
            }
            debug!(
                "Updated alert '{}': value={}",
                rule.rule_name, current_value
            );
        } else {
            // New alarm triggered
            match db::insert_alert(&state.db, &rule, current_value).await {
                Ok(alert_id) => {
                    warn!(
                        "ALARM TRIGGERED: rule='{}' value={} {} {}",
                        rule.rule_name, current_value, rule.operator, rule.value
                    );
                    state
                        .broadcaster
                        .send_alarm_triggered(alert_id, &rule, current_value)
                        .await;
                    send_alarm_count_broadcast(&state).await;
                },
                Err(e) => {
                    error!(
                        "Failed to insert alert for rule '{}': {}",
                        rule.rule_name, e
                    );
                },
            }
        }
    } else if let Some(alert) = existing_alert {
        // Alarm recovered
        match db::resolve_alert(&state.db, &alert, current_value).await {
            Ok(_event_id) => {
                info!(
                    "ALARM RECOVERED: rule='{}' value={}",
                    rule.rule_name, current_value
                );
                state
                    .broadcaster
                    .send_alarm_recovery(alert.id, &rule, Some(current_value), "条件恢复")
                    .await;
                send_alarm_count_broadcast(&state).await;
            },
            Err(e) => {
                error!(
                    "Failed to resolve alert for rule '{}': {}",
                    rule.rule_name, e
                );
            },
        }
    }
}

async fn send_alarm_count_broadcast(state: &Arc<AppState>) {
    match db::get_active_alarm_counts(&state.db).await {
        Ok(counts) => {
            state.broadcaster.send_alarm_count(&counts).await;
        },
        Err(e) => {
            error!("Failed to get alarm counts: {}", e);
        },
    }
}

/// Called when a rule is updated – resolves its alerts if it's now disabled.
pub async fn on_rule_updated(state: &Arc<AppState>, rule_id: i64) {
    let rule = match db::get_rule_by_id(&state.db, rule_id).await {
        Ok(Some(r)) => r,
        _ => return,
    };

    if !rule.enabled {
        let resolved = db::resolve_alerts_by_rule_id(&state.db, rule_id)
            .await
            .unwrap_or_default();
        for alert in &resolved {
            state
                .broadcaster
                .send_alarm_recovery(alert.id, &rule, None, "规则被禁用")
                .await;
        }
        // Broadcast updated count only when at least one alert was resolved.
        // (Must check `resolved` before the DB call, since the alerts are
        // already marked recovered at this point.)
        if !resolved.is_empty() {
            send_alarm_count_broadcast(state).await;
        }
    }
}

/// Called when a rule is deleted – resolves its active alerts first.
pub async fn on_rule_deleted(state: &Arc<AppState>, rule: &crate::models::AlertRule) {
    let resolved = db::resolve_alerts_by_rule_id(&state.db, rule.id)
        .await
        .unwrap_or_default();
    for alert in &resolved {
        state
            .broadcaster
            .send_alarm_recovery(alert.id, rule, None, "规则被删除")
            .await;
    }
    if !resolved.is_empty() {
        send_alarm_count_broadcast(state).await;
    }
}

/// Manual rule check (for the `/monitor/check-rule/{id}` endpoint).
pub async fn manual_check_rule(
    state: &Arc<AppState>,
    rule_id: i64,
) -> anyhow::Result<serde_json::Value> {
    let rule = db::get_rule_by_id(&state.db, rule_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Rule not found"))?;

    if !rule.enabled {
        return Ok(serde_json::json!({
            "success": false,
            "message": "Rule is disabled",
            "data": {},
        }));
    }

    let sample = state
        .live_values
        .read_rule(&rule)
        .map_err(|error| anyhow::anyhow!(error))?;

    let Some(sample) = sample else {
        return Ok(serde_json::json!({
            "success": false,
            "message": "Failed to retrieve live data from SHM",
            "data": {
                "logical_key": rule.logical_key(),
                "point_id": rule.point_id,
                "data_source": "shm",
            },
        }));
    };

    let current_value = sample.value();
    let is_triggered = rule.evaluate(current_value);
    let has_active = db::get_alert_by_rule_id(&state.db, rule.id)
        .await?
        .is_some();

    Ok(serde_json::json!({
        "success": true,
        "message": "Manual check completed",
        "data": {
            "rule_name": rule.rule_name,
            "current_value": current_value,
            "threshold_value": rule.value,
            "operator": rule.operator,
            "is_triggered": is_triggered,
            "has_active_alert": has_active,
            "logical_key": rule.logical_key(),
            "point_id": rule.point_id,
            "data_source": "shm",
            "sample_timestamp_ms": sample.timestamp_ms(),
            "check_time": chrono::Utc::now().to_rfc3339(),
        },
    }))
}
