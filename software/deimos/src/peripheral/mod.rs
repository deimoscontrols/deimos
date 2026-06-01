//! Peripherals are timing-controlled external I/O modules, usually a DAQ.
use std::{any::type_name, collections::BTreeMap};

use core::fmt::Debug;

use super::calc::*;
use deimos_shared::{
    peripherals::{ModelNumber, model_numbers},
    states::*,
};

pub mod analog_i_rev_2;
pub use analog_i_rev_2::AnalogIRev2;

pub mod analog_i_rev_3;
pub use analog_i_rev_3::AnalogIRev3;

pub mod analog_i_rev_4;
pub use analog_i_rev_4::AnalogIRev4;

pub mod deimos_daq_rev5;
pub use deimos_daq_rev5::DeimosDaqRev5;

pub mod deimos_daq_rev6;
pub use deimos_daq_rev6::DeimosDaqRev6;

pub mod deimos_daq_rev7;
pub use deimos_daq_rev7::DeimosDaqRev7;

pub mod hootl;
pub use hootl::{HootlDriver, HootlPeripheral, HootlRunHandle, HootlTransport};

pub use deimos_shared::peripherals::PeripheralId;

/// Generate Python bindings and JSON helpers for peripherals.
#[macro_export]
macro_rules! py_peripheral_methods {
    ($ty:ident) => {
        $crate::py_json_methods!(
            $ty,
            $crate::peripheral::Peripheral,
            #[new]
            fn py_new(serial_number: u64) -> PyResult<Self> {
                Ok(Self { serial_number })
            },
            #[getter]
            fn serial_number(&self) -> u64 {
                self.serial_number
            }
        );
    };
}

/// Plugin system for handling custom device models
/// Takes the model number and serial number from a bind result
/// and initializes a Peripheral representation.
pub type PluginFn = dyn Fn(&BindingOutput) -> Box<dyn Peripheral> + Send + Sync;

/// Map of model numbers to initialization functions so that the controller can find
/// the approriate initialization function.
pub type PluginMap<'a> = BTreeMap<ModelNumber, &'a PluginFn>;

/// Clone isn't inherently object-safe, so to be able to clone dyn trait objects,
/// we send it for a loop through the serde typetag system, which provides an
/// automatically-assembled vtable to determine the downcasted type and clone into it.
impl Clone for Box<dyn Peripheral> {
    fn clone(&self) -> Box<dyn Peripheral> {
        let new: Box<dyn Peripheral> =
            serde_json::from_str(&serde_json::to_string(&self).unwrap()).unwrap();
        new
    }
}

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

    /// List of unprefixed metric channel names this peripheral publishes each cycle.
    ///
    /// Default: the canonical set of timing/health metrics every peripheral produces
    /// (sourced from `OperatingMetrics` plus controller-side bookkeeping). Override
    /// only if a peripheral diverges from this fixed set.
    ///
    /// The returned length is currently locked to
    /// `controller::peripheral_state::PERIPHERAL_METRIC_COUNT` (8). An override
    /// that returns a different length will trip the assertion in
    /// `PeripheralState::new` until `write_metric_values` is generalized to
    /// drive from this list.
    fn metric_names(&self) -> Vec<String> {
        [
            "cycle_time_ns",
            "cycle_time_margin_ns",
            "raw_timing_delta_ns",
            "filtered_timing_delta_ns",
            "requested_period_delta_ns",
            "requested_phase_delta_ns",
            "loss_of_contact_counter",
            "cycle_lag_count",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    /// Declared engineering units for each entry in `metric_names`, parallel and
    /// same length. `None` means the unit is unknown or not applicable.
    ///
    /// Default: all `None` so peripherals that have not been audited preserve
    /// today's behavior (no declared metric units). Override on a specific
    /// peripheral to publish per-channel units (see `DeimosDaqRev7`).
    ///
    /// As with `metric_names`, the length is locked to
    /// `controller::peripheral_state::PERIPHERAL_METRIC_COUNT` (8) until
    /// `write_metric_values` is generalized.
    fn metric_units(&self) -> Vec<Option<String>> {
        vec![None; self.metric_names().len()]
    }
    // Peripheral input and output channels do not yet declare units at the
    // trait surface; only metric channels do (above). Calc-graph unit checking
    // applies between calcs, and metric units flow through
    // `ControllerState::get_units_to_write`.

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
    fn emit_configuring(&self, base_config: ConfiguringInput, bytes: &mut [u8]) {
        let num_to_write = self.configuring_input_size();
        base_config.write_bytes(&mut bytes[..num_to_write]);
    }

    /// Parse bytes of a packet sent to the controller
    fn parse_configuring(&self, bytes: &[u8]) -> Result<(), String> {
        let resp = ConfiguringOutput::read_bytes(bytes);
        match resp.acknowledge {
            AcknowledgeConfiguration::Ack => Ok(()),
            x => Err(format!("{x:?}")),
        }
    }

    /// Get a standard set of calcs that convert the raw outputs into a useable format
    fn standard_calcs(&self, name: String) -> BTreeMap<String, Box<dyn Calc>>;

    /// Get the type name, which is guaranteed to be unique among implementations of the trait
    /// because of the use of a global vtable for serialization, and guaranteed not to include
    /// non-'static lifetimes due to trait bounds.
    fn kind(&self) -> String {
        type_name::<Self>().split(":").last().unwrap().into()
    }
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

        model_numbers::DEIMOS_DAQ_REV_5_MODEL_NUMBER => Ok(Box::new(DeimosDaqRev5 {
            serial_number: msg.peripheral_id.serial_number,
        })),

        model_numbers::DEIMOS_DAQ_REV_6_MODEL_NUMBER => Ok(Box::new(DeimosDaqRev6 {
            serial_number: msg.peripheral_id.serial_number,
        })),

        model_numbers::DEIMOS_DAQ_REV_7_MODEL_NUMBER => Ok(Box::new(DeimosDaqRev7 {
            serial_number: msg.peripheral_id.serial_number,
        })),

        _ => Err(format!("Unrecognized model number {m}").to_owned()),
    }
}
