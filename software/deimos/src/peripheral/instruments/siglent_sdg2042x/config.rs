//! User-facing SDG2042X connection, waveform, and safety configuration.

use serde::{Deserialize, Serialize};

use super::super::ScpiTcpConfig;

pub(super) const CHANNEL_COUNT: usize = 2;

/// Supported SDG2042X basic waveforms.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Waveform {
    /// Sinusoidal waveform.
    Sine,
    /// Square waveform.
    Square,
    /// Ramp or sawtooth waveform.
    Ramp,
    /// Pulse waveform.
    Pulse,
    /// Gaussian noise waveform.
    Noise,
    /// Constant DC waveform.
    Dc,
}

impl Waveform {
    pub(super) fn scpi(self) -> &'static str {
        match self {
            Self::Sine => "SINE",
            Self::Square => "SQUARE",
            Self::Ramp => "RAMP",
            Self::Pulse => "PULSE",
            Self::Noise => "NOISE",
            Self::Dc => "DC",
        }
    }

    pub(super) fn uses_frequency(self) -> bool {
        !matches!(self, Self::Noise | Self::Dc)
    }

    pub(super) fn uses_amplitude(self) -> bool {
        !matches!(self, Self::Noise | Self::Dc)
    }

    pub(super) fn uses_offset(self) -> bool {
        !matches!(self, Self::Noise)
    }

    pub(super) fn uses_duty(self) -> bool {
        matches!(self, Self::Square | Self::Pulse)
    }

    pub(super) fn uses_phase(self) -> bool {
        matches!(self, Self::Sine | Self::Square | Self::Ramp)
    }
}

/// Load presented to an SDG2042X output channel.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Load {
    /// Highest supported numeric load, used to approximate a high-impedance sink.
    HighImpedance,
    /// Explicit load impedance in ohms, inclusive from 50 through 100,000.
    Ohms(u32),
}

impl Load {
    fn validate(self) -> Result<(), String> {
        match self {
            Self::HighImpedance => Ok(()),
            Self::Ohms(ohms) if (50..=100_000).contains(&ohms) => Ok(()),
            Self::Ohms(ohms) => Err(format!("load {ohms} ohms is outside 50..=100000")),
        }
    }

    pub(super) fn scpi(self) -> String {
        match self {
            // SDG2042X firmware 2.01.01.37R2 silently retains 50-ohm
            // compensation for LOAD,HZ. Its maximum numeric load applies and
            // reads back reliably, while differing from an open load by only
            // the generator's 50-ohm source impedance.
            Self::HighImpedance => "100000".to_owned(),
            Self::Ohms(ohms) => ohms.to_string(),
        }
    }
}

/// Fixed channel settings and dynamic-command clamp ranges.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ChannelConfig {
    /// Basic waveform selected whenever this channel is enabled.
    pub waveform: Waveform,
    /// Fixed peak-to-peak amplitude for sine, square, ramp, and pulse waveforms.
    pub amplitude_vpp: f64,
    /// Load used for output-level compensation.
    pub load: Load,
    /// Inclusive frequency clamp range in hertz.
    pub frequency_hz: (f64, f64),
    /// Inclusive DC offset or noise-mean clamp range in volts.
    pub offset_voltage_v: (f64, f64),
    /// Inclusive square/pulse duty-cycle clamp range as a fraction.
    pub pulse_duty_cycle: (f64, f64),
    /// Inclusive phase clamp range in degrees.
    pub phase_deg: (f64, f64),
    /// Inclusive noise standard-deviation clamp range in volts.
    pub stdev: (f64, f64),
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            waveform: Waveform::Dc,
            amplitude_vpp: 1.0,
            load: Load::HighImpedance,
            frequency_hz: (1.0e-6, 40.0e6),
            offset_voltage_v: (-10.0, 10.0),
            pulse_duty_cycle: (0.0, 1.0),
            phase_deg: (0.0, 360.0),
            stdev: (0.0, 10.0),
        }
    }
}

impl ChannelConfig {
    fn validate(&self, channel: usize) -> Result<(), String> {
        self.load
            .validate()
            .map_err(|err| format!("channel {channel}: {err}"))?;
        if self.waveform.uses_amplitude()
            && (self.amplitude_vpp.is_nan()
                || self.amplitude_vpp <= 0.0
                || self.amplitude_vpp == f64::INFINITY)
        {
            return Err(format!(
                "channel {channel}: amplitude_vpp must be finite and positive"
            ));
        }
        validate_range("frequency_hz", self.frequency_hz, channel)?;
        validate_range("offset_voltage_v", self.offset_voltage_v, channel)?;
        validate_range("pulse_duty_cycle", self.pulse_duty_cycle, channel)?;
        validate_range("phase_deg", self.phase_deg, channel)?;
        validate_range("stdev", self.stdev, channel)?;
        if self.pulse_duty_cycle.0 < 0.0 || self.pulse_duty_cycle.1 > 1.0 {
            return Err(format!(
                "channel {channel}: pulse_duty_cycle range must be within 0..=1"
            ));
        }
        Ok(())
    }
}

fn validate_range(name: &str, range: (f64, f64), channel: usize) -> Result<(), String> {
    if !range.0.is_nan()
        && !range.1.is_nan()
        && range.0 != f64::NEG_INFINITY
        && range.1 != f64::INFINITY
        && range.0 <= range.1
    {
        Ok(())
    } else {
        Err(format!(
            "channel {channel}: {name} range must contain finite ascending bounds"
        ))
    }
}

/// Connection and safety configuration for an SDG2042X driver.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Config {
    /// Shared SCPI/TCP connection, identity, and timeout settings.
    pub connection: ScpiTcpConfig,
    /// Fixed settings and accepted dynamic ranges for channels 1 and 2.
    pub channels: [ChannelConfig; CHANNEL_COUNT],
}

impl Config {
    /// Build a configuration from a host name or address, adding SCPI port 5025.
    ///
    /// Args:
    ///   host: Host name, IP address, or address with an explicit port.
    ///   serial_number: Logical software serial used in the peripheral ID.
    ///
    /// Returns:
    ///   A configuration with both channels set to DC, high-impedance load
    ///   compensation, conservative timeouts, and output disabled at startup.
    pub fn new(host: impl Into<String>, serial_number: u64) -> Self {
        Self {
            connection: ScpiTcpConfig::new(host, serial_number, "SIGLENT", "SDG2042X"),
            channels: std::array::from_fn(|_| ChannelConfig::default()),
        }
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        self.connection.validate()?;
        for (index, channel) in self.channels.iter().enumerate() {
            channel.validate(index + 1)?;
        }
        Ok(())
    }
}
