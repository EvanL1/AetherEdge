//! Configuration loading helper functions
//! Provides utilities for loading configuration with fallback logic

use std::fmt::Display;
use std::str::FromStr;
use tracing::{debug, warn};

/// Get configuration value with priority: DB > ENV > Default
///
/// # Arguments
/// * `db_value` - Value from database configuration
/// * `is_default` - Whether the DB value is a default value
/// * `env_var` - Environment variable name to check
/// * `default` - Default value to use as fallback
pub fn get_config_value<T>(db_value: Option<T>, is_default: bool, env_var: &str, default: T) -> T
where
    T: FromStr + PartialEq + Clone,
    T::Err: Display,
{
    // Priority 1: DB value (if not default)
    if let Some(val) = db_value
        && !is_default
    {
        debug!("{} from DB", env_var);
        return val;
    }

    // Priority 2: Environment variable
    if let Ok(env_str) = std::env::var(env_var) {
        match env_str.parse::<T>() {
            Ok(val) => {
                debug!("{} from env: {}", env_var, env_str);
                return val;
            },
            Err(e) => {
                warn!("Parse {} env: {}", env_var, e);
            },
        }
    }

    // Priority 3: Default value
    debug!("{} default", env_var);
    default
}

/// Get string configuration value with priority: DB > ENV > Default
pub fn get_string_config(
    db_value: Option<String>,
    is_default: bool,
    env_var: &str,
    default: String,
) -> String {
    // Priority 1: DB value (if not empty and not default)
    if let Some(val) = db_value
        && !val.is_empty()
        && !is_default
    {
        debug!("{} from DB", env_var);
        return val;
    }

    // Priority 2: Environment variable
    if let Ok(env_val) = std::env::var(env_var)
        && !env_val.is_empty()
    {
        debug!("{} from env", env_var);
        return env_val;
    }

    // Priority 3: Default value
    debug!("{} default", env_var);
    default
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;

    #[test]
    fn test_get_config_value_priority() {
        // Test DB priority
        let val = get_config_value(Some(8080u16), false, "TEST_PORT", 3000);
        assert_eq!(val, 8080);

        // Test default when DB is default value
        let val = get_config_value(Some(3000u16), true, "TEST_PORT", 3000);
        assert_eq!(val, 3000);
    }
}
