//! Calculations that are run at each cycle during operation.
//!
//! `Calc` objects are registered with the `Orchestrator` and serialized with the controller.
//! Each calc is a function consuming any number of inputs and producing any number of outputs.
use std::iter::Iterator;
use std::{collections::BTreeMap, ops::Range};

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

mod orchestrator;
pub use orchestrator::Orchestrator;

// Specific calc implementations

mod affine;
mod constant;
mod inverse_affine;
mod pid;
mod rtd_pt100;
mod sin;
mod tc_ktype;

pub use affine::Affine;
pub use constant::Constant;
pub use inverse_affine::InverseAffine;
pub use pid::Pid;
pub use rtd_pt100::RtdPt100;
pub use sin::Sin;
pub use tc_ktype::TcKtype;

use crate::ControllerCtx;

// Type aliases for clarification purposes, since
// there will be a lot of strings and usize ints
pub type PeripheralName = String;
pub type PeripheralInputName = String;
pub type FieldName = String;

pub type CalcName = String;
pub type CalcInputName = String;
pub type CalcOutputName = String;
pub type CalcConfigName = String;

pub type SrcIndex = usize;
pub type DstIndex = usize;

/// Calcs that can be prototyped
pub trait CalcProto {
    fn prototype() -> (String, Box<dyn Calc>);
}

impl<T> CalcProto for T
where
    T: Calc + Default + 'static,
{
    fn prototype() -> (String, Box<dyn Calc>) {
        let name = std::any::type_name::<T>()
            .split("::")
            .last()
            .unwrap()
            .to_owned();
        let proto: Box<dyn Calc> = Box::new(T::default());

        (name, proto)
    }
}

/// Prototypes of each calc
pub static PROTOTYPES: Lazy<BTreeMap<String, Box<dyn Calc>>> = Lazy::new(|| {
    BTreeMap::<String, Box<dyn Calc>>::from([
        Affine::prototype(),
        Constant::prototype(),
        InverseAffine::prototype(),
        Pid::prototype(),
        RtdPt100::prototype(),
        TcKtype::prototype(),
        Sin::prototype(),
    ])
});

/// A calculation that takes some inputs and produces some outputs
/// at each timestep, and may have some persistent internal state.
#[typetag::serde(tag = "type")]
pub trait Calc: Send + Sync {
    /// Reset internal state and register calc tape indices
    fn init(&mut self, ctx: ControllerCtx, input_indices: Vec<usize>, output_range: Range<usize>);

    /// Clear state to reset for another run
    fn terminate(&mut self);

    /// Run calcs for a cycle
    fn eval(&mut self, tape: &mut [f64]);

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName>;

    /// Change a value in the input map
    fn update_input_map(&mut self, field: &str, source: &str) -> Result<(), String>;

    //
    // Everything below this point can be macro-generated

    /// Get flag for whether to save outputs
    fn get_save_outputs(&self) -> bool;

    /// Set flag for whether to save outputs
    fn set_save_outputs(&mut self, save_outputs: bool);

    /// Get config field values
    fn get_config(&self) -> BTreeMap<String, f64>;

    /// Apply config field values
    fn set_config(&mut self, cfg: &BTreeMap<String, f64>) -> Result<(), String>;

    //
    // These are needed to maintain strict ordering for indexed evaluation

    /// List of input field names in the order that they will be consumed
    fn get_input_names(&self) -> Vec<CalcInputName>;

    /// List of output field names in the order that they will be written out
    fn get_output_names(&self) -> Vec<CalcOutputName>;
}

/// Build functions for getting and setting calc config fields
#[macro_export]
macro_rules! calc_config {
    ($( $field:ident ),*) => {
        /// Get flag for whether to save outputs
        fn get_save_outputs(&self) -> bool {
            self.save_outputs
        }

        /// Set flag for whether to save outputs
        fn set_save_outputs(&mut self, save_outputs: bool) {
            self.save_outputs = save_outputs;
        }

        /// Get config field values
        fn get_config(&self) -> BTreeMap<String, f64> {
            #[allow(unused_mut)]
            let mut cfg = BTreeMap::<String, f64>::new();
            $({cfg.insert(stringify!($field).to_owned(), self.$field);})*

            cfg
        }

        /// Apply config field values
        #[allow(unused)]
        fn set_config(&mut self, cfg: &BTreeMap<String, f64>) -> Result<(), String> {
            $({
                let f = stringify!($field);
                self.$field = *cfg.get(f).ok_or(format!("Config missing key `{f}`"))?;
            })*

            Ok(())
        }
    }
}

/// Build function for getting calc input field names
#[macro_export]
macro_rules! calc_input_names {
    ($( $field:ident ),*) => {
        /// List of input field names in the order that they will be consumed
        fn get_input_names(&self) -> Vec<CalcInputName> {
            #[allow(unused_mut)]
            let mut names = vec![];
            $({
                names.push(stringify!($field).to_owned());
            })*

            names
        }
    }
}

/// Build function for getting calc output field names
#[macro_export]
macro_rules! calc_output_names {
    ($( $field:ident ),*) => {
        /// List of input field names in the order that they will be consumed
        fn get_output_names(&self) -> Vec<CalcOutputName> {
            let mut names = vec![];
            $({
                names.push(stringify!($field).to_owned());
            })*

            names
        }
    }
}
