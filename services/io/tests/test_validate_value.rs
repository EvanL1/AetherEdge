//! Tests for `validate_value` / `ValidationConfig` as used by `ShmCommandListener`.
//!
//! `ShmCommandListener::handle_notification` calls `validate_value` with
//! `ValidationConfig::default()` before forwarding a command to hardware.
//! Sending NaN or Infinity to a physical device can cause device faults, so
//! these tests document and lock in the safety-critical filtering behaviour.
//!
//! `validate_value` lives in `aether_model::validation` and is re-exported at
//! the crate root, so it is fully accessible from integration tests without any
//! production-code changes.

use aether_model::{ValidationConfig, ValueValidationError, validate_value};

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

/// Return a `ValidationConfig` matching what `ShmCommandListener` uses.
fn shm_listener_config() -> ValidationConfig {
    ValidationConfig::default()
}

// ---------------------------------------------------------------------------
// Normal values
// ---------------------------------------------------------------------------

#[test]
fn test_validate_normal_values() {
    let cfg = shm_listener_config();

    let cases: &[f64] = &[
        1.0,
        -1.0,
        100.0,
        -100.0,
        3.14160,
        1_000_000.0,
        -999_999.9,
        1e10,
        -1e10,
        1e-10,
    ];

    for &v in cases {
        let result = validate_value(v, &cfg);
        assert!(
            result.is_ok(),
            "expected Ok for normal value {v}, got {result:?}"
        );
        // The value must be returned unchanged.
        assert_eq!(
            result.unwrap(),
            v,
            "validate_value must not modify valid value {v}"
        );
    }
}

// ---------------------------------------------------------------------------
// NaN rejection
// ---------------------------------------------------------------------------

#[test]
fn test_validate_rejects_nan() {
    let cfg = shm_listener_config();

    // IEEE 754 canonical NaN
    let result = validate_value(f64::NAN, &cfg);
    assert!(
        matches!(result, Err(ValueValidationError::NaN)),
        "expected Err(NaN) for f64::NAN, got {result:?}"
    );

    // NaN produced by arithmetic: f64::NAN - f64::NAN is also NaN
    let arithmetic_nan = f64::NAN - f64::NAN;
    assert!(arithmetic_nan.is_nan(), "sanity: arithmetic produces NaN");
    let result = validate_value(arithmetic_nan, &cfg);
    assert!(
        matches!(result, Err(ValueValidationError::NaN)),
        "expected Err(NaN) for arithmetic NaN, got {result:?}"
    );

    // NaN with custom bit pattern (signaling NaN)
    let snan = f64::from_bits(0x7FF0_0000_0000_0001);
    assert!(snan.is_nan(), "sanity: sNaN is NaN");
    let result = validate_value(snan, &cfg);
    assert!(
        matches!(result, Err(ValueValidationError::NaN)),
        "expected Err(NaN) for signaling NaN, got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Infinity rejection
// ---------------------------------------------------------------------------

#[test]
fn test_validate_rejects_infinity() {
    let cfg = shm_listener_config();

    // Positive infinity
    let result = validate_value(f64::INFINITY, &cfg);
    assert!(
        matches!(result, Err(ValueValidationError::Infinity)),
        "expected Err(Infinity) for f64::INFINITY, got {result:?}"
    );

    // Negative infinity
    let result = validate_value(f64::NEG_INFINITY, &cfg);
    assert!(
        matches!(result, Err(ValueValidationError::Infinity)),
        "expected Err(Infinity) for f64::NEG_INFINITY, got {result:?}"
    );

    // Infinity produced by overflow arithmetic
    let overflow_inf = f64::MAX * 2.0;
    assert!(
        overflow_inf.is_infinite(),
        "sanity: overflow produces Infinity"
    );
    let result = validate_value(overflow_inf, &cfg);
    assert!(
        matches!(result, Err(ValueValidationError::Infinity)),
        "expected Err(Infinity) for overflow infinity, got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Zero (including negative zero) must pass
// ---------------------------------------------------------------------------

#[test]
fn test_validate_accepts_zero() {
    let cfg = shm_listener_config();

    // Positive zero
    let result = validate_value(0.0_f64, &cfg);
    assert!(result.is_ok(), "0.0 must be accepted, got {result:?}");
    assert_eq!(result.unwrap(), 0.0);

    // Negative zero — IEEE 754 defines -0.0 as a distinct bit pattern but
    // equal to 0.0. It is finite and non-NaN, so validation must pass.
    let neg_zero = -0.0_f64;
    assert!(neg_zero == 0.0, "sanity: -0.0 == 0.0 in IEEE 754");
    let result = validate_value(neg_zero, &cfg);
    assert!(result.is_ok(), "-0.0 must be accepted, got {result:?}");
}

// ---------------------------------------------------------------------------
// Extreme but finite values must pass
// ---------------------------------------------------------------------------

#[test]
fn test_validate_accepts_extremes() {
    let cfg = shm_listener_config();

    // f64::MAX — largest finite positive double
    let result = validate_value(f64::MAX, &cfg);
    assert!(
        result.is_ok(),
        "f64::MAX must be accepted under default config (no range limit), got {result:?}"
    );
    assert_eq!(result.unwrap(), f64::MAX);

    // f64::MIN — most-negative finite double (≈ -1.8 × 10^308)
    let result = validate_value(f64::MIN, &cfg);
    assert!(
        result.is_ok(),
        "f64::MIN must be accepted under default config, got {result:?}"
    );
    assert_eq!(result.unwrap(), f64::MIN);

    // f64::MIN_POSITIVE — smallest positive normalised double (≈ 2.2 × 10^-308)
    let result = validate_value(f64::MIN_POSITIVE, &cfg);
    assert!(
        result.is_ok(),
        "f64::MIN_POSITIVE must be accepted, got {result:?}"
    );
    assert_eq!(result.unwrap(), f64::MIN_POSITIVE);

    // Subnormal (denormalised) value — still finite, must not be rejected
    let subnormal = f64::from_bits(1); // smallest positive subnormal
    assert!(subnormal.is_finite(), "sanity: subnormal is finite");
    let result = validate_value(subnormal, &cfg);
    assert!(
        result.is_ok(),
        "subnormal value must be accepted under default config, got {result:?}"
    );
}
