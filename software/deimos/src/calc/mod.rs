//! Calculations that are run at each cycle during operation.
//!
//! `Calc` objects are registered with the `CalcOrchestrator` and serialized with the controller.
//! Each calc is a function consuming any number of inputs and producing any number of outputs.
use std::any::type_name;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::iter::Iterator;

use serde::{Deserialize, Serialize};

mod orchestrator;
pub(crate) use orchestrator::CalcOrchestrator;

// Specific calc implementations

mod affine;
mod butter;
mod constant;
mod hysteretic;
mod inverse_affine;
mod pid;
mod polynomial;
mod reduction;
mod rtd_pt100;
mod sin;
mod tc_ktype;

pub mod sequence_machine;

pub use affine::Affine;
pub use butter::Butter2;
pub use constant::Constant;
pub use hysteretic::Hysteretic;
pub use inverse_affine::InverseAffine;
pub use pid::Pid;
pub use polynomial::Polynomial;
pub use reduction::{max, min};
pub use rtd_pt100::*;
pub use sequence_machine::SequenceMachine;
pub use sin::Sin;
pub use tc_ktype::*;

use crate::ControllerCtx;

// Type aliases for clarification purposes, since
// there will be a lot of strings and usize ints
pub type PeripheralName = String;
pub type PeripheralInputName = String;
pub type FieldName = String;

pub type CalcName = String;
pub type CalcInputName = String;
pub type CalcOutputName = String;

pub type SrcIndex = usize;
pub type DstIndex = usize;

/// An initialized calc evaluator with fresh mutable state for one controller run.
///
/// Inputs and outputs are ordered according to [`Calc::names`]. Evaluators run
/// synchronously in the control loop and must not allocate, block, perform I/O,
/// or panic.
pub type CalcFn = Box<dyn FnMut(&[f64], &mut [f64]) -> Result<(), String> + Send + Sync + 'static>;

/// Clone isn't inherently object-safe, so to be able to clone dyn trait objects,
/// we send it for a loop through the serde typetag system, which provides an
/// automatically-assembled vtable to determine the downcasted type and clone into it.
impl Clone for Box<dyn Calc> {
    fn clone(&self) -> Box<dyn Calc> {
        let new: Box<dyn Calc> =
            serde_json::from_str(&serde_json::to_string(&self).unwrap()).unwrap();
        new
    }
}

/// Serializable calc configuration that can create a fresh evaluator for each run.
#[typetag::serde(tag = "type")]
pub trait Calc: Send + Sync + Debug {
    /// Validate configuration and create fresh mutable state for one run.
    ///
    /// Calling `init` repeatedly must produce independent evaluators without
    /// changing this calc definition.
    fn init(&self, ctx: ControllerCtx) -> Result<CalcFn, String>;

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName>;

    /// Return input and output names in evaluation order.
    fn names(&self) -> (Vec<CalcInputName>, Vec<CalcOutputName>);

    /// Get the type name, which is guaranteed to be unique among implementations of the trait
    /// because of the use of a global vtable for serialization, and guaranteed not to include
    /// non-'static lifetimes due to trait bounds.
    fn kind(&self) -> String {
        type_name::<Self>().split(':').next_back().unwrap().into()
    }
}

/// Build `Calc::names` for calcs with statically named inputs and outputs.
#[macro_export]
macro_rules! calc_names {
    (($($input:ident),*), ($($output:ident),*)) => {
        fn names(&self) -> (Vec<CalcInputName>, Vec<CalcOutputName>) {
            (
                vec![$(stringify!($input).to_owned()),*],
                vec![$(stringify!($output).to_owned()),*],
            )
        }
    }
}
