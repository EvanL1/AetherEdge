//! Startup dependency checker — waits for upstream services to be healthy.

use std::time::Duration;
use tracing::{info, warn};

/// Wait for an upstream service to respond at its health endpoint.
///
/// Retries with fixed interval until timeout. Returns Ok(()) when the service
/// responds with HTTP 2xx, or Err with a descriptive message if timeout expires.
pub async fn wait_for_dependency(
    name: &str,
    health_url: &str,
    timeout: Duration,
) -> Result<(), String> {
    let interval = Duration::from_secs(2);
    let start = std::time::Instant::now();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| format!("failed to create HTTP client: {e}"))?;

    info!(
        "Waiting for {name} at {health_url} (timeout: {}s)...",
        timeout.as_secs()
    );

    loop {
        match client.get(health_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("{name} is ready ({}ms)", start.elapsed().as_millis());
                return Ok(());
            },
            Ok(resp) => {
                warn!("{name} responded with status {}", resp.status());
            },
            Err(e) => {
                warn!("{name} not ready: {e}");
            },
        }

        if start.elapsed() > timeout {
            return Err(format!(
                "{name} did not become ready within {}s at {health_url}",
                timeout.as_secs()
            ));
        }

        tokio::time::sleep(interval).await;
    }
}
