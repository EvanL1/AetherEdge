//! Modbus command batching for optimized communications.
//!
//! This module provides batching functionality to group multiple Modbus write commands
//! for more efficient communication with devices. By collecting commands within a time
//! window and detecting consecutive register addresses, it can use FC16 (Write Multiple
//! Registers) instead of individual FC06 (Write Single Register) calls.
//!
//! # Design
//!
//! - Commands are grouped by `(slave_id, function_code)` key
//! - Batch window: 20ms or MAX_BATCH_SIZE commands (whichever first)
//! - FC16 optimization: consecutive addresses are merged into single request
//!
//! # Example
//!
//! ```ignore
//! use crate::protocols::adapters::command_batcher::{CommandBatcher, BatchCommand};
//! use crate::protocols::core::point::{DataFormat, ByteOrder};
//! use crate::protocols::core::data::Value;
//!
//! let mut batcher = CommandBatcher::new();
//!
//! // Add write commands
//! batcher.add_command(BatchCommand {
//!     point_id: 1,
//!     value: Value::Float(25.5),
//!     slave_id: 1,
//!     function_code: 6,
//!     register_address: 100,
//!     data_format: DataFormat::Float32,
//!     byte_order: ByteOrder::Abcd,
//! });
//!
//! // Check if batch should be executed
//! if batcher.should_execute() {
//!     let commands = batcher.take_commands();
//!     // Execute commands...
//! }
//! ```

use std::collections::HashMap;

use crate::protocols::core::data::Value;
use crate::protocols::core::point::{ByteOrder, DataFormat};

/// Batch window in milliseconds.
/// Commands within this window are collected before execution.
pub const BATCH_WINDOW_MS: u64 = 20;

/// Maximum number of commands before forcing batch execution.
pub const MAX_BATCH_SIZE: usize = 100;

/// A single write command to be batched.
#[derive(Debug, Clone)]
pub struct BatchCommand {
    /// Point ID (for tracking/logging).
    pub point_id: u32,
    /// Value to write.
    pub value: Value,
    /// Modbus slave/unit ID.
    pub slave_id: u8,
    /// Modbus function code (6 = single register, 16 = multiple registers).
    pub function_code: u8,
    /// Starting register address.
    pub register_address: u16,
    /// Data format for encoding.
    pub data_format: DataFormat,
    /// Byte order for multi-byte values.
    pub byte_order: ByteOrder,
}

/// Command batcher for optimizing Modbus write communications.
///
/// Collects write commands and groups them by (slave_id, function_code).
/// When the batch window expires or max size is reached, commands can be
/// executed with potential FC16 optimization for consecutive addresses.
#[derive(Debug)]
pub struct CommandBatcher {
    /// Pending commands grouped by (slave_id, function_code).
    pending_commands: HashMap<(u8, u8), Vec<BatchCommand>>,
    /// Last batch execution time.
    last_batch_time: tokio::time::Instant,
    /// Total pending commands count.
    total_pending: usize,
}

impl CommandBatcher {
    /// Create a new empty command batcher.
    pub fn new() -> Self {
        Self {
            pending_commands: HashMap::new(),
            last_batch_time: tokio::time::Instant::now(),
            total_pending: 0,
        }
    }

    /// Get the number of pending commands.
    pub fn pending_count(&self) -> usize {
        self.total_pending
    }

    /// Get time elapsed since last batch execution.
    pub fn elapsed_since_last_batch(&self) -> tokio::time::Duration {
        self.last_batch_time.elapsed()
    }

    /// Check if batch should be executed.
    ///
    /// Returns true if:
    /// - Time window has expired (>= BATCH_WINDOW_MS), or
    /// - Maximum batch size reached (>= MAX_BATCH_SIZE)
    pub fn should_execute(&self) -> bool {
        self.last_batch_time.elapsed().as_millis() >= BATCH_WINDOW_MS as u128
            || self.total_pending >= MAX_BATCH_SIZE
    }

