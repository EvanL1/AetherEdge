//! numfmt Precision Tests
//!
//! Tests for ryu float formatting precision and correctness:
//! - Common industrial values (voltage, current, power)
//! - Edge cases (MAX, MIN, NaN, Inf, subnormal)
//! - Round-trip precision (f64 -> str -> f64)
//! - Precomputed pool completeness

#![allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable

use aether_rtdb::numfmt::{f64_to_bytes, i64_to_bytes, precomputed, u32_to_arc_str, u32_to_bytes};
use std::sync::Arc;

// ============================================================================
// Common Industrial Values
// ============================================================================

#[test]
fn test_ryu_common_voltage_values() {
    // Common voltage readings in industrial control
    let test_cases = [
        (220.0, "220.0"),
        (380.0, "380.0"),
        (10000.0, "10000.0"),
        (35000.0, "35000.0"),
        (110.0, "110.0"),
        (0.0, "0.0"),
        (-220.0, "-220.0"),
    ];

    for (input, expected) in test_cases {
        let bytes = f64_to_bytes(input);
        let actual = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(actual, expected, "Failed for input: {}", input);
    }
}

#[test]
fn test_ryu_common_current_values() {
    // Common current readings (often small decimals)
    let test_cases = [
        (0.001, "0.001"),
        (0.01, "0.01"),
        (0.1, "0.1"),
        (1.0, "1.0"),
        (10.5, "10.5"),
        (100.25, "100.25"),
        (1000.0, "1000.0"),
    ];

    for (input, expected) in test_cases {
        let bytes = f64_to_bytes(input);
        let actual = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(actual, expected, "Failed for input: {}", input);
    }
}

#[test]
fn test_ryu_common_power_values() {
    // Power factor, efficiency, percentage values
    let test_cases = [
        (0.95, "0.95"),
        (0.99, "0.99"),
        (1.0, "1.0"),
        (50.0, "50.0"),   // Frequency Hz
        (60.0, "60.0"),   // Frequency Hz
        (99.9, "99.9"),   // Percentage
        (100.0, "100.0"), // Percentage
    ];

    for (input, expected) in test_cases {
        let bytes = f64_to_bytes(input);
        let actual = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(actual, expected, "Failed for input: {}", input);
    }
}

