//! Optional loopback-only HTTP dashboard for SHM observability.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use aether_shm_bridge::{ShmObserver, ShmTopologyObservation};
use anyhow::{Context, Result, bail};
use axum::Router;
use axum::extract::State;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use tokio::sync::watch;

use crate::shm::ShmRuntimeView;

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone)]
struct DashboardState {
    observation: watch::Receiver<ShmTopologyObservation>,
    point_view: Option<Arc<ShmRuntimeView>>,
}

/// Serves the embedded dashboard until Ctrl+C.
pub async fn serve_dashboard(
    observer: ShmObserver,
    point_view: Option<Arc<ShmRuntimeView>>,
    bind: SocketAddr,
) -> Result<()> {
    if !bind.ip().is_loopback() {
        bail!("SHM dashboard is local-only; refusing non-loopback bind address {bind}");
    }

    let initial = observer.inspect();
    let (sender, receiver) = watch::channel(initial);
    let refresh_task = tokio::spawn(refresh_observation(observer, sender));
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("failed to bind SHM dashboard at {bind}"))?;
    let address = listener
        .local_addr()
        .context("failed to resolve SHM dashboard address")?;
    let url = format!("http://{address}");
    println!("Aether SHM dashboard: {url}");
    println!("Read-only, loopback-only; press Ctrl+C to stop.");

    let result = axum::serve(listener, dashboard_router(receiver, point_view))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("SHM dashboard server failed");
    refresh_task.abort();
    result
}

async fn refresh_observation(observer: ShmObserver, sender: watch::Sender<ShmTopologyObservation>) {
    let mut interval = tokio::time::interval(REFRESH_INTERVAL);
    interval.tick().await;
    loop {
        interval.tick().await;
        let observer = observer.clone();
        let observation = match tokio::task::spawn_blocking(move || observer.inspect()).await {
            Ok(observation) => observation,
            Err(error) => {
                tracing::warn!(error = %error, "SHM dashboard observer task failed");
                continue;
            },
        };
        sender.send_replace(observation);
    }
}

fn dashboard_router(
    observation: watch::Receiver<ShmTopologyObservation>,
    point_view: Option<Arc<ShmRuntimeView>>,
) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/observation", get(observation_api))
        .fallback(not_found)
        .with_state(Arc::new(DashboardState {
            observation,
            point_view,
        }))
}

async fn index() -> Response {
    secure_response(Html(DASHBOARD_HTML).into_response())
}

async fn observation_api(State(state): State<Arc<DashboardState>>) -> Response {
    let observation = state.observation.borrow().clone();
    let mut value = crate::shm::observation_json(&observation);
    if let Some(point_view) = &state.point_view {
        match point_view.point_preview(64) {
            Ok(points) => value["points"] = serde_json::Value::Array(points),
            Err(error) => value["points_error"] = serde_json::Value::String(error.to_string()),
        }
    }
    let body = axum::Json(value).into_response();
    secure_response(body)
}

async fn not_found() -> Response {
    secure_response((StatusCode::NOT_FOUND, "SHM dashboard route not found").into_response())
}

fn secure_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
    );
    response
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::warn!(error = %error, "failed to install Ctrl+C handler");
    }
}

