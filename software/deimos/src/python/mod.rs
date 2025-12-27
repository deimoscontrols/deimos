use pyo3::exceptions;
use pyo3::prelude::*;
use pyo3::wrap_pymodule;

pub(crate) mod controller;
pub(crate) mod transfer; // Glue

#[pymodule]
#[pyo3(name = "deimos")]
fn deimos<'py>(_py: Python, m: &Bound<'py, PyModule>) -> PyResult<()> {
    m.add_class::<controller::Controller>()?;
    m.add_class::<crate::Overflow>()?;
    m.add_class::<controller::RunHandle>()?;
    m.add_class::<controller::Snapshot>()?;
    m.add_class::<crate::LoopMethod>()?;
    m.add_class::<crate::LossOfContactPolicy>()?;
    m.add_class::<crate::Termination>()?;

    #[pymodule]
    #[pyo3(name = "calc")]
    mod calc_ {
        #[pymodule_export]
        pub use crate::calc::{
            Affine, Butter2, Constant, InverseAffine, Pid, Polynomial, RtdPt100, SequenceMachine,
            Sin, TcKtype,
        };
    }

    m.add_wrapped(wrap_pymodule!(calc_))?;

    #[pymodule]
    #[pyo3(name = "peripheral")]
    mod peripheral_ {
        #[pymodule_export]
        pub use crate::peripheral::{
            AnalogIRev2, AnalogIRev3, AnalogIRev4, DeimosDaqRev5, DeimosDaqRev6, HootlDriver,
            HootlMockupPeripheral, MockupTransport,
        };
    }

    m.add_wrapped(wrap_pymodule!(peripheral_))?;

    #[pymodule]
    #[pyo3(name = "socket")]
    mod socket_ {
        #[pymodule_export]
        pub use crate::socket::{
            thread_channel::ThreadChannelSocket, udp::UdpSocket, unix::UnixSocket,
        };
    }

    m.add_wrapped(wrap_pymodule!(socket_))?;

    #[pymodule]
    #[pyo3(name = "dispatcher")]
    mod dispatcher_ {
        #[pymodule_export]
        pub use crate::dispatcher::{
            ChannelFilter, CsvDispatcher, DataFrameDispatcher, DecimationDispatcher,
            LatestValueDispatcher, LowPassDispatcher, TimescaleDbDispatcher,
        };
    }

    m.add_wrapped(wrap_pymodule!(dispatcher_))?;

    Ok(())
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum BackendErr {
    InvalidPathErr { msg: String },
    RunErr { msg: String },
    InvalidPeripheralErr { msg: String },
    InvalidCalcErr { msg: String },
    InvalidDispatcherErr { msg: String },
    InvalidSocketErr { msg: String },
}

impl From<BackendErr> for PyErr {
    fn from(val: BackendErr) -> Self {
        match &val {
            BackendErr::InvalidPathErr { msg: _ } => {
                exceptions::PyValueError::new_err(format!("{:#?}", val))
            }
            BackendErr::RunErr { msg: _ } => exceptions::PyIOError::new_err(format!("{:#?}", val)),
            BackendErr::InvalidPeripheralErr { msg: _ } => {
                exceptions::PyValueError::new_err(format!("{:#?}", val))
            }
            BackendErr::InvalidCalcErr { msg: _ } => {
                exceptions::PyValueError::new_err(format!("{:#?}", val))
            }
            BackendErr::InvalidDispatcherErr { msg: _ } => {
                exceptions::PyValueError::new_err(format!("{:#?}", val))
            }
            BackendErr::InvalidSocketErr { msg: _ } => {
                exceptions::PyValueError::new_err(format!("{:#?}", val))
            }
        }
    }
}
