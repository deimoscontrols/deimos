pub use operating_roundtrip::*;

pub mod operating_roundtrip {
    pub use byte_struct::{ByteStruct, ByteStructLen, ByteStructUnspecifiedByteOrder};

    use crate::OperatingMetrics;

    #[derive(ByteStruct, Clone, Copy, Debug, Default)]
    #[byte_struct_le]
    pub struct OperatingRoundtripInput {
        /// Application-level packet ID
        pub id: u64,

        /// Adjustment to apply to board cycle duration to
        /// synchronize phase.
        ///
        /// This part is preserved if a packet from the controller is missed.
        ///
        /// For a PID timing controller, this would be the integral term.
        pub period_delta_ns: i64,

        /// Adjustment to apply to board cycle duration to
        /// synchronize phase.
        ///
        /// This part is applied for a single cycle, and is not
        /// preserved between cycles.
        ///
        /// For a PID timing controller, this would be the `P` and `D` terms.
        pub phase_delta_ns: i64,

        /// PWM duty cycle in range of [0, 1]
        pub pwm_duty_frac: [f32; 8],

        /// PWM frequency in Hz
        /// PWM counters are buffered, so when using PWMs as
        /// GPIO by setting duty cycle to 0%/100%, pwm
        /// frequency should be set high to produce a quick
        /// response.
        pub pwm_freq_hz: [u32; 8],
    }

    #[derive(ByteStruct, Clone, Copy, Debug, Default)]
    #[byte_struct_le]
    pub struct OperatingRoundtripOutput {
        pub metrics: OperatingMetrics,
        pub adc_voltages: [f32; 20],
    }
}
