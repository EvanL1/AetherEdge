//! Log file rotation and compression utilities
//!
//! Background compression of old log files and cleanup of expired archives.
//! Extracted from `logging.rs` for separation of concerns.

use std::path::Path;
use std::time::{Duration, SystemTime};

use flate2::Compression;
use flate2::write::GzEncoder;

/// Spawn background log compression task.
///
/// Returns the task handle for lifecycle management by the caller.
/// Compresses `.log` files older than 1 day, deletes `.gz` files beyond retention.
pub fn spawn_compression_task(
    log_dir: std::path::PathBuf,
    service_name: String,
    max_log_files: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Initial delay to let service fully start
        tokio::time::sleep(Duration::from_secs(60)).await;

        let mut interval = tokio::time::interval(Duration::from_secs(6 * 3600));
        loop {
            interval.tick().await;
            if let Err(e) = compress_service_logs(&log_dir, &service_name, max_log_files).await {
                tracing::error!("Log compression error for {}: {}", service_name, e);
            }
        }
    })
}

/// Compress service log files and clean up channel logs.
async fn compress_service_logs(
    log_dir: &Path,
    service_name: &str,
    max_log_files: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let retention = Duration::from_secs((max_log_files as u64) * 86400);
    let service_pattern = format!("_{}", service_name);
    let api_pattern = format!("_{}_api.log", service_name);

    rotate_files_in_dir(log_dir, retention, |name| {
        let is_regular =
            name.contains(&service_pattern) && name.ends_with(".log") && !name.contains("_api");
        let is_api = name.contains(&api_pattern);
        is_regular || is_api || name.ends_with(".log.gz")
    })
    .await?;

    // Clean up channel logs (io writes per-channel daily logs here)
    let channels_dir = log_dir.join("channels");
    if channels_dir.exists() {
        cleanup_channel_logs(&channels_dir, max_log_files).await;
    }

    Ok(())
}

/// Clean up old channel log files under per-channel directories.
async fn cleanup_channel_logs(channels_dir: &Path, max_log_files: usize) {
    let retention = Duration::from_secs((max_log_files as u64) * 86400);
    let Ok(mut channel_dirs) = tokio::fs::read_dir(channels_dir).await else {
        return;
    };

    while let Ok(Some(entry)) = channel_dirs.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let _ = rotate_files_in_dir(&path, retention, |name| {
            name.ends_with(".log") || name.ends_with(".log.gz")
        })
        .await;
    }
}

/// Rotate log files in a directory: compress old `.log` files, delete expired `.gz` archives.
///
/// This unified helper replaces the previously duplicated logic in `compress_old_logs`
/// and `cleanup_channel_logs`, which both performed the same age-check-compress-or-delete pattern.
async fn rotate_files_in_dir(
    dir: &Path,
    retention: Duration,
    file_filter: impl Fn(&str) -> bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut entries = tokio::fs::read_dir(dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let file_name = match path.file_name() {
            Some(name) => name.to_string_lossy().to_string(),
            None => continue,
        };

        if !file_filter(&file_name) {
            continue;
        }

        // Use resilient pattern: skip files with metadata errors (may be deleted concurrently)
        let Ok(metadata) = tokio::fs::metadata(&path).await else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = SystemTime::now().duration_since(modified) else {
            continue;
        };

        if !file_name.ends_with(".gz") {
            // Compress logs older than 1 day
            if age > Duration::from_secs(86400) && compress_file(&path).await.is_ok() {
                let _ = tokio::fs::remove_file(&path).await;
                tracing::debug!("Compressed: {}", file_name);
            }
        } else if age > retention {
            let _ = tokio::fs::remove_file(&path).await;
            tracing::debug!("Deleted old log: {}", file_name);
        }
    }

    Ok(())
}

/// Compress a single file to `.gz` format.
async fn compress_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use tokio::io::AsyncReadExt;

    let mut input = tokio::fs::File::open(path).await?;
    let mut buffer = Vec::new();
    input.read_to_end(&mut buffer).await?;

    let output_path = format!("{}.gz", path.display());
    let output = std::fs::File::create(&output_path)?;
    let mut encoder = GzEncoder::new(output, Compression::best());
    encoder.write_all(&buffer)?;
    encoder.finish()?;

    Ok(())
}
