#![doc = include_str!("../README.md")]
#![allow(clippy::needless_range_loop)]

pub mod calc;
pub mod controller;
pub mod dispatcher;
pub mod logging;
pub mod math;
pub mod peripheral;
pub mod socket;

/// Generate a `pymethods` block with a provided `py_new` plus `to_json`/`from_json`.
///
/// Usage:
/// ```
/// py_json_methods!(MyCalc,
///     #[new]
///     fn py_new(arg: f64) -> Self {
///         Self::new(arg)
///     }
/// );
/// ```
#[macro_export]
macro_rules! py_json_methods {
    ($ty:ident, $( $method:item )+ $(,)?) => {
        #[cfg(feature = "python")]
        #[pymethods]
        impl $ty {
            $(
                $method
            )+

            /// Serialize to typetagged JSON so Python can pass into add_calc
            fn to_json(&self) -> PyResult<String> {
                let calc: &dyn Calc = self;
                serde_json::to_string(calc)
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
            }

            /// Deserialize from typetagged JSON
            #[classmethod]
            fn from_json(_cls: &Bound<'_, pyo3::types::PyType>, s: &str) -> PyResult<Self> {
                serde_json::from_str::<Self>(s)
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
            }
        }
    };
}

pub use controller::{
    Controller,
    context::{ControllerCtx, LossOfContactPolicy, Termination},
};
pub use dispatcher::{
    ChannelFilter, CsvDispatcher, DecimationDispatcher, Dispatcher, LowPassDispatcher,
};
pub use socket::{Socket, SocketAddr, SocketId, udp::UdpSocket, unix::UnixSocket};

pub use dispatcher::DataFrameDispatcher;
pub use dispatcher::TimescaleDbDispatcher;

#[cfg(feature = "python")]
pub mod python;
