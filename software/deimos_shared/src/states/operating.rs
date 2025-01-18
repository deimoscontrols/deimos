//! Most of the `operating` frames for each peripheral are device-specific, but
//! some metrics are needed for all peripherals in order to maintain time sync.
use byte_struct::*;

/// Metrics that are needed for every peripheral device in order to
/// maintain time sync and system health.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
pub struct OperatingMetrics {
    /// Packet id
    pub id: u64,

    /// Board time at start of cycle
    pub cycle_time_ns: i64,

    /// Sub-cycle time when packet was sent
    pub sent_time_ns: i64,

    /// ID of last received input from controller
    pub last_input_id: u64,

    /// Sub-cycle time when last input from controller was received
    pub last_input_received_time_ns: i64,

    /// How much time was left in the last cycle before the start of the next
    pub cycle_time_margin_ns: i64,
}
