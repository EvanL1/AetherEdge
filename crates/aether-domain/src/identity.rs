//! Strongly typed identifiers used by the domain model.

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
numeric_id!(PointId, u32);
numeric_id!(CommandId, u128);
numeric_id!(TimestampMs, u64);
