//! File-based channel logging handler.
//!
//! This module provides `ChannelFileLogHandler` which writes channel logs
//! to per-channel, per-day log files.
//!
//! # Directory Structure
//!
//! ```text
//! /logs/io/channels/
//! ├── PCS#1/
//! │   ├── 2025-01-22.log
//! │   └── 2025-01-21.log
//! ├── BAMS#1/
//! │   └── 2025-01-22.log
//! └── GENSET#1/
//!     └── 2025-01-22.log
//! ```
//!
//! # Log Levels
//!
//! - **Info**: Raw packets (hex format) and errors only
//! - **Debug**: Raw packets + poll cycles, state changes, control writes

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

use async_trait::async_trait;
use chrono::{Local, NaiveDate};

use super::logging::{
    ChannelLogEvent, ChannelLogHandler, ErrorContext, PacketDirection, PacketMetadata,
};

// ============================================================================
// File Log Level
// ============================================================================

/// Log level for file logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileLogLevel {
    /// Info level: raw packets and errors only.
    #[default]
    Info,
    /// Debug level: raw packets + poll cycles, state changes, control writes.
    Debug,
}

impl FileLogLevel {
    /// Parse from optional string (case-insensitive).
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(str::to_lowercase).as_deref() {
            Some("debug") => Self::Debug,
            _ => Self::Info,
        }
    }

    /// Convert to u8 for atomic storage.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Debug => 1,
        }
    }

    /// Convert from u8 (atomic load).
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Debug,
            _ => Self::Info,
        }
    }
}

// ============================================================================
// Channel File Log Handler
// ============================================================================

/// State for an open log file.
struct OpenFile {
    /// The date this file was created for.
    date: NaiveDate,
    /// Buffered writer for the file.
    writer: BufWriter<File>,
}

/// File-based channel log handler.
///
/// Writes channel logs to per-channel, per-day log files.
/// Thread-safe through internal `Mutex` on file handles.
///
/// The log level can be changed dynamically at runtime via `set_level()`.
pub struct ChannelFileLogHandler {
    /// Base directory for log files.
    base_dir: PathBuf,
    /// Mapping from channel_id to channel name.
    channel_names: HashMap<u32, String>,
    /// Open file handles: channel_id -> (date, writer).
    open_files: Mutex<HashMap<u32, OpenFile>>,
    /// Log level filter (stored as AtomicU8 for hot-reload support).
    level: AtomicU8,
}

