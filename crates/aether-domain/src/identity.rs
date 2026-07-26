//! Strongly typed identifiers used by the domain model.

use alloc::string::String;
use core::fmt;

macro_rules! numeric_id {
    ($name:ident, $inner:ty) => {
        #[doc = concat!("Strongly typed `", stringify!($name), "` value.")]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        pub struct $name($inner);

        impl $name {
            #[doc = concat!("Creates a new `", stringify!($name), "`.")]
            #[must_use]
            pub const fn new(value: $inner) -> Self {
                Self(value)
            }

            /// Returns the underlying transport value.
            #[must_use]
            pub const fn get(self) -> $inner {
                self.0
            }
        }

        impl From<$inner> for $name {
            fn from(value: $inner) -> Self {
                Self::new(value)
            }
        }

        impl From<$name> for $inner {
            fn from(value: $name) -> Self {
                value.get()
            }
        }
    };
}

numeric_id!(InstanceId, u32);
numeric_id!(ChannelId, u32);
numeric_id!(PointId, u32);
numeric_id!(RuleId, u64);
numeric_id!(AlarmRuleId, u64);
numeric_id!(AlertId, u64);
numeric_id!(CommandId, u128);
numeric_id!(TimestampMs, u64);

const MAX_INSTANCE_NAME_BYTES: usize = 64;
const FORBIDDEN_INSTANCE_NAME_CHARACTERS: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// A validated operator-visible instance identifier.
///
/// The accepted representation deliberately matches the legacy persisted/API
/// contract: up to 64 UTF-8 bytes, no control characters, and no characters
/// unsafe in file-system or shell-derived identifiers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstanceName(String);

impl InstanceName {
    /// Creates an instance name after enforcing the stable naming contract.
    pub fn new(value: impl Into<String>) -> Result<Self, InstanceNameError> {
        let value = value.into();
        Self::validate(&value)?;
        Ok(Self(value))
    }

    fn validate(value: &str) -> Result<(), InstanceNameError> {
        if value.is_empty() {
            return Err(InstanceNameError::Empty);
        }
        if value.len() > MAX_INSTANCE_NAME_BYTES {
            return Err(InstanceNameError::TooLong {
                length: value.len(),
            });
        }
        for character in value.chars() {
            if character.is_control() {
                return Err(InstanceNameError::ControlCharacter);
            }
            if FORBIDDEN_INSTANCE_NAME_CHARACTERS.contains(&character) {
                return Err(InstanceNameError::ForbiddenCharacter(character));
            }
        }
        Ok(())
    }

    /// Returns the stable string representation used at compatibility boundaries.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the value and returns its compatibility representation.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl TryFrom<&str> for InstanceName {
    type Error = InstanceNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for InstanceName {
    type Error = InstanceNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl AsRef<str> for InstanceName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<InstanceName> for String {
    fn from(value: InstanceName) -> Self {
        value.into_inner()
    }
}

impl fmt::Display for InstanceName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Failure to construct an [`InstanceName`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceNameError {
    /// The name was empty.
    Empty,
    /// The UTF-8 representation exceeded the persisted compatibility limit.
    TooLong {
        /// Number of UTF-8 bytes in the rejected value.
        length: usize,
    },
    /// The name contained a control character.
    ControlCharacter,
    /// The name contained a file-system or shell-unsafe character.
    ForbiddenCharacter(char),
}

impl fmt::Display for InstanceNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Instance name cannot be empty"),
            Self::TooLong { length } => write!(
                formatter,
                "Instance name too long ({length} characters). Maximum length is 64 characters."
            ),
            Self::ControlCharacter => {
                formatter.write_str("Instance name cannot contain control characters")
            },
            Self::ForbiddenCharacter(character) => write!(
                formatter,
                "Instance name cannot contain '{character}'. Forbidden characters: / \\ : * ? \" < > |"
            ),
        }
    }
}
