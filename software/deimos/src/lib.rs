#![doc = include_str!("../README.md")]
#![allow(clippy::needless_range_loop)]

pub mod buffer_pool;
pub mod calc;
pub mod controller;
pub mod dispatcher;
pub mod logging;
pub mod math;
pub mod peripheral;
pub mod socket;

pub use controller::{
    Controller, RunHandle, Snapshot,
    context::{ControllerCtx, LoopMethod, LossOfContactPolicy, Termination},
};
pub use dispatcher::{
    ChannelFilter, CsvDispatcher, DecimationDispatcher, Dispatcher, LowPassDispatcher, Overflow,
};
#[cfg(unix)]
pub use socket::unix::UnixSocket;
pub use socket::{
    Socket, SocketAddr, SocketId, thread_channel::ThreadChannelSocket, udp::UdpSocket,
};

pub use dispatcher::DataFrameDispatcher;
pub use dispatcher::TimescaleDbDispatcher;
pub use peripheral::{HootlDriver, HootlMockupPeripheral, HootlRunHandle, MockupTransport};

#[cfg(feature = "python")]
pub mod python;

/// Generate a `pymethods` block with a provided `py_new` plus `to_json`/`from_json`.
#[macro_export]
macro_rules! py_json_methods {
    ($ty:ident, $trait:path, $( $method:item ),+ $(,)?) => {
        #[cfg(feature = "python")]
        #[pymethods]
        impl $ty {
            $(
                $method
            )+

            /// Serialize to typetagged JSON so Python can pass into trait handoff
            fn to_json(&self) -> PyResult<String> {
                let payload: &dyn $trait = self;
                serde_json::to_string(payload)
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
            }

            /// Deserialize from typetagged JSON
            #[staticmethod]
            fn from_json(s: &str) -> PyResult<Self> {
                serde_json::from_str::<Self>(s)
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
            }
        }
    };
}
