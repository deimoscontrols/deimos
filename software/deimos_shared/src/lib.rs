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
pub const STATIC_FALLBACK_IPV4_PREFIX_LEN: u8 = 16;

/// Number of deterministic fallback IPv4 candidates generated for each peripheral.
pub const STATIC_FALLBACK_CANDIDATE_COUNT: usize = 3;

/// Number of usable third-octet values in the fallback candidate address space.
const STATIC_FALLBACK_USABLE_THIRD_OCTETS: u32 = 254;
/// Number of usable fourth-octet values in the fallback candidate address space.
const STATIC_FALLBACK_USABLE_FOURTH_OCTETS: u32 = 253;
/// Total number of usable host slots in the fallback candidate address space.
const STATIC_FALLBACK_USABLE_HOST_COUNT: u32 =
    STATIC_FALLBACK_USABLE_THIRD_OCTETS * STATIC_FALLBACK_USABLE_FOURTH_OCTETS;
/// Fixed stride used to spread successive fallback candidates across the usable host slots.
const STATIC_FALLBACK_CANDIDATE_STRIDE: u32 = 21_419;

/// Derive one deterministic direct-connect IPv4 fallback candidate from a peripheral MAC address.
///
/// Candidates are spread over the usable `169.254.1.1` through `169.254.254.254` space
/// while avoiding `.0`, `.1`, and `.255` in the last octet. `index` is wrapped modulo
/// [`STATIC_FALLBACK_CANDIDATE_COUNT`].
pub fn static_fallback_ipv4_candidate_from_mac(mac: [u8; 6], index: usize) -> [u8; 4] {
    let seed = u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]);
    let slot = seed
        .wrapping_add((index % STATIC_FALLBACK_CANDIDATE_COUNT) as u32 * STATIC_FALLBACK_CANDIDATE_STRIDE)
        % STATIC_FALLBACK_USABLE_HOST_COUNT;
    let third = 1 + (slot / STATIC_FALLBACK_USABLE_FOURTH_OCTETS) as u8;
    let fourth = 2 + (slot % STATIC_FALLBACK_USABLE_FOURTH_OCTETS) as u8;
    [169, 254, third, fourth]
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
