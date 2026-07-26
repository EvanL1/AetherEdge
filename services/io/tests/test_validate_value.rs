//! Command-value validation used by `ShmCommandListener`.
//!
//! The listener validates values with the domain command policy before sending
//! them to hardware. NaN and infinity must never cross that device boundary.

use aether_domain::{CommandConstraints, DomainError};

fn validate_for_shm_listener(value: f64) -> Result<f64, DomainError> {
    CommandConstraints::unbounded()
        .validate_value(value)
        .map(|()| value)
}

#[test]
fn command_listener_accepts_normal_values_without_mutation() {
    for value in [
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
    ] {
        assert_eq!(validate_for_shm_listener(value), Ok(value));
    }
}

#[test]
fn command_listener_rejects_all_nan_representations() {
    for value in [
        f64::NAN,
        f64::NAN - f64::NAN,
        f64::from_bits(0x7FF0_0000_0000_0001),
    ] {
        assert!(value.is_nan());
        assert_eq!(
            validate_for_shm_listener(value),
            Err(DomainError::NonFiniteCommandValue)
        );
    }
}

#[test]
fn command_listener_rejects_all_infinities() {
    for value in [f64::INFINITY, f64::NEG_INFINITY, f64::MAX * 2.0] {
        assert!(value.is_infinite());
        assert_eq!(
            validate_for_shm_listener(value),
            Err(DomainError::NonFiniteCommandValue)
        );
    }
}

#[test]
fn command_listener_accepts_zero_and_extreme_finite_values() {
    let subnormal = f64::from_bits(1);
    assert!(subnormal.is_finite());

    for value in [0.0, -0.0, f64::MAX, f64::MIN, f64::MIN_POSITIVE, subnormal] {
        assert_eq!(validate_for_shm_listener(value), Ok(value));
    }
}
