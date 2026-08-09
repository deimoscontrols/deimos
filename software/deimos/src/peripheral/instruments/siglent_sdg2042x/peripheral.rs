//! Pure controller-side SDG2042X representation and operating packet layout.

use std::collections::BTreeMap;

use byte_struct::ByteStructUnspecifiedByteOrder;
use serde::{Deserialize, Serialize};

use deimos_shared::peripherals::PeripheralId;
use deimos_shared::states::{ByteStruct, ByteStructLen, OperatingMetrics};

use super::super::SOFTWARE_MODEL_NUMBER_BASE;
use super::config::{CHANNEL_COUNT, ChannelConfig};
use crate::calc::Calc;
use crate::peripheral::Peripheral;

const CHANNEL_FIELDS: &[&str] = &[
    "enabled",
    "frequency_hz",
    "offset_voltage_v",
    "pulse_duty_cycle",
    "phase_deg",
    "stdev",
];
pub(super) const VALUES_PER_CHANNEL: usize = CHANNEL_FIELDS.len();
pub(super) const INPUT_COUNT: usize = CHANNEL_COUNT * VALUES_PER_CHANNEL;
pub(super) const OUTPUT_COUNT: usize = INPUT_COUNT;

/// Complete dynamic state for one output channel.
#[derive(ByteStruct, Clone, Copy, Debug, Default, PartialEq)]
#[byte_struct_le]
pub(super) struct ChannelState {
    pub(super) enabled: f64,
    pub(super) frequency_hz: f64,
    pub(super) offset_voltage_v: f64,
    pub(super) pulse_duty_cycle: f64,
    pub(super) phase_deg: f64,
    pub(super) stdev: f64,
}

impl ChannelState {
    fn from_values(values: &[f64]) -> Self {
        Self {
            enabled: values[0],
            frequency_hz: values[1],
            offset_voltage_v: values[2],
            pulse_duty_cycle: values[3],
            phase_deg: values[4],
            stdev: values[5],
        }
    }

    fn write_values(self, values: &mut [f64]) {
        values[0] = self.enabled;
        values[1] = self.frequency_hz;
        values[2] = self.offset_voltage_v;
        values[3] = self.pulse_duty_cycle;
        values[4] = self.phase_deg;
        values[5] = self.stdev;
    }

    fn contains_nan(self) -> bool {
        self.enabled.is_nan()
            || self.frequency_hz.is_nan()
            || self.offset_voltage_v.is_nan()
            || self.pulse_duty_cycle.is_nan()
            || self.phase_deg.is_nan()
            || self.stdev.is_nan()
    }

    /// Clamp one controller command to the configured channel envelope.
    ///
    /// A NaN in any field disables the channel rather than allowing a partial
    /// command to reach the instrument.
    fn normalized(self, config: &ChannelConfig) -> Self {
        if self.contains_nan() {
            return Self::default();
        }
        Self {
            enabled: f64::from(self.enabled >= 0.5),
            frequency_hz: self
                .frequency_hz
                .clamp(config.frequency_hz.0, config.frequency_hz.1),
            offset_voltage_v: self
                .offset_voltage_v
                .clamp(config.offset_voltage_v.0, config.offset_voltage_v.1),
            pulse_duty_cycle: self
                .pulse_duty_cycle
                .clamp(config.pulse_duty_cycle.0, config.pulse_duty_cycle.1),
            phase_deg: self.phase_deg.clamp(config.phase_deg.0, config.phase_deg.1),
            stdev: self.stdev.clamp(config.stdev.0, config.stdev.1),
        }
    }
}

/// Complete dynamic state for both output channels.
#[derive(ByteStruct, Clone, Copy, Debug, Default, PartialEq)]
#[byte_struct_le]
pub(super) struct InstrumentState {
    pub(super) ch1: ChannelState,
    pub(super) ch2: ChannelState,
}

impl InstrumentState {
    fn from_values(values: &[f64]) -> Self {
        Self {
            ch1: ChannelState::from_values(&values[..VALUES_PER_CHANNEL]),
            ch2: ChannelState::from_values(&values[VALUES_PER_CHANNEL..INPUT_COUNT]),
        }
    }