    /// Take all pending commands and reset the batcher.
    ///
    /// Returns commands grouped by (slave_id, function_code).
    /// Resets the batch timer and pending count.
    pub fn take_commands(&mut self) -> HashMap<(u8, u8), Vec<BatchCommand>> {
        self.last_batch_time = tokio::time::Instant::now();
        self.total_pending = 0;
        std::mem::take(&mut self.pending_commands)
    }

    /// Add a command to the pending batch.
    ///
    /// Commands are automatically grouped by (slave_id, function_code).
    pub fn add_command(&mut self, command: BatchCommand) {
        let key = (command.slave_id, command.function_code);
        self.pending_commands.entry(key).or_default().push(command);
        self.total_pending += 1;
    }

    /// Check if commands have strictly consecutive register addresses.
    ///
    /// This is used for FC16 optimization: if registers are consecutive,
    /// multiple single writes can be combined into one multi-register write.
    ///
    /// # Arguments
    ///
    /// * `commands` - Slice of commands to check
    ///
    /// # Returns
    ///
    /// `true` if commands cover consecutive registers with no gaps,
    /// considering each command's data format size.
    pub fn are_strictly_consecutive(commands: &[BatchCommand]) -> bool {
        if commands.len() < 2 {
            return false;
        }

        // Sort indices by register address (avoid cloning BatchCommands)
        let mut indices: Vec<usize> = (0..commands.len()).collect();
        indices.sort_by_key(|&i| commands[i].register_address);

        let mut expected_addr = commands[indices[0]].register_address;

        for &i in &indices {
            let cmd = &commands[i];
            if cmd.register_address != expected_addr {
                return false; // Gap detected
            }
            // Next expected address based on data format size
            expected_addr += cmd.data_format.register_count();
        }

        true
    }
}