const DASHBOARD_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Aether SHM Observatory</title>
  <style>
    :root {
      color-scheme: dark;
      --bg: #07100e;
      --panel: rgba(15, 32, 28, .82);
      --panel-strong: #10251f;
      --line: rgba(142, 211, 184, .18);
      --text: #e8f7f0;
      --muted: #89a79b;
      --good: #51e09c;
      --warn: #ffc766;
      --bad: #ff6b75;
      --blue: #62b7ff;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      min-height: 100vh;
      font: 14px/1.5 ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
      color: var(--text);
      background:
        radial-gradient(circle at 80% -10%, rgba(47, 180, 128, .19), transparent 34rem),
        radial-gradient(circle at -10% 60%, rgba(49, 119, 169, .13), transparent 32rem),
        var(--bg);
    }
    body::before {
      content: "";
      position: fixed;
      inset: 0;
      pointer-events: none;
      opacity: .2;
      background-image: linear-gradient(var(--line) 1px, transparent 1px), linear-gradient(90deg, var(--line) 1px, transparent 1px);
      background-size: 36px 36px;
      mask-image: linear-gradient(to bottom, black, transparent 75%);
    }
    main { position: relative; max-width: 1240px; margin: 0 auto; padding: 38px 24px 56px; }
    header { display: flex; align-items: flex-end; justify-content: space-between; gap: 24px; margin-bottom: 28px; }
    .eyebrow { color: var(--good); letter-spacing: .16em; text-transform: uppercase; font-size: 11px; }
    h1 { margin: 5px 0 0; font: 600 clamp(28px, 5vw, 48px)/1.05 system-ui, sans-serif; letter-spacing: -.04em; }
    .subtitle { color: var(--muted); margin: 9px 0 0; }
    .live { display: flex; align-items: center; gap: 9px; color: var(--muted); white-space: nowrap; }
    .pulse { width: 9px; height: 9px; border-radius: 50%; background: var(--good); box-shadow: 0 0 16px var(--good); }
    .hero, .card { border: 1px solid var(--line); background: var(--panel); backdrop-filter: blur(16px); box-shadow: 0 22px 70px rgba(0,0,0,.22); }
    .hero { border-radius: 18px; padding: 24px; display: grid; grid-template-columns: 1.2fr repeat(3, 1fr); gap: 22px; }
    .label { color: var(--muted); font-size: 11px; letter-spacing: .1em; text-transform: uppercase; }
    .value { margin-top: 7px; font-size: 21px; }
    .status { display: inline-flex; align-items: center; gap: 10px; font-size: 24px; font-weight: 700; text-transform: uppercase; }
    .status::before { content: ""; width: 11px; height: 11px; border-radius: 50%; background: currentColor; box-shadow: 0 0 18px currentColor; }
    .healthy { color: var(--good); }
    .degraded { color: var(--warn); }
    .unhealthy { color: var(--bad); }
    .grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 18px; margin-top: 18px; }
    .card { border-radius: 16px; padding: 22px; }
    .card-head { display: flex; justify-content: space-between; gap: 20px; align-items: center; margin-bottom: 20px; }
    .card h2 { font: 600 17px/1.2 system-ui, sans-serif; margin: 0; }
    .path { color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 70%; }
    .metrics { display: grid; grid-template-columns: repeat(3, 1fr); gap: 14px; }
    .metric { border-left: 2px solid var(--line); padding-left: 11px; min-width: 0; }
    .metric strong { display: block; margin-top: 5px; font-size: 17px; overflow: hidden; text-overflow: ellipsis; }
    .quality-card, .findings-card { grid-column: 1 / -1; }
    .quality-row { display: grid; grid-template-columns: 110px 1fr 70px; align-items: center; gap: 14px; margin-top: 12px; }
    .bar { height: 8px; background: rgba(255,255,255,.06); border-radius: 999px; overflow: hidden; }
    .fill { height: 100%; width: 0; border-radius: inherit; transition: width .35s ease; }
    .good-fill { background: var(--good); } .warn-fill { background: var(--warn); }
    .bad-fill { background: var(--bad); } .blue-fill { background: var(--blue); }
    .finding { display: grid; grid-template-columns: 90px 220px 1fr; gap: 14px; padding: 13px 0; border-top: 1px solid var(--line); }
    .finding:first-child { border-top: 0; }
    .point-table { width: 100%; border-collapse: collapse; }
    .point-table th { color: var(--muted); font-size: 11px; letter-spacing: .08em; text-align: left; text-transform: uppercase; padding: 0 10px 10px; }
    .point-table td { border-top: 1px solid var(--line); padding: 11px 10px; }
    .point-table td:nth-child(5), .point-table th:nth-child(5) { text-align: right; }
    .point-key { color: #b9ddd0; }
    .point-scroll { overflow-x: auto; }
    .empty { color: var(--good); padding-top: 4px; }
    footer { color: var(--muted); margin-top: 20px; display: flex; justify-content: space-between; gap: 20px; }
    code { color: #b9ddd0; }
    @media (max-width: 820px) {
      header { align-items: flex-start; flex-direction: column; }
      .hero { grid-template-columns: repeat(2, 1fr); }
      .grid { grid-template-columns: 1fr; }
      .quality-card, .findings-card { grid-column: auto; }
      .metrics { grid-template-columns: repeat(2, 1fr); }
      .finding { grid-template-columns: 80px 1fr; }
      .finding-message { grid-column: 1 / -1; }
    }
  </style>
</head>
<body>
<main>
  <header>
    <div><div class="eyebrow">AetherEdge / Live State</div><h1>SHM Observatory</h1><p class="subtitle">Read-only view of the committed point and channel-health planes.</p></div>
    <div class="live"><span class="pulse"></span><span>refreshing every second</span></div>
  </header>
  <section class="hero">
    <div><div class="label">Authority status</div><div id="status" class="status unhealthy">connecting</div></div>
    <div><div class="label">Publication epoch</div><div id="epoch" class="value">—</div></div>
    <div><div class="label">Point heartbeat</div><div id="point-heartbeat" class="value">—</div></div>
    <div><div class="label">Health heartbeat</div><div id="health-heartbeat" class="value">—</div></div>
  </section>
  <section class="grid">
    <article class="card" id="point-plane"><div class="card-head"><h2>Point plane</h2><span class="path" data-field="path">unavailable</span></div><div class="metrics"></div></article>
    <article class="card" id="health-plane"><div class="card-head"><h2>Channel-health plane</h2><span class="path" data-field="path">unavailable</span></div><div class="metrics"></div></article>
    <article class="card quality-card"><div class="card-head"><h2>Point quality distribution</h2><span class="path" id="quality-total">no scan</span></div><div id="quality"></div></article>
    <article class="card quality-card"><div class="card-head"><h2>Live point preview</h2><span class="path" id="point-count">0 points</span></div><div id="points" class="empty">Typed point preview requires the runtime database.</div></article>
    <article class="card findings-card"><div class="card-head"><h2>Diagnostic findings</h2><span class="path" id="finding-count">0</span></div><div id="findings" class="empty">No findings. The committed topology is coherent and current.</div></article>
  </section>
  <footer><span id="observed">Waiting for first observation…</span><span>Local read-only endpoint: <code>/api/observation</code></span></footer>
</main>
<script>
const byId = id => document.getElementById(id);
const text = (id, value) => { byId(id).textContent = value; };
const fmtAge = value => value == null ? '—' : value < 1000 ? `${value} ms` : `${(value / 1000).toFixed(1)} s`;
const fmtNumber = value => value == null ? '—' : Number(value).toLocaleString();
const fmtValue = value => value == null ? '—' : Number(value).toLocaleString(undefined, { maximumFractionDigits: 4 });
const metric = (label, value) => { const node = document.createElement('div'); node.className = 'metric'; const l = document.createElement('span'); l.className = 'label'; l.textContent = label; const v = document.createElement('strong'); v.textContent = value; node.append(l, v); return node; };
function renderPlane(id, plane) {
  const card = byId(id); const path = card.querySelector('[data-field="path"]'); const metrics = card.querySelector('.metrics'); metrics.replaceChildren();
  if (!plane) { path.textContent = 'unavailable'; metrics.append(metric('status', 'not mapped')); return; }
  path.textContent = plane.path;
  [['slots', fmtNumber(plane.slot_count)], ['size', `${fmtNumber(plane.file_size)} B`], ['generation', fmtNumber(plane.writer_generation)], ['epoch', fmtNumber(plane.publication_epoch)], ['heartbeat', fmtAge(plane.heartbeat_age_ms)], ['layout', plane.layout_hash]].forEach(([label, value]) => metrics.append(metric(label, value)));
}
function renderQuality(slots) {
  const root = byId('quality'); root.replaceChildren();
  if (!slots) { text('quality-total', 'scan disabled'); root.className = 'empty'; root.textContent = 'Start without --no-scan to collect aggregate slot quality.'; return; }
  root.className = ''; text('quality-total', `${fmtNumber(slots.present)} present / ${fmtNumber(slots.total)} slots`);
  const rows = [['Good', slots.good, 'good-fill'], ['Uncertain', slots.uncertain, 'warn-fill'], ['Bad', slots.bad, 'bad-fill'], ['Unavailable', slots.unavailable, 'blue-fill'], ['Unwritten', slots.unwritten, 'blue-fill']];
  for (const [label, count, cls] of rows) { const row = document.createElement('div'); row.className = 'quality-row'; const name = document.createElement('span'); name.textContent = label; const bar = document.createElement('div'); bar.className = 'bar'; const fill = document.createElement('div'); fill.className = `fill ${cls}`; fill.style.width = `${slots.total ? Math.min(100, count * 100 / slots.total) : 0}%`; bar.append(fill); const value = document.createElement('strong'); value.textContent = fmtNumber(count); row.append(name, bar, value); root.append(row); }
}
function renderFindings(findings) {
  const root = byId('findings'); root.replaceChildren(); text('finding-count', `${findings.length} finding${findings.length === 1 ? '' : 's'}`);
  if (!findings.length) { root.className = 'empty'; root.textContent = 'No findings. The committed topology is coherent and current.'; return; }
  root.className = '';
  for (const finding of findings) { const row = document.createElement('div'); row.className = 'finding'; const severity = document.createElement('strong'); severity.className = finding.severity; severity.textContent = finding.severity.toUpperCase(); const code = document.createElement('code'); code.textContent = finding.code; const message = document.createElement('span'); message.className = 'finding-message'; message.textContent = finding.message; row.append(severity, code, message); root.append(row); }
}
function renderPoints(points, error) {
  const root = byId('points'); root.replaceChildren(); text('point-count', `${points?.length || 0} points`);
  if (error) { root.className = 'empty'; root.textContent = error; return; }
  if (!points?.length) { root.className = 'empty'; root.textContent = 'No typed point samples are available.'; return; }
  root.className = 'point-scroll'; const table = document.createElement('table'); table.className = 'point-table'; const head = document.createElement('thead'); const hr = document.createElement('tr');
  ['Slot', 'Address', 'Quality', 'Raw', 'Value'].forEach(label => { const th = document.createElement('th'); th.textContent = label; hr.append(th); }); head.append(hr); table.append(head); const body = document.createElement('tbody');
  for (const point of points) { const row = document.createElement('tr'); const values = [point.slot, `ch:${point.channel_id}:${point.kind}:${point.point_id}`, point.present ? point.quality : 'unwritten', point.present ? fmtValue(point.raw) : '—', point.present ? fmtValue(point.value) : '—']; values.forEach((value, index) => { const td = document.createElement('td'); td.textContent = value; if (index === 1) td.className = 'point-key'; if (index === 2) td.className = point.quality || ''; row.append(td); }); body.append(row); }
  table.append(body); root.append(table);
}
function render(data) {
  const status = byId('status'); status.textContent = data.status; status.className = `status ${data.status}`;
  text('epoch', data.publication_epoch == null ? 'unverified' : fmtNumber(data.publication_epoch)); text('point-heartbeat', fmtAge(data.point?.heartbeat_age_ms)); text('health-heartbeat', fmtAge(data.health?.heartbeat_age_ms));
  renderPlane('point-plane', data.point); renderPlane('health-plane', data.health); renderQuality(data.point?.slots); renderPoints(data.points, data.points_error); renderFindings(data.findings || []);
  text('observed', `Observed ${new Date(data.observed_at_ms).toLocaleString()}`);
}
async function refresh() {
  try { const response = await fetch('/api/observation', { cache: 'no-store' }); if (!response.ok) throw new Error(`HTTP ${response.status}`); render(await response.json()); }
  catch (error) { const status = byId('status'); status.textContent = 'observer offline'; status.className = 'status unhealthy'; text('observed', error.message); }
}
refresh(); setInterval(refresh, 1000);
</script>
</body>
</html>"#;

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn dashboard_is_self_contained_and_hardened() {
        let directory = tempfile::tempdir().expect("dashboard fixture");
        let (_sender, receiver) = watch::channel(ShmObserver::new(directory.path()).inspect());
        let response = dashboard_router(receiver, None)
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("dashboard response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(X_CONTENT_TYPE_OPTIONS),
            Some(&HeaderValue::from_static("nosniff"))
        );
        assert!(response.headers().contains_key(CONTENT_SECURITY_POLICY));
        assert!(response.headers().contains_key("content-type"));
        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("dashboard body");
        let html = std::str::from_utf8(&body).expect("UTF-8 dashboard");
        assert!(html.contains("SHM Observatory"));
        assert!(html.contains("/api/observation"));
        assert!(!html.contains("https://"));
    }

    #[tokio::test]
    async fn observation_endpoint_exposes_current_fail_closed_state() {
        let directory = tempfile::tempdir().expect("dashboard fixture");
        let (_sender, receiver) = watch::channel(ShmObserver::new(directory.path()).inspect());
        let response = dashboard_router(receiver, None)
            .oneshot(
                Request::builder()
                    .uri("/api/observation")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("observation response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("observation body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("observation JSON");
        assert_eq!(json["status"], "unhealthy");
        assert_eq!(json["findings"][0]["code"], "point_plane_unavailable");
    }

    #[tokio::test]
    async fn versioned_observation_path_is_not_mounted() {
        let directory = tempfile::tempdir().expect("dashboard fixture");
        let (_sender, receiver) = watch::channel(ShmObserver::new(directory.path()).inspect());
        let response = dashboard_router(receiver, None)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/observation")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("observation response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn non_loopback_bind_is_rejected_before_listening() {
        let bind = "0.0.0.0:6070".parse().expect("bind address");
        let error = serve_dashboard(ShmObserver::new("missing.shm"), None, bind)
            .await
            .expect_err("non-loopback bind must fail");

        assert!(error.to_string().contains("local-only"));
    }
}