    pub(super) fn channels(self) -> [ChannelState; CHANNEL_COUNT] {
        [self.ch1, self.ch2]
    }

    fn write_values(self, values: &mut [f64]) {
        self.ch1.write_values(&mut values[..VALUES_PER_CHANNEL]);
        self.ch2
            .write_values(&mut values[VALUES_PER_CHANNEL..OUTPUT_COUNT]);
    }

    fn contains_nan(self) -> bool {
        self.ch1.contains_nan() || self.ch2.contains_nan()
    }

    pub(super) fn normalized(self, configs: &[ChannelConfig; CHANNEL_COUNT]) -> Self {
        Self {
            ch1: self.ch1.normalized(&configs[0]),
            ch2: self.ch2.normalized(&configs[1]),
        }
    }
}

/// Controller packet containing one coherent two-channel state.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
pub(super) struct OperatingInput {
    pub(super) id: u64,
    pub(super) state: InstrumentState,
}

/// Last completely applied two-channel state returned to the controller.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
pub(super) struct OperatingOutput {
    pub(super) metrics: OperatingMetrics,
    pub(super) state: InstrumentState,
}

// Request: u64 packet ID, then six little-endian f64 values per channel.
// Response: OperatingMetrics followed by both applied channel vectors.
pub(super) const INPUT_SIZE: usize = OperatingInput::BYTE_LEN;
pub(super) const OUTPUT_SIZE: usize = OperatingOutput::BYTE_LEN;

/// Software model number for the Siglent SDG2042X integration.
pub const MODEL_NUMBER: u64 = SOFTWARE_MODEL_NUMBER_BASE + 1;

/// Pure controller-side representation of an SDG2042X.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SiglentSdg2042X {
    /// Logical software serial number used in the Deimos peripheral ID.
    pub serial_number: u64,
}

impl SiglentSdg2042X {
    /// Construct the pure controller-side peripheral representation.
    ///
    /// Args:
    ///   serial_number: Logical software serial used in the peripheral ID.
    ///
    /// Returns:
    ///   A serializable peripheral with no connection or worker state.
    pub fn new(serial_number: u64) -> Self {
        Self { serial_number }
    }
}

#[typetag::serde]
impl Peripheral for SiglentSdg2042X {
    fn id(&self) -> PeripheralId {
        PeripheralId {
            model_number: MODEL_NUMBER,
            serial_number: self.serial_number,
        }
    }

    fn input_names(&self) -> Vec<String> {
        channel_names("")
    }

    fn output_names(&self) -> Vec<String> {
        channel_names("applied_")
    }

    fn operating_roundtrip_input_size(&self) -> usize {
        INPUT_SIZE
    }

    fn operating_roundtrip_output_size(&self) -> usize {
        OUTPUT_SIZE
    }

    fn emit_operating_roundtrip(
        &self,
        id: u64,
        _period_delta_ns: i64,
        _phase_delta_ns: i64,
        inputs: &[f64],
        bytes: &mut [u8],
    ) {
        OperatingInput {
            id,
            state: InstrumentState::from_values(&inputs[..INPUT_COUNT]),
        }
        .write_bytes(bytes);
    }

    fn parse_operating_roundtrip(&self, bytes: &[u8], outputs: &mut [f64]) -> OperatingMetrics {
        let response = OperatingOutput::read_bytes(bytes);
        response.state.write_values(&mut outputs[..OUTPUT_COUNT]);
        response.metrics
    }

    fn validate_operating_roundtrip(&self, bytes: &[u8]) -> bool {
        bytes.len() == OUTPUT_SIZE && !OperatingOutput::read_bytes(bytes).state.contains_nan()
    }

    fn standard_calcs(&self, _name: &str) -> BTreeMap<String, Box<dyn Calc>> {
        BTreeMap::new()
    }
}

fn channel_names(infix: &str) -> Vec<String> {
    let mut names = Vec::with_capacity(INPUT_COUNT);
    for channel in 1..=CHANNEL_COUNT {
        for field in CHANNEL_FIELDS {
            names.push(format!("ch{channel}_{infix}{field}"));
        }
    }
    names
}
