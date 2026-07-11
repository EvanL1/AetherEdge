//! AetherEMS Firmware Prototype
//!
//! This is a minimal firmware example demonstrating how to use
//! aether-core and aether-shm on embedded platforms.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    MCU (Cortex-M4)                          │
//! │                                                             │
//! │  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐  │
//! │  │   UART ISR   │    │  CAN ISR     │    │ SysTick ISR  │  │
//! │  │  (DL645 Rx)  │    │ (Frame Rx)   │    │ (100ms tick) │  │
//! │  └──────┬───────┘    └──────┬───────┘    └──────┬───────┘  │
//! │         │                   │                   │          │
//! │         ▼                   ▼                   ▼          │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │              Main Loop (Polling)                    │   │
//! │  │  - Process RX buffers                               │   │
//! │  │  - Decode protocol frames                           │   │
//! │  │  - Update shared memory slots                       │   │
//! │  │  - Check for control commands                       │   │
//! │  └──────────────────────────┬──────────────────────────┘   │
//! │                              │                              │
//! │                              ▼                              │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │           Shared Memory Region (SRAM)               │   │
//! │  │           PointSlot[256] + Header                   │   │
//! │  └─────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Building
//!
//! ```bash
//! # Install target
//! rustup target add thumbv7em-none-eabihf
//!
//! # Build for STM32F4
//! cargo build --target thumbv7em-none-eabihf --release
//! ```

#![no_std]
#![no_main]

use panic_halt as _;
use cortex_m_rt::entry;

use aether_core::codec::dl645::{decode_response, Dl645Frame};
use aether_core::{PointType, Quality};
use aether_shm::{RawPtrShm, ShmOps, ShmOpsExt};

// Shared memory configuration
const MAX_SLOTS: u32 = 256;
const SHM_BASE_ADDR: usize = 0x2000_0000; // Example: Start of SRAM2 on STM32H7

// Ring buffer for UART RX
const UART_RX_BUF_SIZE: usize = 256;
static mut UART_RX_BUF: [u8; UART_RX_BUF_SIZE] = [0; UART_RX_BUF_SIZE];
static mut UART_RX_HEAD: usize = 0;
static mut UART_RX_TAIL: usize = 0;

// System tick counter (incremented by SysTick ISR)
static mut SYSTICK_MS: u64 = 0;

/// Get current timestamp in milliseconds
fn get_timestamp_ms() -> u64 {
    // In real implementation, this would read from a hardware timer
    // or use atomic operations to read SYSTICK_MS safely
    unsafe { SYSTICK_MS }
}

/// Initialize hardware peripherals
fn init_hardware() {
    // In real implementation:
    // 1. Configure system clock (e.g., 168 MHz from HSE + PLL)
    // 2. Enable peripheral clocks (USART, CAN, GPIO)
    // 3. Configure GPIO pins for UART/CAN
    // 4. Initialize UART with DL/T 645 parameters (2400/9600 baud, even parity)
    // 5. Initialize CAN bus (500 kbps, filters for device addresses)
    // 6. Configure SysTick for 1ms interrupts
    // 7. Enable NVIC interrupts

    // Placeholder: just set up basic clocks
}

/// Process received UART data (called from main loop)
fn process_uart_rx(shm: &mut RawPtrShm) {
    // Check if we have a complete DL/T 645 frame in the buffer
    let (head, tail) = unsafe { (UART_RX_HEAD, UART_RX_TAIL) };

    if head == tail {
        return; // No data
    }

    // Calculate available bytes
    let available = if head >= tail {
        head - tail
    } else {
        UART_RX_BUF_SIZE - tail + head
    };

    // Need at least 14 bytes for minimum frame
    if available < 14 {
        return;
    }

    // Copy data to local buffer for processing
    let mut frame_buf = [0u8; 256];
    let mut frame_len = 0;
    let mut idx = tail;

    // Look for frame start (0x68)
    while idx != head {
        let byte = unsafe { UART_RX_BUF[idx] };
        if byte == 0x68 {
            break;
        }
        idx = (idx + 1) % UART_RX_BUF_SIZE;
    }

    // Copy potential frame
    while idx != head && frame_len < 256 {
        frame_buf[frame_len] = unsafe { UART_RX_BUF[idx] };
        frame_len += 1;
        idx = (idx + 1) % UART_RX_BUF_SIZE;

        // Check for frame end (0x16)
        if frame_buf[frame_len - 1] == 0x16 && frame_len >= 14 {
            break;
        }
    }

    // Try to decode the frame
    if frame_len >= 14 && frame_buf[frame_len - 1] == 0x16 {
        if let Ok(frame) = decode_response(&frame_buf[..frame_len]) {
            process_dl645_frame(shm, &frame);
        }

        // Advance tail past processed frame
        unsafe {
            UART_RX_TAIL = idx;
        }
    }
}

/// Process a decoded DL/T 645 frame
fn process_dl645_frame(shm: &mut RawPtrShm, frame: &Dl645Frame) {
    let timestamp = get_timestamp_ms();

    // Map data identifier to slot index
    // In real implementation, this would use a configuration table
    let slot_index = match frame.data_id.bytes {
        [0x00, 0x01, 0x00, 0x00] => 0,  // Total active energy
        [0x02, 0x01, 0x01, 0x00] => 1,  // A-phase voltage
        [0x02, 0x02, 0x01, 0x00] => 2,  // A-phase current
        _ => return, // Unknown data identifier
    };

    // Parse the value based on data identifier
    let value = if frame.data_len >= 4 {
        // Energy: 4 bytes BCD, unit 0.01 kWh
        aether_core::codec::dl645::parse_energy(&frame.data[..frame.data_len])
    } else if frame.data_len >= 2 {
        // Voltage: 2 bytes BCD, unit 0.1 V
        aether_core::codec::dl645::parse_voltage(&frame.data[..frame.data_len])
    } else {
        0.0
    };

    // Write to shared memory
    shm.write_slot(slot_index, value, timestamp, Quality::Good as u8);
}

/// Main firmware entry point
#[entry]
fn main() -> ! {
    // Initialize hardware
    init_hardware();

    // Initialize shared memory
    let shm_ptr = SHM_BASE_ADDR as *mut u8;
    let mut shm = unsafe { RawPtrShm::from_raw(shm_ptr, MAX_SLOTS) };

    // Initialize shared memory (only on first boot)
    if !shm.is_valid() {
        shm.init();
    }

    // Pre-configure slot metadata
    shm.set_slot_metadata(0, 0, 1, PointType::Telemetry as u8); // Total energy
    shm.set_slot_metadata(1, 1, 1, PointType::Telemetry as u8); // A-phase voltage
    shm.set_slot_metadata(2, 2, 1, PointType::Telemetry as u8); // A-phase current

    // Main loop
    loop {
        // Process UART RX buffer
        process_uart_rx(&mut shm);

        // In real implementation:
        // - Check CAN RX FIFO
        // - Process control commands from shared memory
        // - Handle watchdog
        // - Enter low-power mode if idle

        // Placeholder: simulate some work
        cortex_m::asm::nop();
    }
}

// In real implementation, these would be interrupt handlers:
//
// #[interrupt]
// fn USART1() {
//     // Read byte from UART data register
//     // Push to ring buffer
// }
//
// #[interrupt]
// fn CAN1_RX0() {
//     // Read CAN frame from FIFO
//     // Push to CAN message queue
// }
//
// #[interrupt]
// fn SysTick() {
//     unsafe { SYSTICK_MS += 1; }
// }