#[test]
fn test_ryu_common_temperature_values() {
    // Temperature readings
    let test_cases = [
        (-40.0, "-40.0"),
        (-20.5, "-20.5"),
        (0.0, "0.0"),
        (25.0, "25.0"),
        (37.5, "37.5"),
        (85.0, "85.0"),
        (125.0, "125.0"),
    ];

    for (input, expected) in test_cases {
        let bytes = f64_to_bytes(input);
        let actual = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(actual, expected, "Failed for input: {}", input);
    }
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn test_ryu_edge_zero_values() {
    // Positive and negative zero
    let pos_zero = f64_to_bytes(0.0);
    assert_eq!(&pos_zero[..], b"0.0");

    let neg_zero = f64_to_bytes(-0.0);
    // ryu formats -0.0 as "-0.0" or "0.0" depending on version
    // Both are acceptable representations
    let neg_zero_str = std::str::from_utf8(&neg_zero).unwrap();
    assert!(neg_zero_str == "0.0" || neg_zero_str == "-0.0");
}

#[test]
fn test_ryu_edge_max_min_values() {
    // Maximum and minimum finite f64 values
    let max_bytes = f64_to_bytes(f64::MAX);
    let max_str = std::str::from_utf8(&max_bytes).unwrap();
    let parsed: f64 = max_str.parse().unwrap();
    assert_eq!(parsed, f64::MAX);

    let min_bytes = f64_to_bytes(f64::MIN);
    let min_str = std::str::from_utf8(&min_bytes).unwrap();
    let parsed: f64 = min_str.parse().unwrap();
    assert_eq!(parsed, f64::MIN);
}

#[test]
fn test_ryu_edge_epsilon_values() {
    // Very small values near epsilon
    let epsilon = f64_to_bytes(f64::EPSILON);
    let epsilon_str = std::str::from_utf8(&epsilon).unwrap();
    let parsed: f64 = epsilon_str.parse().unwrap();
    assert_eq!(parsed, f64::EPSILON);

    // Smallest positive normal value
    let min_positive = f64_to_bytes(f64::MIN_POSITIVE);
    let min_pos_str = std::str::from_utf8(&min_positive).unwrap();
    let parsed: f64 = min_pos_str.parse().unwrap();
    assert_eq!(parsed, f64::MIN_POSITIVE);
}

#[test]
fn test_ryu_edge_special_values() {
    // NaN
    let nan_bytes = f64_to_bytes(f64::NAN);
    let nan_str = std::str::from_utf8(&nan_bytes).unwrap();
    assert!(nan_str.to_lowercase() == "nan");

    // Positive infinity
    let inf_bytes = f64_to_bytes(f64::INFINITY);
    let inf_str = std::str::from_utf8(&inf_bytes).unwrap();
    assert!(inf_str.to_lowercase() == "inf" || inf_str == "Infinity");

    // Negative infinity
    let neg_inf_bytes = f64_to_bytes(f64::NEG_INFINITY);
    let neg_inf_str = std::str::from_utf8(&neg_inf_bytes).unwrap();
    assert!(neg_inf_str.to_lowercase() == "-inf" || neg_inf_str == "-Infinity");
}

#[test]
fn test_ryu_edge_scientific_notation() {
    // Very large values that require scientific notation
    let large = f64_to_bytes(1e308);
    let large_str = std::str::from_utf8(&large).unwrap();
    let parsed: f64 = large_str.parse().unwrap();
    assert!((parsed - 1e308).abs() < 1e293); // Allow small relative error

    // Very small values
    let small = f64_to_bytes(1e-308);
    let small_str = std::str::from_utf8(&small).unwrap();
    let parsed: f64 = small_str.parse().unwrap();
    assert!((parsed - 1e-308).abs() < 1e-323);
}

// ============================================================================
// Round-Trip Precision Tests
// ============================================================================

#[test]
fn test_ryu_round_trip_common_values() {
    // Test that f64 -> str -> f64 preserves value exactly
    let test_values = [
        0.0,
        1.0,
        -1.0,
        0.5,
        0.25,
        0.125,
        100.0,
        1000.0,
        0.001,
        0.0001,
        std::f64::consts::PI,
        std::f64::consts::E,
        std::f64::consts::SQRT_2,
        1.7320508075688772, // sqrt(3) - no std constant
    ];

    for &value in &test_values {
        let bytes = f64_to_bytes(value);
        let str_value = std::str::from_utf8(&bytes).unwrap();
        let parsed: f64 = str_value.parse().unwrap();
        assert_eq!(
            value, parsed,
            "Round-trip failed for {}: formatted as '{}', parsed as {}",
            value, str_value, parsed
        );
    }
}

#[test]
fn test_ryu_round_trip_industrial_values() {
    // Industrial control system typical values
    let test_values = [
        220.0,       // Voltage
        380.0,       // Voltage
        50.0,        // Frequency
        60.0,        // Frequency
        0.95,        // Power factor
        99.9,        // Percentage
        -40.0,       // Temperature
        85.0,        // Temperature
        1000.0,      // Power kW
        0.001,       // Current mA
        1234567.890, // Large meter reading
    ];

    for &value in &test_values {
        let bytes = f64_to_bytes(value);
        let str_value = std::str::from_utf8(&bytes).unwrap();
        let parsed: f64 = str_value.parse().unwrap();
        assert_eq!(
            value, parsed,
            "Round-trip failed for {}: formatted as '{}', parsed as {}",
            value, str_value, parsed
        );
    }
}

#[test]
fn test_ryu_round_trip_random_values() {
    // Test with various bit patterns
    let test_values: Vec<f64> = (0..100)
        .map(|i| {
            let bits = (i as u64) * 0x0123456789ABCDEF;
            f64::from_bits(bits % 0x7FEFFFFFFFFFFFFF) // Avoid NaN/Inf
        })
        .filter(|&v| v.is_finite())
        .collect();

    for value in test_values {
        let bytes = f64_to_bytes(value);
        let str_value = std::str::from_utf8(&bytes).unwrap();
        let parsed: f64 = str_value.parse().unwrap();
        assert_eq!(
            value, parsed,
            "Round-trip failed for {}: formatted as '{}', parsed as {}",
            value, str_value, parsed
        );
    }
}

// ============================================================================
// Precomputed Pool Tests
// ============================================================================

#[test]
fn test_precomputed_pool_0_to_255() {
    // Verify all values 0-255 are correctly precomputed
    for i in 0..256u32 {
        let arc = precomputed::get_point_id_str(i).unwrap();
        assert_eq!(&*arc, i.to_string(), "Mismatch at index {}", i);
    }
}

#[test]
fn test_precomputed_pool_identity() {
    // Same index should return the same Arc (pointer equality)
    for i in 0..256u32 {
        let arc1 = precomputed::get_point_id_str(i).unwrap();
        let arc2 = precomputed::get_point_id_str(i).unwrap();
        assert!(Arc::ptr_eq(&arc1, &arc2), "Arc not reused for index {}", i);
    }
}

#[test]
fn test_precomputed_pool_boundary() {
    // 255 should be in pool
    assert!(precomputed::get_point_id_str(255).is_some());

    // 256 should NOT be in pool
    assert!(precomputed::get_point_id_str(256).is_none());

    // Fallback allocation should work for values >= 256
    let arc = precomputed::get_point_id_str_or_alloc(256);
    assert_eq!(&*arc, "256");

    let arc = precomputed::get_point_id_str_or_alloc(1000);
    assert_eq!(&*arc, "1000");

    let arc = precomputed::get_point_id_str_or_alloc(u32::MAX);
    assert_eq!(&*arc, u32::MAX.to_string());
}

// ============================================================================
// Integer Formatting Tests
// ============================================================================

#[test]
fn test_u32_to_bytes_range() {
    let test_cases = [
        (0u32, "0"),
        (1, "1"),
        (10, "10"),
        (100, "100"),
        (1000, "1000"),
        (10000, "10000"),
        (100000, "100000"),
        (1000000, "1000000"),
        (u32::MAX, "4294967295"),
    ];

    for (input, expected) in test_cases {
        let bytes = u32_to_bytes(input);
        let actual = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(actual, expected, "Failed for input: {}", input);
    }
}

#[test]
fn test_i64_to_bytes_range() {
    let test_cases = [
        (0i64, "0"),
        (1, "1"),
        (-1, "-1"),
        (i64::MAX, "9223372036854775807"),
        (i64::MIN, "-9223372036854775808"),
        (1704067200000i64, "1704067200000"), // Typical timestamp ms
    ];

    for (input, expected) in test_cases {
        let bytes = i64_to_bytes(input);
        let actual = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(actual, expected, "Failed for input: {}", input);
    }
}

#[test]
fn test_u32_to_arc_str_values() {
    let test_cases = [0u32, 1, 42, 255, 256, 1000, u32::MAX];

    for input in test_cases {
        let arc = u32_to_arc_str(input);
        assert_eq!(&*arc, input.to_string(), "Failed for input: {}", input);
    }
}

// ============================================================================
// Consistency Tests (ryu vs std)
// ============================================================================

#[test]
fn test_ryu_consistency_with_std_simple() {
    // For simple values, ryu output should parse to the same value as std
    let test_values = [0.0, 1.0, -1.0, 10.0, 100.0, 1000.0];

    for &value in &test_values {
        let ryu_bytes = f64_to_bytes(value);
        let ryu_str = std::str::from_utf8(&ryu_bytes).unwrap();
        let ryu_parsed: f64 = ryu_str.parse().unwrap();

        let std_str = value.to_string();
        let std_parsed: f64 = std_str.parse().unwrap();

        assert_eq!(
            ryu_parsed, std_parsed,
            "Inconsistency for {}: ryu='{}' vs std='{}'",
            value, ryu_str, std_str
        );
    }
}