impl ChannelFileLogHandler {
    /// Create a new file log handler with the given base directory.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let handler = ChannelFileLogHandler::new("/logs/io/channels")
    ///     .with_level(FileLogLevel::Debug)
    ///     .with_channel(1, "PCS#1");
    /// ```
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            channel_names: HashMap::new(),
            open_files: Mutex::new(HashMap::new()),
            level: AtomicU8::new(FileLogLevel::default().as_u8()),
        }
    }

    /// Set the log level (builder pattern).
    #[must_use]
    pub fn with_level(self, level: FileLogLevel) -> Self {
        self.level.store(level.as_u8(), Ordering::Relaxed);
        self
    }

    /// Get the current log level.
    #[must_use]
    pub fn level(&self) -> FileLogLevel {
        FileLogLevel::from_u8(self.level.load(Ordering::Relaxed))
    }

    /// Set the log level dynamically at runtime.
    ///
    /// This method is thread-safe and can be called while the handler is
    /// actively processing log events.
    pub fn set_level(&self, level: FileLogLevel) {
        self.level.store(level.as_u8(), Ordering::Relaxed);
    }

    /// Register a channel with its name.
    #[must_use]
    pub fn with_channel(mut self, channel_id: u32, channel_name: impl Into<String>) -> Self {
        self.channel_names.insert(channel_id, channel_name.into());
        self
    }

    /// Register a channel (mutable variant).
    pub fn add_channel(&mut self, channel_id: u32, channel_name: impl Into<String>) {
        self.channel_names.insert(channel_id, channel_name.into());
    }

    /// Sanitize channel name for use as directory name.
    /// Replaces invalid filesystem characters with underscore.
    fn sanitize_name(name: &str) -> String {
        name.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' || c == '#' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    /// Get or create the directory for a channel.
    fn get_channel_dir(&self, channel_id: u32) -> PathBuf {
        let channel_name = self
            .channel_names
            .get(&channel_id)
            .map(|s| Self::sanitize_name(s))
            .unwrap_or_else(|| format!("channel_{}", channel_id));

        self.base_dir.join(channel_name)
    }

    /// Get or create a file writer for the given channel and date.
    fn get_writer(
        &self,
        channel_id: u32,
        date: NaiveDate,
    ) -> Option<std::sync::MutexGuard<'_, HashMap<u32, OpenFile>>> {
        // Recover from poisoned mutex - the data is still valid even if a thread panicked
        let mut files = match self.open_files.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!(
                    "Ch{} recovering from poisoned mutex in file logger",
                    channel_id
                );
                poisoned.into_inner()
            },
        };

        // Check if we need to open a new file (new day or new channel or directory deleted)
        // This ensures directory is recreated if it was deleted while the channel was running
        let needs_new_file = match files.get(&channel_id) {
            Some(open) => open.date != date || !self.get_channel_dir(channel_id).exists(),
            None => true,
        };

        if needs_new_file {
            // Create channel directory if needed
            let channel_dir = self.get_channel_dir(channel_id);
            if let Err(e) = fs::create_dir_all(&channel_dir) {
                tracing::error!(
                    "Ch{} log dir create failed: {} - path: {}",
                    channel_id,
                    e,
                    channel_dir.display()
                );
                return None;
            }

            // Open log file for the date
            let file_path = channel_dir.join(format!("{}.log", date.format("%Y-%m-%d")));
            let file = match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file_path)
            {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!(
                        "Ch{} log file open failed: {} - path: {}",
                        channel_id,
                        e,
                        file_path.display()
                    );
                    return None;
                },
            };

            files.insert(
                channel_id,
                OpenFile {
                    date,
                    writer: BufWriter::new(file),
                },
            );
        }

        Some(files)
    }

    /// Format a raw packet event as log line.
    ///
    /// Output format:
    /// - TCP: `>>> modbus [TID=D596] [slave=2 fc=0x03 @100-163] [87B] D5 96 ...`
    /// - RTU: `>>> modbus [slave=2 fc=0x03 @100-163] [12B] 02 03 ...`
    fn format_raw_packet(
        &self,
        direction: &PacketDirection,
        data: &[u8],
        metadata: &PacketMetadata,
    ) -> String {
        let mut line = String::with_capacity(128);

        // Direction arrow
        let arrow = match direction {
            PacketDirection::Send => ">>>",
            PacketDirection::Receive => "<<<",
        };

        // Protocol name and metadata
        let proto_info = match metadata {
            PacketMetadata::Modbus {
                slave_id,
                function_code,
                transaction_id,
                start_address,
                quantity,
                ..
            } => {
                let mut info = String::with_capacity(64);

                // TID (TCP only)
                if let Some(tid) = transaction_id {
                    let _ = write!(info, "[TID={:04X}] ", tid);
                }

                // Base info: slave and function code
                let _ = write!(info, "[slave={} fc=0x{:02X}", slave_id, function_code);

                // Address range (if available)
                if let (Some(start), Some(qty)) = (start_address, quantity)
                    && *qty > 0
                {
                    let end = start.saturating_add(qty.saturating_sub(1));
                    let _ = write!(info, " @{}-{}", start, end);
                }

                info.push(']');
                format!("modbus {}", info)
            },
            PacketMetadata::Iec104 {
                asdu_type,
                cause_of_tx,
                common_addr,
            } => {
                format!(
                    "iec104 [type={} cot={} ca={}]",
                    asdu_type, cause_of_tx, common_addr
                )
            },
            PacketMetadata::J1939 {
                pgn,
                source,
                destination,
            } => {
                format!("j1939 [pgn={} src={} dst={}]", pgn, source, destination)
            },
            PacketMetadata::OpcUa {
                message_type,
                request_id,
            } => {
                format!("opcua [msg={} req={}]", message_type, request_id)
            },
            PacketMetadata::Gpio => "gpio".to_string(),
            PacketMetadata::Virtual => "virtual".to_string(),
            PacketMetadata::Other { protocol } => protocol.clone(),
        };

        // Format: >>> modbus [TID=D596] [slave=1 fc=0x03 @100-163] [12B] 00 01 ...
        let _ = write!(line, "{} {} [{}B] ", arrow, proto_info, data.len());

        // Append hex data
        for (i, byte) in data.iter().enumerate() {
            if i > 0 {
                line.push(' ');
            }
            let _ = write!(line, "{:02X}", byte);
        }

        line
    }

    /// Format an error event as log line.
    fn format_error(&self, error: &str, context: &ErrorContext) -> String {
        format!("[ERROR] [{}] {}", context, error)
    }

    /// Format a poll cycle event as log line (debug mode only).
    fn format_poll_cycle(
        &self,
        points_count: usize,
        success_count: usize,
        failed_count: usize,
        duration_ms: u64,
    ) -> String {
        format!(
            "[POLL] points={} ok={} fail={} ({}ms)",
            points_count, success_count, failed_count, duration_ms
        )
    }

    /// Format a state change event as log line (debug mode only).
    fn format_state_change(
        &self,
        old_state: &crate::protocols::core::ConnectionState,
        new_state: &crate::protocols::core::ConnectionState,
    ) -> String {
        format!("[STATE] {} -> {}", old_state, new_state)
    }

    /// Format a write result (control or adjustment) as log line.
    fn format_write_result(
        tag: &str,
        commands_count: usize,
        result: &Result<crate::protocols::core::WriteResult, String>,
        duration_ms: u64,
    ) -> String {
        match result {
            Ok(wr) => format!(
                "[{}] cmds={} ok ({}) ({}ms)",
                tag, commands_count, wr.success_count, duration_ms
            ),
            Err(e) => format!(
                "[{}] cmds={} FAILED: {} ({}ms)",
                tag, commands_count, e, duration_ms
            ),
        }
    }

    /// Write a log line to the channel's log file.
    ///
    /// If write fails (e.g., directory was deleted), the file handle is invalidated
    /// so the next write attempt will recreate the directory and file.
    fn write_log(&self, channel_id: u32, line: &str) {
        let now = Local::now();
        let date = now.date_naive();

        if let Some(mut files) = self.get_writer(channel_id, date)
            && let Some(open_file) = files.get_mut(&channel_id)
        {
            let timestamp = now.format("%Y-%m-%d %H:%M:%S%.3f");
            let write_result = writeln!(open_file.writer, "{} {}", timestamp, line);
            let flush_result = open_file.writer.flush();

            // If write or flush failed, invalidate cache so next call will recreate
            if write_result.is_err() || flush_result.is_err() {
                if let Err(e) = write_result {
                    tracing::warn!(
                        "Ch{} log write failed: {}, will recreate on next write",
                        channel_id,
                        e
                    );
                }
                if let Err(e) = flush_result {
                    tracing::warn!(
                        "Ch{} log flush failed: {}, will recreate on next write",
                        channel_id,
                        e
                    );
                }
                // Remove from cache to force recreation on next write
                files.remove(&channel_id);
            }
        }
    }

    /// Check if the event should be logged based on the level.
    ///
    /// Log level mapping:
    /// - **Info**: Errors, connections, disconnections, control writes, **point values** (key events)
    /// - **Debug**: All of Info + raw packets, poll cycles, state changes, reconnects
    fn should_log(&self, event: &ChannelLogEvent) -> bool {
        let level = self.level(); // Atomic read for hot-reload support
        match event {
            // Always log these at Info level (key events)
            ChannelLogEvent::Error { .. } => true,
            ChannelLogEvent::Connected { .. } => true,
            ChannelLogEvent::Disconnected { .. } => true,
            ChannelLogEvent::ControlWrite { .. } => true, // Control commands are important
            ChannelLogEvent::AdjustmentWrite { .. } => true, // Adjustment commands are important
            ChannelLogEvent::PointValues { .. } => true,  // Point values at Info level

            // Debug level only (verbose logging)
            ChannelLogEvent::RawPacket { .. } => level == FileLogLevel::Debug,
            ChannelLogEvent::PollCycleCompleted { .. } => level == FileLogLevel::Debug,
            ChannelLogEvent::StateChanged { .. } => level == FileLogLevel::Debug,
            ChannelLogEvent::ReconnectAttempt { .. } => level == FileLogLevel::Debug,
            ChannelLogEvent::ReconnectSuccess { .. } => level == FileLogLevel::Debug,
            ChannelLogEvent::ReadOperation { .. } => level == FileLogLevel::Debug,
        }
    }

    /// Format a log line with optional group ID prefix.
    ///
    /// If group_id is Some, prepends `[G001] ` to the line.
    fn format_with_group_id(&self, group_id: Option<u32>, line: &str) -> String {
        if let Some(gid) = group_id {
            format!("[G{:03}] {}", gid % 1000, line)
        } else {
            line.to_string()
        }
    }

    /// Format point values grouped by type.
    ///
    /// Returns multiple log lines, one per point type present in the values.
    /// Format: `[T] 1001:23.5, 1002:45.2` or `[S] 2001:1, 2002:0!` (! = bad quality)
    fn format_point_values_by_type(
        &self,
        values: &[super::logging::PointValueSummary],
    ) -> Vec<String> {
        use aether_model::PointType;
        use std::collections::HashMap;
        use std::fmt::Write;

        // Group by point type
        let mut by_type: HashMap<PointType, Vec<&super::logging::PointValueSummary>> =
            HashMap::new();
        for v in values {
            by_type.entry(v.point_type).or_default().push(v);
        }

        let mut lines = Vec::new();

        // Process types in consistent order: T, S, C, A
        let type_order = [
            (PointType::Telemetry, "T"),
            (PointType::Signal, "S"),
            (PointType::Control, "C"),
            (PointType::Adjustment, "A"),
        ];

        for (pt, tag) in type_order {
            if let Some(points) = by_type.get(&pt) {
                let mut line = format!("[{}] ", tag);
                for (i, v) in points.iter().enumerate() {
                    if i > 0 {
                        line.push_str(", ");
                    }
                    // Format: id:value (quality bad = !)
                    let quality_mark = if v.quality.is_good() { "" } else { "!" };
                    let _ = write!(line, "{}:{}{}", v.id, v.value, quality_mark);
                }
                lines.push(line);
            }
        }

        lines
    }
}

