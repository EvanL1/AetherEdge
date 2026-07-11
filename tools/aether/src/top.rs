//! Interactive TUI dashboard for real-time AetherEMS monitoring
//!
//! `aether top` — hierarchical navigation across all services:
//! ←→ switch view (Channels / Instances / Rules) | ↑↓ navigate | Enter drill | Esc back
//!
//! Drawing logic is in `top_draw.rs`.

use anyhow::Result;
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::TableState;
use reqwest::Client;
use serde_json::Value;
use std::io;
use std::time::{Duration, Instant};

// ─── Data Model (pub(crate) for top_draw access) ────────────────────────────

pub(crate) struct ChannelInfo {
    pub id: u32,
    pub name: String,
    pub protocol: String,
    pub connected: bool,
    pub error_count: u64,
    pub read_count: u64,
    pub point_count: usize,
    pub address: String,
}

pub(crate) struct PointRow {
    pub point_id: u32,
    pub name: String,
    pub value: f64,
    pub unit: String,
    /// Millisecond epoch timestamp of last update (0 = unknown)
    pub ts_ms: u64,
}

pub(crate) struct InstanceInfo {
    pub id: u32,
    pub name: String,
    pub product: String,
}

pub(crate) struct InstPointRow {
    pub point_id: u32,
    pub kind: &'static str,
    pub name: String,
    pub value: f64,
    pub unit: String,
    /// Routing source/target, e.g. "Ch1:T.1" or "Ch1:C.3"
    pub routing: String,
    /// Millisecond epoch timestamp of last update (0 = unknown)
    pub ts_ms: u64,
}

pub(crate) struct RuleInfo {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub description: String,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum View {
    Channels,
    Instances,
    Rules,
}

impl View {
    pub const ALL: [View; 3] = [View::Channels, View::Instances, View::Rules];

    pub fn index(self) -> usize {
        match self {
            View::Channels => 0,
            View::Instances => 1,
            View::Rules => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            View::Channels => "Channels",
            View::Instances => "Instances",
            View::Rules => "Rules",
        }
    }

    fn next(self) -> Self {
        View::ALL[(self.index() + 1) % View::ALL.len()]
    }

