#![doc = include_str!("../README.md")]
#![no_std]
#![allow(clippy::needless_range_loop)]

pub mod peripherals;
pub mod states;
pub use states::OperatingMetrics;

/// The UDP port on which the hardware peripherals expect to receive packets from the control machine
pub const PERIPHERAL_RX_PORT: u16 = 12367;

/// The UDP port on which the control machine expects to receive packets from hardware peripherals
pub const CONTROLLER_RX_PORT: u16 = 12368;

/// Prefix length for the direct-connect static IPv4 fallback subnet.
pub const STATIC_FALLBACK_IPV4_PREFIX_LEN: u8 = 24;

/// Derive the direct-connect static IPv4 fallback address from a peripheral MAC address.
///
/// The returned address is always in `192.168.254.0/24` and avoids `.0`, `.1`, and `.255`
/// so the host can use `.1` consistently while peripherals occupy the remaining host range.
pub fn static_fallback_ipv4_from_mac(mac: [u8; 6]) -> [u8; 4] {
    let host = 2 + ((((mac[4] as u16) << 8) | mac[5] as u16) % 252) as u8;
    [192, 168, 254, host]
}

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