#[async_trait]
impl ChannelLogHandler for ChannelFileLogHandler {
    async fn on_log(&self, channel_id: u32, event: ChannelLogEvent) {
        // Level filter
        if !self.should_log(&event) {
            return;
        }

        let line = match &event {
            ChannelLogEvent::RawPacket {
                direction,
                data,
                metadata,
                group_id,
                ..
            } => {
                let packet_line = self.format_raw_packet(direction, data, metadata);
                self.format_with_group_id(*group_id, &packet_line)
            },

            ChannelLogEvent::Error { error, context, .. } => self.format_error(error, context),

            ChannelLogEvent::Connected {
                endpoint,
                duration_ms,
                ..
            } => {
                format!("[CONNECTED] {} ({}ms)", endpoint, duration_ms)
            },

            ChannelLogEvent::Disconnected { reason, .. } => {
                let reason_str = reason.as_deref().unwrap_or("intentional");
                format!("[DISCONNECTED] reason={}", reason_str)
            },

            ChannelLogEvent::PollCycleCompleted {
                points_count,
                success_count,
                failed_count,
                duration_ms,
                ..
            } => self.format_poll_cycle(*points_count, *success_count, *failed_count, *duration_ms),

            ChannelLogEvent::StateChanged {
                old_state,
                new_state,
                ..
            } => self.format_state_change(old_state, new_state),

            ChannelLogEvent::ControlWrite {
                commands,
                result,
                duration_ms,
                ..
            } => Self::format_write_result("CONTROL", commands.len(), result, *duration_ms),

            ChannelLogEvent::AdjustmentWrite {
                commands,
                result,
                duration_ms,
                ..
            } => Self::format_write_result("ADJUST", commands.len(), result, *duration_ms),

            ChannelLogEvent::ReconnectAttempt {
                attempt,
                max_attempts,
                next_retry_ms,
                ..
            } => {
                let max_str = max_attempts
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "∞".to_string());
                let retry_str = next_retry_ms
                    .map(|ms| format!(" retry in {}ms", ms))
                    .unwrap_or_default();
                format!("[RECONNECT] attempt {}/{}{}", attempt, max_str, retry_str)
            },

            ChannelLogEvent::ReconnectSuccess {
                total_attempts,
                total_duration_ms,
                ..
            } => {
                format!(
                    "[RECONNECT] SUCCESS after {} attempts ({}ms)",
                    total_attempts, total_duration_ms
                )
            },

            ChannelLogEvent::ReadOperation { .. } => {
                // Skip detailed read operations in file log
                return;
            },

            ChannelLogEvent::PointValues {
                values, group_id, ..
            } => {
                // Point values: output multiple lines, one per type
                let lines = self.format_point_values_by_type(values);
                // File I/O: avoid blocking tokio worker thread
                tokio::task::block_in_place(|| {
                    for line in lines {
                        let formatted = self.format_with_group_id(*group_id, &line);
                        self.write_log(channel_id, &formatted);
                    }
                });
                return; // Already handled, skip the generic write_log below
            },
        };