    fn prev(self) -> Self {
        View::ALL[(self.index() + View::ALL.len() - 1) % View::ALL.len()]
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Depth {
    List,
    Detail,
}

pub(crate) struct TopState {
    pub view: View,
    pub depth: Depth,
    pub channels: Vec<ChannelInfo>,
    pub ch_table: TableState,
    pub points: Vec<PointRow>,
    pub pt_table: TableState,
    pub hide_zero: bool,
    pub last_ch_id: Option<u32>,
    pub instances: Vec<InstanceInfo>,
    pub inst_table: TableState,
    pub inst_points: Vec<InstPointRow>,
    pub inst_pt_table: TableState,
    pub last_inst_id: Option<u32>,
    pub rules: Vec<RuleInfo>,
    pub rule_table: TableState,
    pub last_refresh: Instant,
    pub host_display: String,
    pub error_msg: Option<String>,
    pub shm_ok: bool,
}

impl TopState {
    fn new(host_display: String) -> Self {
        Self {
            view: View::Channels,
            depth: Depth::List,
            channels: Vec::new(),
            ch_table: TableState::default(),
            points: Vec::new(),
            pt_table: TableState::default(),
            hide_zero: false,
            last_ch_id: None,
            instances: Vec::new(),
            inst_table: TableState::default(),
            inst_points: Vec::new(),
            inst_pt_table: TableState::default(),
            last_inst_id: None,
            rules: Vec::new(),
            rule_table: TableState::default(),
            last_refresh: Instant::now() - Duration::from_secs(10),
            host_display,
            error_msg: None,
            shm_ok: false,
        }
    }

    fn active_table(&mut self) -> &mut TableState {
        if self.depth == Depth::Detail {
            return match self.view {
                View::Channels => &mut self.pt_table,
                View::Instances => &mut self.inst_pt_table,
                View::Rules => &mut self.rule_table,
            };
        }
        match self.view {
            View::Channels => &mut self.ch_table,
            View::Instances => &mut self.inst_table,
            View::Rules => &mut self.rule_table,
        }
    }

    fn active_len(&self) -> usize {
        if self.depth == Depth::Detail {
            return match self.view {
                View::Channels => self.visible_points().len(),
                View::Instances => self.inst_points.len(),
                View::Rules => self.rules.len(),
            };
        }
        match self.view {
            View::Channels => self.channels.len(),
            View::Instances => self.instances.len(),
            View::Rules => self.rules.len(),
        }
    }

    fn move_up(&mut self) {
        let tbl = self.active_table();
        let i = tbl.selected().unwrap_or(0);
        if i > 0 {
            tbl.select(Some(i - 1));
        }
    }

    fn move_down(&mut self) {
        let max = self.active_len().saturating_sub(1);
        let tbl = self.active_table();
        let i = tbl.selected().unwrap_or(0);
        if i < max {
            tbl.select(Some(i + 1));
        }
    }

    fn selected_ch_id(&self) -> Option<u32> {
        self.ch_table
            .selected()
            .and_then(|i| self.channels.get(i))
            .map(|ch| ch.id)
    }

    fn selected_inst_id(&self) -> Option<u32> {
        self.inst_table
            .selected()
            .and_then(|i| self.instances.get(i))
            .map(|inst| inst.id)
    }

    pub fn visible_points(&self) -> Vec<&PointRow> {
        self.points
            .iter()
            // hide_zero hides only real zero readings. NaN (rendered "—",
            // meaning "no data fetched") satisfies `value != 0.0` because
            // NaN ≠ 0.0 in IEEE-754, so missing-data rows stay visible.
            // Don't conflate "device returned zero" with "device offline".
            .filter(|p| !self.hide_zero || p.value != 0.0)
            .collect()
    }
}

// ─── Entry Point ────────────────────────────────────────────────────────────

pub async fn run_top(io_url: &str, automation_url: &str) -> Result<()> {
    let handle = tokio::runtime::Handle::current();
    let client = Client::builder().timeout(Duration::from_secs(5)).build()?;

    let host_display = io_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split(':')
        .next()
        .unwrap_or("localhost")
        .to_string();
    let mut state = TopState::new(host_display);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let urls = Urls {
        io: io_url.to_string(),
        automation: automation_url.to_string(),
    };

    let result = tokio::task::block_in_place(|| {
        run_event_loop(&mut terminal, &client, &urls, &mut state, &handle)
    });

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    result
}

struct Urls {
    io: String,
    automation: String,
}

// ─── Event Loop ─────────────────────────────────────────────────────────────

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    client: &Client,
    urls: &Urls,
    state: &mut TopState,
    handle: &tokio::runtime::Handle,
) -> Result<()> {
    loop {
        if state.last_refresh.elapsed() >= Duration::from_secs(1) {
            do_refresh(handle, client, urls, state);
        }

        if state.view == View::Channels && state.selected_ch_id() != state.last_ch_id {
            reload_ch_points(handle, client, &urls.io, state);
        }
        if state.view == View::Instances
            && state.depth == Depth::Detail
            && state.selected_inst_id() != state.last_inst_id
        {
            reload_inst_points(handle, client, &urls.automation, state);
        }

        terminal.draw(|f| crate::top_draw::draw_ui(f, state))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && handle_key(key.code, state, handle, client, urls)
        {
            return Ok(());
        }
    }
}

/// Returns true if the app should exit.
fn handle_key(
    code: KeyCode,
    state: &mut TopState,
    handle: &tokio::runtime::Handle,
    client: &Client,
    urls: &Urls,
) -> bool {
    match code {
        KeyCode::Char('q') => return true,
        // Esc always goes back one level; never exits
        KeyCode::Esc => {
            if state.depth == Depth::Detail {
                state.depth = Depth::List;
            }
            // At list level, Esc is no-op (use q to quit)
        },
        KeyCode::Left => {
            if state.depth == Depth::Detail {
                state.depth = Depth::List;
            } else {
                state.view = state.view.prev();
            }
        },
        KeyCode::Right => {
            if state.depth == Depth::List {
                state.view = state.view.next();
            }
        },
        KeyCode::Tab => {
            state.view = state.view.next();
            state.depth = Depth::List;
        },
        KeyCode::BackTab => {
            state.view = state.view.prev();
            state.depth = Depth::List;
        },
        KeyCode::Enter => {
            if state.depth == Depth::List {
                match state.view {
                    View::Channels => {
                        state.depth = Depth::Detail;
                        state.pt_table.select(Some(0));
                    },
                    View::Instances => {
                        state.depth = Depth::Detail;
                        state.inst_pt_table.select(Some(0));
                        reload_inst_points(handle, client, &urls.automation, state);
                    },
                    View::Rules => {},
                }
            }
        },
        KeyCode::Up | KeyCode::Char('k') => state.move_up(),
        KeyCode::Down | KeyCode::Char('j') => state.move_down(),
        KeyCode::Char('z') => state.hide_zero = !state.hide_zero,
        KeyCode::Char('r') => {
            do_refresh(handle, client, urls, state);
            if state.view == View::Channels {
                reload_ch_points(handle, client, &urls.io, state);
            }
            if state.view == View::Instances && state.depth == Depth::Detail {
                reload_inst_points(handle, client, &urls.automation, state);
            }
        },
        KeyCode::Char('1') => {
            state.view = View::Channels;
            state.depth = Depth::List;
        },
        KeyCode::Char('2') => {
            state.view = View::Instances;
            state.depth = Depth::List;
        },
        KeyCode::Char('3') => {
            state.view = View::Rules;
            state.depth = Depth::List;
        },
        _ => {},
    }
    false
}

// ─── Refresh ────────────────────────────────────────────────────────────────

fn do_refresh(handle: &tokio::runtime::Handle, client: &Client, urls: &Urls, state: &mut TopState) {
    let prev_pt = state.pt_table.selected();

    if let Ok(chs) = handle.block_on(fetch_channels(client, &urls.io)) {
        state.channels = chs;
        clamp_select(&mut state.ch_table, state.channels.len());
    }

    if let Some(ch_id) = state.selected_ch_id()
        && state.last_ch_id == Some(ch_id)
        && let Ok((pts, shm_ok)) = handle.block_on(load_points(client, &urls.io, ch_id))
    {
        state.points = pts;
        state.shm_ok = shm_ok;
        let max = state.visible_points().len().saturating_sub(1);
        state.pt_table.select(prev_pt.map(|s| s.min(max)));
    }

    if state.view == View::Instances
        && state.depth == Depth::Detail
        && let Some(inst_id) = state.selected_inst_id()
    {
        let prev_ipt = state.inst_pt_table.selected();
        if let Ok((pts, shm_ok)) =
            handle.block_on(fetch_inst_points(client, &urls.automation, inst_id))
        {
            state.inst_points = pts;
            state.shm_ok |= shm_ok;
            let max = state.inst_points.len().saturating_sub(1);
            state.inst_pt_table.select(prev_ipt.map(|s| s.min(max)));
        }
    }

    if let Ok(insts) = handle.block_on(fetch_instances(client, &urls.automation)) {
        state.instances = insts;
        clamp_select(&mut state.inst_table, state.instances.len());
    }

    if let Ok(rules) = handle.block_on(fetch_rules(client, &urls.automation)) {
        state.rules = rules;
        clamp_select(&mut state.rule_table, state.rules.len());
    }

    state.error_msg = None;
    state.last_refresh = Instant::now();
}

fn reload_ch_points(
    handle: &tokio::runtime::Handle,
    client: &Client,
    io_url: &str,
    state: &mut TopState,
) {
    let ch_id = state.selected_ch_id();
    state.last_ch_id = ch_id;
    if let Some(id) = ch_id
        && let Ok((pts, shm_ok)) = handle.block_on(load_points(client, io_url, id))
    {
        state.points = pts;
        state.shm_ok = shm_ok;
        state.pt_table.select(Some(0));
    }
}

fn reload_inst_points(
    handle: &tokio::runtime::Handle,
    client: &Client,
    automation_url: &str,
    state: &mut TopState,
) {
    let inst_id = state.selected_inst_id();
    state.last_inst_id = inst_id;
    if let Some(id) = inst_id
        && let Ok((pts, shm_ok)) = handle.block_on(fetch_inst_points(client, automation_url, id))
    {
        state.inst_points = pts;
        state.shm_ok |= shm_ok;
        state.inst_pt_table.select(Some(0));
    }
}

fn clamp_select(table: &mut TableState, len: usize) {
    if len == 0 {
        table.select(None);
    } else if table.selected().is_none() {
        table.select(Some(0));
    } else if let Some(s) = table.selected()
        && s >= len
    {
        table.select(Some(len - 1));
    }
}

// ─── Data Fetching ──────────────────────────────────────────────────────────

async fn fetch_channels(client: &Client, base_url: &str) -> Result<Vec<ChannelInfo>> {
    let json: Value = client
        .get(format!("{base_url}/api/channels"))
        .send()
        .await?
        .json()
        .await?;

    let raw = json
        .pointer("/data/list")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut out = Vec::with_capacity(raw.len());
    for ch in &raw {
        let id = ch["id"].as_u64().unwrap_or(0) as u32;
        if id == 0 {
            continue;
        }
        out.push(build_channel(client, base_url, id, ch).await);
    }
    out.sort_by_key(|c| c.id);
    Ok(out)
}

async fn build_channel(client: &Client, base_url: &str, id: u32, ch: &Value) -> ChannelInfo {
    let name = ch["name"].as_str().unwrap_or("").to_string();
    let protocol = ch["protocol"].as_str().unwrap_or("").to_string();
    let (connected, error_count, read_count, point_count, address) =
        fetch_ch_stats(client, base_url, id).await;
    ChannelInfo {
        id,
        name,
        protocol,
        connected,
        error_count,
        read_count,
        point_count,
        address,
    }
}

async fn fetch_ch_stats(
    client: &Client,
    base_url: &str,
    id: u32,
) -> (bool, u64, u64, usize, String) {
    let url = format!("{base_url}/api/channels/{id}/status");
    let Ok(resp) = client.get(&url).send().await else {
        return (false, 0, 0, 0, String::new());
    };
    let Ok(json): Result<Value, _> = resp.json().await else {
        return (false, 0, 0, 0, String::new());
    };
    let d = json.pointer("/data").unwrap_or(&Value::Null);
    let s = d.get("statistics").unwrap_or(&Value::Null);
    let x = s.get("extra").unwrap_or(&Value::Null);
    (
        d["connected"].as_bool().unwrap_or(false),
        s["error_count"].as_u64().unwrap_or(0),
        s["read_count"].as_u64().unwrap_or(0),
        x["points"].as_u64().unwrap_or(0) as usize,
        x["address"].as_str().unwrap_or("").to_string(),
    )
}

async fn load_points(client: &Client, base_url: &str, ch_id: u32) -> Result<(Vec<PointRow>, bool)> {
    let defs = fetch_point_defs(client, base_url, ch_id).await?;
    let mut rows = Vec::with_capacity(defs.len());
    let mut shm_reachable = defs.is_empty();

    for (pid, name, unit) in defs {
        let sample = match fetch_channel_sample(client, base_url, ch_id, pid).await {
            Ok(sample) => {
                shm_reachable = true;
                sample
            },
            Err(_) => None,
        };
        let (value, ts_ms) = sample.unwrap_or((f64::NAN, 0));
        rows.push(PointRow {
            point_id: pid,
            value,
            ts_ms,
            name,
            unit,
        });
    }

    Ok((rows, shm_reachable))
}

async fn fetch_point_defs(
    client: &Client,
    base_url: &str,
    ch_id: u32,
) -> Result<Vec<(u32, String, String)>> {
    let json: Value = client
        .get(format!("{base_url}/api/channels/{ch_id}/points"))
        .send()
        .await?
        .json()
        .await?;

    let arr = json
        .pointer("/data/telemetry")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(arr
        .iter()
        .map(|p| {
            (
                p["point_id"].as_u64().unwrap_or(0) as u32,
                p["signal_name"].as_str().unwrap_or("").to_string(),
                p["unit"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect())
}

async fn fetch_channel_sample(
    client: &Client,
    base_url: &str,
    channel_id: u32,
    point_id: u32,
) -> Result<Option<(f64, u64)>> {
    let response = client
        .get(format!("{base_url}/api/channels/{channel_id}/T/{point_id}"))
        .send()
        .await?
        .error_for_status()?;
    let json: Value = response.json().await?;
    Ok(json.pointer("/data").and_then(parse_live_sample))
}

async fn fetch_instances(client: &Client, base_url: &str) -> Result<Vec<InstanceInfo>> {
    let json: Value = client
        .get(format!("{base_url}/api/instances"))
        .send()
        .await?
        .json()
        .await?;

    let arr = json
        .pointer("/data/list")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(arr
        .iter()
        .map(|i| InstanceInfo {
            id: i["instance_id"].as_u64().unwrap_or(0) as u32,
            name: i["instance_name"].as_str().unwrap_or("").to_string(),
            product: i["product_name"].as_str().unwrap_or("").to_string(),
        })
        .collect())
}

async fn fetch_rules(client: &Client, base_url: &str) -> Result<Vec<RuleInfo>> {
    let json: Value = client
        .get(format!("{base_url}/api/rules"))
        .send()
        .await?
        .json()
        .await?;

    let arr = json
        .pointer("/data/list")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(arr
        .iter()
        .map(|r| RuleInfo {
            id: r["id"].as_i64().unwrap_or(0),
            name: r["name"].as_str().unwrap_or("").to_string(),
            enabled: r["enabled"].as_bool().unwrap_or(false),
            description: r["description"].as_str().unwrap_or("").to_string(),
        })
        .collect())
}

async fn fetch_inst_points(
    client: &Client,
    automation_url: &str,
    inst_id: u32,
) -> Result<(Vec<InstPointRow>, bool)> {
    let json: Value = client
        .get(format!("{automation_url}/api/instances/{inst_id}/points"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let data = json.pointer("/data").unwrap_or(&Value::Null);
    let live_response = client
        .get(format!("{automation_url}/api/instances/{inst_id}/data"))
        .send()
        .await;
    let (live, shm_reachable) = match live_response {
        Ok(response) if response.status().is_success() => {
            let json = response.json::<Value>().await.unwrap_or(Value::Null);
            (json.pointer("/data").cloned().unwrap_or(Value::Null), true)
        },
        _ => (Value::Null, false),
    };

    let mut rows = Vec::new();
    if let Some(arr) = data.get("measurements").and_then(|v| v.as_array()) {
        for p in arr {
            let pid = p["measurement_id"].as_u64().unwrap_or(0) as u32;
            let sample = live
                .pointer(&format!("/measurements/{pid}"))
                .and_then(parse_live_sample);
            let (value, ts_ms) = sample.unwrap_or((f64::NAN, 0));
            rows.push(InstPointRow {
                point_id: pid,
                kind: "M",
                name: p["name"].as_str().unwrap_or("").to_string(),
                value,
                unit: p["unit"].as_str().unwrap_or("").to_string(),
                routing: format_routing(p, "←"),
                ts_ms,
            });
        }
    }
    if let Some(arr) = data.get("actions").and_then(|v| v.as_array()) {
        for p in arr {
            let pid = p["action_id"].as_u64().unwrap_or(0) as u32;
            let sample = live
                .pointer(&format!("/actions/{pid}"))
                .and_then(parse_live_sample);
            let (value, ts_ms) = sample.unwrap_or((f64::NAN, 0));
            rows.push(InstPointRow {
                point_id: pid,
                kind: "A",
                name: p["name"].as_str().unwrap_or("").to_string(),
                value,
                unit: p["unit"].as_str().unwrap_or("").to_string(),
                routing: format_routing(p, "→"),
                ts_ms,
            });
        }
    }
    Ok((rows, shm_reachable))
}

/// Format routing info from point JSON: "← Ch1:T.5" or "→ Ch1:C.3"
fn format_routing(point: &Value, arrow: &str) -> String {
    let r = match point.get("routing") {
        Some(v) if !v.is_null() => v,
        _ => return String::new(),
    };
    let ch_id = r["channel_id"].as_u64().unwrap_or(0);
    let ch_type = r["channel_type"].as_str().unwrap_or("?");
    let pt_id = r["channel_point_id"].as_u64().unwrap_or(0);
    if ch_id == 0 {
        return String::new();
    }
    format!("{arrow} Ch{ch_id}:{ch_type}.{pt_id}")
}

/// Current time in milliseconds (for staleness comparison)
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Format a millisecond epoch timestamp as HH:MM:SS local time
pub(crate) fn format_time(ts_ms: u64) -> String {
    if ts_ms == 0 {
        return "-".to_string();
    }
    use chrono::{Local, TimeZone};
    let secs = (ts_ms / 1000) as i64;
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => {
            let today = Local::now().date_naive();
            if dt.date_naive() == today {
                dt.format("%H:%M:%S").to_string()
            } else {
                dt.format("%m-%d %H:%M:%S").to_string()
            }
        },
        _ => "-".to_string(),
    }
}

/// Is this timestamp stale (older than threshold)?
pub(crate) fn is_stale(ts_ms: u64, threshold_s: u64) -> bool {
    if ts_ms == 0 {
        return true;
    }
    now_ms().saturating_sub(ts_ms) / 1000 > threshold_s
}

fn parse_live_sample(sample: &Value) -> Option<(f64, u64)> {
    let value = sample.get("value").and_then(|value| {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    })?;
    if !value.is_finite() {
        return None;
    }
    let timestamp = sample
        .get("timestamp_ms")
        .or_else(|| sample.get("timestamp"))
        .and_then(|timestamp| {
            timestamp
                .as_u64()
                .or_else(|| timestamp.as_str().and_then(|value| value.parse().ok()))
        })
        .unwrap_or(0);
    Some((value, timestamp))
}

#[cfg(test)]
mod tests {
    use super::parse_live_sample;

    #[test]
    fn parses_shm_samples_from_channel_and_instance_apis() {
        let channel = serde_json::json!({
            "value": "12.5",
            "timestamp": "1729000815000",
            "source": "shm"
        });
        let instance = serde_json::json!({
            "value": 7.25,
            "timestamp_ms": 1729000816000_u64
        });

        assert_eq!(parse_live_sample(&channel), Some((12.5, 1729000815000)));
        assert_eq!(parse_live_sample(&instance), Some((7.25, 1729000816000)));
        assert_eq!(parse_live_sample(&serde_json::json!({"value": null})), None);
    }
}
