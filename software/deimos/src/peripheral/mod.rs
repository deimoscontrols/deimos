use std::collections::BTreeMap;

use typetag;

use core::fmt::Debug;

use super::calc::*;
use deimos_shared::{
    peripherals::{model_numbers, ModelNumber},
    states::*,
};

pub mod analog_i_rev_2;
pub use analog_i_rev_2::AnalogIRev2;

pub mod analog_i_rev_3;
pub use analog_i_rev_3::AnalogIRev3;

pub mod analog_i_rev_4;
pub use analog_i_rev_4::AnalogIRev4;

pub use deimos_shared::peripherals::PeripheralId;

/// Plugin system for handling custom device models
/// Takes the model number and serial number from a bind result
/// and initializes a Peripheral representation.
pub type PluginFn = dyn Fn(&BindingOutput) -> Box<dyn Peripheral>;

/// Map of model numbers to initialization functions so that the controller can find
/// the approriate initialization function.
pub type PluginMap<'a> = BTreeMap<ModelNumber, &'a PluginFn>;

/// Object-safe outer device trait
/// from the perspective of the controller,
/// serializable and deserializable as `Box<dyn Peripheral>`.
///
/// This is a representation from the perspective of
/// the application-side controller.
#[typetag::serde(tag = "type")]
pub trait Peripheral: Send + Sync + Debug {
    /// Unique device ID combining model number and serial number
    fn id(&self) -> PeripheralId;

    /// List of names of input fields
    fn input_names(&self) -> Vec<String>;

    /// List of names of output fields
    fn output_names(&self) -> Vec<String>;

    // fn input_units(&self) -> Vec<String>;
    // fn output_units(&self) -> Vec<String>;

    /// Byte length of packet to send to the peripheral
    fn operating_roundtrip_input_size(&self) -> usize;

    /// Byte length of packet to send to the controller
    fn operating_roundtrip_output_size(&self) -> usize;

    /// Generate bytes for a packet to send to the peripheral based on some input values
    fn emit_operating_roundtrip(
        &self,
        id: u64,
        period_delta_ns: i64,
        phase_delta_ns: i64,
        inputs: &[f64],
        bytes: &mut [u8],
    );

    /// Parse bytes of a packet sent to the controller
    fn parse_operating_roundtrip(&self, bytes: &[u8], outputs: &mut [f64]) -> OperatingMetrics;

    /// Byte length of packet to send to the peripheral
    fn configuring_input_size(&self) -> usize {
        ConfiguringInput::BYTE_LEN
    }

    /// Byte length of packet to send to the controller
    fn configuring_output_size(&self) -> usize {
        ConfiguringOutput::BYTE_LEN
    }

    /// Generate bytes for a packet to send to the peripheral based on some input values
    fn emit_configuring(
        &self,
        base_config: ConfiguringInput,
        bytes: &mut [u8],
    ) {
        let num_to_write = self.configuring_input_size();
        base_config.write_bytes(&mut bytes[..num_to_write]);
    }

    /// Parse bytes of a packet sent to the controller
    fn parse_configuring(&self, bytes: &[u8]) -> Result<(), String> {
        let resp = ConfiguringOutput::read_bytes(bytes);
        match resp.acknowledge {
            AcknowledgeConfiguration::Ack => Ok(()),
            x => Err(format!("{x:?}"))
        }
    }

    /// Get a standard set of calcs that convert the raw outputs into a useable format
    fn standard_calcs(&self, name: String) -> BTreeMap<String, Box<dyn Calc>>;
}

/// Parse a binding response to the corresponding peripheral type
/// based on its model number. If needed, a map of plugins can be provided
/// to provide initialization of custom peripherals.
pub fn parse_binding(
    msg: &BindingOutput,
    plugins: &Option<PluginMap>,
) -> Result<Box<dyn Peripheral>, String> {
    let m = msg.peripheral_id.model_number;

    // First, check if a plugin matches the input
    if let Some(plugins) = plugins {
        // We have plugins, but is this model in the map?
        if let Some(f) = plugins.get(&m) {
            return Ok(f(msg));
        }
    }

    // If we didn't find a plugin for this model, try the existing ones
    match m {
        model_numbers::ANALOG_I_REV_2_MODEL_NUMBER => Ok(Box::new(AnalogIRev2 {
            serial_number: msg.peripheral_id.serial_number,
        })),

        model_numbers::ANALOG_I_REV_3_MODEL_NUMBER => Ok(Box::new(AnalogIRev3 {
            serial_number: msg.peripheral_id.serial_number,
        })),

        model_numbers::ANALOG_I_REV_4_MODEL_NUMBER => Ok(Box::new(AnalogIRev4 {
            serial_number: msg.peripheral_id.serial_number,
        })),

        _ => Err(format!("Unrecognized model number {m}").to_owned()),
    }
}
