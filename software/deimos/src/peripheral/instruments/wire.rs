//! Shared declarations for instrument operating fields and packet layouts.

/// Declare a stable list of controller field names.
macro_rules! instrument_fields {
    ($name:ident = [$($field:ident),+ $(,)?]) => {
        const $name: &[&str] = &[$(stringify!($field)),+];
    };
}

/// Declare a copyable dynamic-value record and its controller field names.
macro_rules! instrument_value_fields {
    (
        $record:ident, $names:ident, $count:ident {
            $($field:ident: $field_type:ty => $encode:path),+ $(,)?
        }
    ) => {
        const $names: &[&str] = &[$(stringify!($field)),+];
        const $count: usize = $names.len();

        #[derive(Clone, Copy, Debug, Default, PartialEq)]
        struct $record {
            $($field: $field_type),+
        }

        impl $record {
            fn values(self) -> [f64; $count] {
                [$($encode(self.$field)),+]
            }
        }
    };
}

/// Declare fixed-width little-endian operating request and response packets.
macro_rules! operating_packets {
    ($input:ident, $output:ident, $input_count:expr, $output_count:expr) => {
        #[derive(deimos_shared::states::ByteStruct, Clone, Copy, Debug, Default)]
        #[byte_struct_le]
        struct $input {
            id: u64,
            values: [f64; $input_count],
        }

        #[derive(deimos_shared::states::ByteStruct, Clone, Copy, Debug, Default)]
        #[byte_struct_le]
        struct $output {
            metrics: deimos_shared::states::OperatingMetrics,
            values: [f64; $output_count],
        }
    };
}

pub(crate) use instrument_fields;
pub(crate) use instrument_value_fields;
pub(crate) use operating_packets;