        // File I/O: avoid blocking tokio worker thread
        tokio::task::block_in_place(|| {
            self.write_log(channel_id, &line);
        });
    }

    fn set_log_level(&self, level: &str) {
        let new_level = FileLogLevel::parse(Some(level));
        self.set_level(new_level);
    }
}

impl Drop for ChannelFileLogHandler {
    fn drop(&mut self) {
        // Flush all open file writers to ensure no buffered data is lost
        // when the handler is dropped (e.g., during channel reconnection)
        if let Ok(mut files) = self.open_files.lock() {
            for (channel_id, open_file) in files.iter_mut() {
                if let Err(e) = open_file.writer.flush() {
                    // Can't use tracing in drop (might be shutting down), use eprintln
                    eprintln!(
                        "[FileLogHandler] Ch{} flush failed on drop: {}",
                        channel_id, e
                    );
                }
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    use tempfile::TempDir;

    #[test]
    fn test_sanitize_name() {
        assert_eq!(ChannelFileLogHandler::sanitize_name("PCS#1"), "PCS#1");
        assert_eq!(ChannelFileLogHandler::sanitize_name("BAMS/1"), "BAMS_1");
        assert_eq!(
            ChannelFileLogHandler::sanitize_name("test:name"),
            "test_name"
        );
        assert_eq!(ChannelFileLogHandler::sanitize_name("a b c"), "a_b_c");
    }

    #[test]
    fn test_file_log_level_from_str() {
        assert_eq!(FileLogLevel::parse(None), FileLogLevel::Info);
        assert_eq!(FileLogLevel::parse(Some("info")), FileLogLevel::Info);
        assert_eq!(FileLogLevel::parse(Some("INFO")), FileLogLevel::Info);
        assert_eq!(FileLogLevel::parse(Some("debug")), FileLogLevel::Debug);
        assert_eq!(FileLogLevel::parse(Some("DEBUG")), FileLogLevel::Debug);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_file_log_handler() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let handler = ChannelFileLogHandler::new(temp_dir.path())
            .with_level(FileLogLevel::Debug)
            .with_channel(1, "TestChannel#1");

        // Log a raw packet
        let event = ChannelLogEvent::RawPacket {
            timestamp: SystemTime::now(),
            direction: PacketDirection::Send,
            data: vec![
                0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x01, 0x03, 0x00, 0x64, 0x00, 0x0A,
            ],
            metadata: PacketMetadata::modbus_tcp(1, 0x03),
            group_id: None,
        };

        handler.on_log(1, event).await;

        // Verify file was created
        let channel_dir = temp_dir.path().join("TestChannel#1");
        assert!(channel_dir.exists());

        let today = Local::now().date_naive();
        let log_file = channel_dir.join(format!("{}.log", today.format("%Y-%m-%d")));
        assert!(log_file.exists());

        // Verify content
        let content = fs::read_to_string(&log_file).expect("Failed to read log file");
        assert!(content.contains(">>> modbus [slave=1 fc=0x03]"));
        assert!(content.contains("00 01 00 00 00 06 01 03 00 64 00 0A"));
    }

    #[test]
    fn test_dynamic_level_change() {
        let handler = ChannelFileLogHandler::new("/tmp").with_level(FileLogLevel::Info);

        // Initial level should be Info
        assert_eq!(handler.level(), FileLogLevel::Info);

        // Change to Debug dynamically
        handler.set_level(FileLogLevel::Debug);
        assert_eq!(handler.level(), FileLogLevel::Debug);

        // Change back to Info
        handler.set_level(FileLogLevel::Info);
        assert_eq!(handler.level(), FileLogLevel::Info);

        // Test via trait method (simulates API call)
        use crate::protocols::core::logging::ChannelLogHandler;
        handler.set_log_level("debug");
        assert_eq!(handler.level(), FileLogLevel::Debug);

        handler.set_log_level("info");
        assert_eq!(handler.level(), FileLogLevel::Info);

        // Invalid level should default to Info
        handler.set_log_level("invalid");
        assert_eq!(handler.level(), FileLogLevel::Info);
    }

    #[test]
    fn test_format_raw_packet() {
        let handler = ChannelFileLogHandler::new("/tmp");

        let line = handler.format_raw_packet(
            &PacketDirection::Send,
            &[0x00, 0x01, 0x00, 0x00, 0x00, 0x06],
            &PacketMetadata::modbus_tcp(1, 0x03),
        );

        assert_eq!(line, ">>> modbus [slave=1 fc=0x03] [6B] 00 01 00 00 00 06");
    }
}