impl Default for CommandBatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    /// Helper function to create a test BatchCommand.
    fn create_test_command(
        point_id: u32,
        slave_id: u8,
        function_code: u8,
        register_address: u16,
        data_format: DataFormat,
    ) -> BatchCommand {
        BatchCommand {
            point_id,
            value: Value::Float(100.0),
            slave_id,
            function_code,
            register_address,
            data_format,
            byte_order: ByteOrder::Abcd,
        }
    }

    // ========== new() tests ==========

    #[test]
    fn test_new_creates_empty_batcher() {
        let batcher = CommandBatcher::new();

        assert_eq!(batcher.pending_count(), 0);
        assert!(batcher.pending_commands.is_empty());
    }

    #[test]
    fn test_default_is_equivalent_to_new() {
        let batcher1 = CommandBatcher::new();
        let batcher2 = CommandBatcher::default();

        assert_eq!(batcher1.pending_count(), batcher2.pending_count());
    }

    // ========== pending_count() tests ==========

    #[test]
    fn test_pending_count_after_add() {
        let mut batcher = CommandBatcher::new();

        batcher.add_command(create_test_command(1, 1, 6, 100, DataFormat::UInt16));
        assert_eq!(batcher.pending_count(), 1);

        batcher.add_command(create_test_command(2, 1, 6, 101, DataFormat::UInt16));
        assert_eq!(batcher.pending_count(), 2);
    }

    #[test]
    fn test_pending_count_resets_after_take() {
        let mut batcher = CommandBatcher::new();

        batcher.add_command(create_test_command(1, 1, 6, 100, DataFormat::UInt16));
        batcher.add_command(create_test_command(2, 1, 6, 101, DataFormat::UInt16));
        assert_eq!(batcher.pending_count(), 2);

        let _ = batcher.take_commands();
        assert_eq!(batcher.pending_count(), 0);
    }

    // ========== should_execute() tests ==========

    #[test]
    fn test_should_execute_false_when_empty_and_recent() {
        let batcher = CommandBatcher::new();

        // Just created, should not execute immediately
        assert!(!batcher.should_execute());
    }

    #[test]
    fn test_should_execute_true_at_max_batch_size() {
        let mut batcher = CommandBatcher::new();

        // Add MAX_BATCH_SIZE commands
        for i in 0..MAX_BATCH_SIZE {
            batcher.add_command(create_test_command(
                i as u32,
                1,
                6,
                i as u16,
                DataFormat::UInt16,
            ));
        }

        assert!(batcher.should_execute());
    }

    #[tokio::test]
    async fn test_should_execute_true_after_time_window() {
        let mut batcher = CommandBatcher::new();
        batcher.add_command(create_test_command(1, 1, 6, 100, DataFormat::UInt16));

        // Wait for batch window to expire
        tokio::time::sleep(tokio::time::Duration::from_millis(BATCH_WINDOW_MS + 5)).await;

        assert!(batcher.should_execute());
    }

    // ========== take_commands() tests ==========

    #[test]
    fn test_take_commands_returns_all_pending() {
        let mut batcher = CommandBatcher::new();

        batcher.add_command(create_test_command(1, 1, 6, 100, DataFormat::UInt16));
        batcher.add_command(create_test_command(2, 1, 6, 101, DataFormat::UInt16));
        batcher.add_command(create_test_command(3, 2, 6, 200, DataFormat::UInt16));

        let commands = batcher.take_commands();

        // Commands should be grouped by (slave_id, function_code)
        assert_eq!(commands.len(), 2); // Two groups: (1, 6) and (2, 6)
        assert_eq!(commands.get(&(1, 6)).map(|v| v.len()), Some(2));
        assert_eq!(commands.get(&(2, 6)).map(|v| v.len()), Some(1));
    }

    #[test]
    fn test_take_commands_empties_batcher() {
        let mut batcher = CommandBatcher::new();

        batcher.add_command(create_test_command(1, 1, 6, 100, DataFormat::UInt16));
        let _ = batcher.take_commands();

        assert!(batcher.pending_commands.is_empty());
        assert_eq!(batcher.pending_count(), 0);
    }

    // ========== add_command() tests ==========

    #[test]
    fn test_add_command_groups_by_slave_and_function() {
        let mut batcher = CommandBatcher::new();

        // Same slave, same function code
        batcher.add_command(create_test_command(1, 1, 6, 100, DataFormat::UInt16));
        batcher.add_command(create_test_command(2, 1, 6, 101, DataFormat::UInt16));

        // Different slave
        batcher.add_command(create_test_command(3, 2, 6, 100, DataFormat::UInt16));

        // Different function code
        batcher.add_command(create_test_command(4, 1, 16, 100, DataFormat::UInt16));

        let commands = batcher.take_commands();

        assert_eq!(commands.len(), 3); // (1,6), (2,6), (1,16)
        assert_eq!(commands.get(&(1, 6)).map(|v| v.len()), Some(2));
        assert_eq!(commands.get(&(2, 6)).map(|v| v.len()), Some(1));
        assert_eq!(commands.get(&(1, 16)).map(|v| v.len()), Some(1));
    }

    #[test]
    fn test_add_command_preserves_all_fields() {
        let mut batcher = CommandBatcher::new();

        let cmd = BatchCommand {
            point_id: 42,
            value: Value::Float(123.456),
            slave_id: 5,
            function_code: 16,
            register_address: 999,
            data_format: DataFormat::Float32,
            byte_order: ByteOrder::Cdab,
        };

        batcher.add_command(cmd);
        let commands = batcher.take_commands();

        let stored = &commands.get(&(5, 16)).unwrap()[0];
        assert_eq!(stored.point_id, 42);
        assert_eq!(stored.slave_id, 5);
        assert_eq!(stored.function_code, 16);
        assert_eq!(stored.register_address, 999);
        assert!(matches!(stored.data_format, DataFormat::Float32));
        assert!(matches!(stored.byte_order, ByteOrder::Cdab));
    }

    // ========== are_strictly_consecutive() tests ==========

    #[test]
    fn test_consecutive_single_command_returns_false() {
        let commands = vec![create_test_command(1, 1, 6, 100, DataFormat::UInt16)];
        assert!(!CommandBatcher::are_strictly_consecutive(&commands));
    }

    #[test]
    fn test_consecutive_empty_returns_false() {
        let commands: Vec<BatchCommand> = vec![];
        assert!(!CommandBatcher::are_strictly_consecutive(&commands));
    }

    #[test]
    fn test_consecutive_uint16_sequence() {
        // uint16 uses 1 register each
        let commands = vec![
            create_test_command(1, 1, 6, 100, DataFormat::UInt16),
            create_test_command(2, 1, 6, 101, DataFormat::UInt16),
            create_test_command(3, 1, 6, 102, DataFormat::UInt16),
        ];
        assert!(CommandBatcher::are_strictly_consecutive(&commands));
    }

    #[test]
    fn test_consecutive_float32_sequence() {
        // float32 uses 2 registers each
        let commands = vec![
            create_test_command(1, 1, 6, 100, DataFormat::Float32),
            create_test_command(2, 1, 6, 102, DataFormat::Float32),
            create_test_command(3, 1, 6, 104, DataFormat::Float32),
        ];
        assert!(CommandBatcher::are_strictly_consecutive(&commands));
    }

    #[test]
    fn test_consecutive_mixed_types() {
        // Mixed: uint16(1 reg) + float32(2 regs) + uint16(1 reg)
        let commands = vec![
            create_test_command(1, 1, 6, 100, DataFormat::UInt16),
            create_test_command(2, 1, 6, 101, DataFormat::Float32),
            create_test_command(3, 1, 6, 103, DataFormat::UInt16),
        ];
        assert!(CommandBatcher::are_strictly_consecutive(&commands));
    }

    #[test]
    fn test_non_consecutive_gap() {
        let commands = vec![
            create_test_command(1, 1, 6, 100, DataFormat::UInt16),
            create_test_command(2, 1, 6, 105, DataFormat::UInt16), // Gap at 101-104
        ];
        assert!(!CommandBatcher::are_strictly_consecutive(&commands));
    }

    #[test]
    fn test_consecutive_out_of_order_input() {
        // Registers should be sorted internally
        let commands = vec![
            create_test_command(3, 1, 6, 102, DataFormat::UInt16),
            create_test_command(1, 1, 6, 100, DataFormat::UInt16),
            create_test_command(2, 1, 6, 101, DataFormat::UInt16),
        ];
        assert!(CommandBatcher::are_strictly_consecutive(&commands));
    }

    // ========== elapsed_since_last_batch() tests ==========

    #[tokio::test]
    async fn test_elapsed_increases_over_time() {
        let batcher = CommandBatcher::new();
        let initial = batcher.elapsed_since_last_batch();

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let later = batcher.elapsed_since_last_batch();
        assert!(later > initial);
    }

    #[tokio::test]
    async fn test_elapsed_resets_after_take() {
        let mut batcher = CommandBatcher::new();

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let before_take = batcher.elapsed_since_last_batch();

        let _ = batcher.take_commands();
        let after_take = batcher.elapsed_since_last_batch();

        assert!(after_take < before_take);
    }

    // ========== Integration-style tests ==========

    #[test]
    fn test_batch_workflow() {
        let mut batcher = CommandBatcher::new();

        // Initially empty
        assert_eq!(batcher.pending_count(), 0);

        // Add commands
        for i in 0..5 {
            batcher.add_command(create_test_command(
                i,
                1,
                6,
                100 + i as u16,
                DataFormat::UInt16,
            ));
        }
        assert_eq!(batcher.pending_count(), 5);

        // Take and verify
        let batch = batcher.take_commands();
        assert_eq!(batch.get(&(1, 6)).unwrap().len(), 5);

        // Should be empty after take
        assert_eq!(batcher.pending_count(), 0);
        assert!(batcher.pending_commands.is_empty());
    }
}
