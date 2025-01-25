#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::needless_range_loop)]

pub mod peripherals;
pub mod states;

// Calc functions are run on the control machine, and are stored here
// because they are tightly integrated into the application-side
// definitions of the hardware peripherals and their default set of calcs.
#[cfg(feature = "std")]
pub mod calcs;

pub use states::OperatingMetrics;

/// The UDP port on which the hardware peripherals expect to receive packets from the control machine
pub const PERIPHERAL_RX_PORT: u16 = 12367;

/// The UDP port on which the control machine expects to receive packets from hardware peripherals
pub const CONTROLLER_RX_PORT: u16 = 12368;

/// Derive To/From with an added "Unknown" variant catch-all for converting
/// from numerical values that do not match a valid variant in order to
/// avoid either panicking or cumbersome error handling.
///
/// Yoinked shamelessly (with some modification) from smoltcp.
#[macro_export]
macro_rules! enum_with_unknown {
    (
        $( #[$enum_attr:meta] )*
        pub enum $name:ident($ty:ty) {
            $(
              $( #[$variant_attr:meta] )*
              $variant:ident = $value:expr
            ),+ $(,)?
        }
    ) => {
        #[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
        $( #[$enum_attr] )*
        pub enum $name {
            $(
              $( #[$variant_attr] )*
              $variant
            ),*,
            /// Catch-all for values that do not match a variant
            Unknown($ty)
        }

        impl ::core::convert::From<$ty> for $name {
            fn from(value: $ty) -> Self {
                match value {
                    $( $value => $name::$variant ),*,
                    other => $name::Unknown(other)
                }
            }
        }

        impl ::core::convert::From<$name> for $ty {
            fn from(value: $name) -> Self {
                match value {
                    $( $name::$variant => $value ),*,
                    $name::Unknown(other) => other
                }
            }
        }
    }
}
