use std::sync::{Arc, atomic::AtomicBool};

// use numpy::borrow::{PyReadonlyArray1, PyReadwriteArray1};
use pyo3::exceptions;
use pyo3::prelude::*;
use std::collections::HashMap;

use pyo3::wrap_pymodule;

// Dispatchers
use crate::Socket;
use crate::calc::Calc;
use crate::dispatcher::Dispatcher;
use crate::peripheral::Peripheral;

use crate::dispatcher::LatestValueDispatcher;
use crate::dispatcher::LatestValueHandle;

pub use crate::dispatcher::Overflow;
mod transfer; // Glue

#[pymodule]
#[pyo3(name = "deimos")]
fn deimos<'py>(_py: Python, m: &Bound<'py, PyModule>) -> PyResult<()> {
    m.add_class::<Controller>()?;
    m.add_class::<Overflow>()?;
    m.add_class::<RunHandle>()?;
    m.add_class::<Snapshot>()?;

    #[pymodule]
    #[pyo3(name = "calc")]
    mod calc_ {

        #[pymodule_export]
        pub use crate::calc::{
            Affine, Butter2, Constant, InverseAffine, Pid, Polynomial, RtdPt100,
            Sin, TcKtype,
            sequence_machine::{SequenceLookup, ThreshOp, Timeout, Transition},
        };
    }

    m.add_wrapped(wrap_pymodule!(calc_))?;

    #[pymodule]
    #[pyo3(name = "peripheral")]
    mod peripheral_ {
        #[pymodule_export]
        pub use crate::peripheral::{
            AnalogIRev2, AnalogIRev3, AnalogIRev4, DeimosDaqRev5, DeimosDaqRev6,
            HootlMockupPeripheral, HootlDriver, MockupTransport,
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

    #[pymodule]
    #[pyo3(name = "context")]
    mod context_ {
        #[pymodule_export]
        pub use crate::controller::context::{LossOfContactPolicy, Termination};
    }

    m.add_wrapped(wrap_pymodule!(context_))?;

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

#[pyclass]
pub(crate) struct Controller {
    controller: Option<crate::Controller>,
}

impl Controller {
    fn ctrl(&mut self) -> PyResult<&mut crate::Controller> {
        self.controller.as_mut().ok_or_else(|| {
            BackendErr::RunErr {
                msg: "Controller has been moved into a running thread".to_string(),
            }
            .into()
        })
    }

    pub(crate) fn ctx(&self) -> PyResult<&crate::ControllerCtx> {
        self.controller
            .as_ref()
            .ok_or_else(|| {
                BackendErr::RunErr {
                    msg: "Controller has been moved into a running thread".to_string(),
                }
                .into()
            })
            .map(|c| &c.ctx)
    }

    fn ctx_mut(&mut self) -> PyResult<&mut crate::ControllerCtx> {
        self.controller
            .as_mut()
            .ok_or_else(|| {
                BackendErr::RunErr {
                    msg: "Controller has been moved into a running thread".to_string(),
                }
                .into()
            })
            .map(|c| &mut c.ctx)
    }
}

#[pymethods]
impl Controller {
    /// Build a new controller.
    /// `rate_hz` will be rounded to the nearest nanosecond when converted to the sample period.
    ///
    /// This constructor does not run the controller or attach any peripherals.
    #[new]
    fn new(op_name: &str, op_dir: &str, rate_hz: f64) -> PyResult<Self> {
        let mut ctx = crate::ControllerCtx::default();
        ctx.op_name = op_name.into();
        ctx.op_dir = op_dir.to_string().into();
        ctx.dt_ns = (1e9 / rate_hz) as u32;

        let controller = crate::Controller::new(ctx);

        Ok(Self {
            controller: Some(controller),
        })
    }

    /// Run the control program
    fn run(&mut self, py: Python<'_>) -> PyResult<String> {
        // Shared signal indicating whether the controller should exit
        let termination_signal = Arc::new(AtomicBool::new(false));

        std::thread::scope(|s| {
            let ctrl = self.ctrl()?;
            let term_for_thread = termination_signal.clone();
            let handle = s.spawn(move || ctrl.run(&None, Some(&*term_for_thread)));

            // Wait without using too much processor time
            while !handle.is_finished() {
                // Check for keyboard interrupt
                if py.check_signals().is_err() {
                    termination_signal.store(true, std::sync::atomic::Ordering::Relaxed);
                };

                // Yield thread to minimize processor usage
                std::thread::yield_now();
            }

            // The operation is done; check the result
            match handle.join().map_err(|e| BackendErr::RunErr {
                msg: format!("Controller operation failed to complete: {e:?}"),
            })? {
                Ok(msg) => Ok(msg),
                Err(msg) => Err(BackendErr::RunErr { msg }.into()),
            }
        })
    }

    /// Run the control program on a separate thread and return a handle for coordination.
    fn run_nonblocking(&mut self) -> PyResult<RunHandle> {
        let mut controller = self.controller.take().ok_or_else(|| BackendErr::RunErr {
            msg: "Controller has already been moved into a running thread".to_string(),
        })?;

        // Attach a latest-value dispatcher to expose live data
        let (latest_dispatcher, latest_handle) = LatestValueDispatcher::new();
        controller.add_dispatcher(Box::new(latest_dispatcher));

        let termination_signal = Arc::new(AtomicBool::new(false));
        let term_for_thread = termination_signal.clone();
        let handle = std::thread::spawn(move || controller.run(&None, Some(&*term_for_thread)));

        Ok(RunHandle {
            termination: termination_signal,
            latest: latest_handle,
            join: Some(handle),
        })
    }

    /// Scan the local network (and any other attached sockets) for available peripherals.
    #[pyo3(signature=(timeout_ms=10))]
    fn scan(&mut self, py: Python, timeout_ms: u16) -> PyResult<Vec<Py<PyAny>>> {
        match self.ctrl()?.scan(timeout_ms, &None) {
            Err(msg) => Err(BackendErr::RunErr { msg }.into()),
            Ok(map) => {
                let module = py
                    .import("deimos")
                    .and_then(|root| root.getattr("peripheral"))
                    .map_err(|e| BackendErr::InvalidPeripheralErr {
                        msg: format!("Failed to resolve peripheral module: {e}"),
                    })?;
                let mut peripherals = Vec::with_capacity(map.len());

                for peripheral in map.values() {
                    let kind = peripheral.kind();
                    let json = serde_json::to_string(peripheral).map_err(|e| {
                        BackendErr::InvalidPeripheralErr {
                            msg: format!("Failed to serialize {kind}: {e}"),
                        }
                    })?;
                    let cls = module.getattr(kind.as_str()).map_err(|e| {
                        BackendErr::InvalidPeripheralErr {
                            msg: format!("Missing Python class for {kind}: {e}"),
                        }
                    })?;
                    let obj = cls.call_method1("from_json", (json,)).map_err(|e| {
                        BackendErr::InvalidPeripheralErr {
                            msg: format!("Failed to construct {kind} from JSON: {e}"),
                        }
                    })?;
                    peripherals.push(obj.unbind());
                }

                Ok(peripherals)
            }
        }
    }

    #[getter(op_name)]
    fn op_name(&mut self) -> PyResult<String> {
        Ok(self.ctx()?.op_name.clone())
    }

    #[setter(op_name)]
    fn set_op_name(&mut self, v: &str) -> PyResult<()> {
        self.ctx_mut()?.op_name = v.to_string();
        Ok(())
    }

    #[getter(op_dir)]
    fn op_dir(&self) -> PyResult<String> {
        if let Some(x) = self.ctx()?.op_dir.to_str() {
            Ok(x.to_string())
        } else {
            let invalid_string = self.ctx()?.op_dir.to_string_lossy();
            Err(BackendErr::InvalidPathErr {
                msg: format!(
                    "Unable to convert op_dir to valid string: {}",
                    invalid_string
                ),
            }
            .into())
        }
    }

    #[setter(op_dir)]
    fn set_op_dir(&mut self, v: &str) -> PyResult<()> {
        self.ctx_mut()?.op_dir = v.to_string().into();
        Ok(())
    }

    #[getter(dt_ns)]
    fn dt_ns(&self) -> PyResult<u32> {
        Ok(self.ctx()?.dt_ns)
    }

    #[setter(dt_ns)]
    fn set_dt_ns(&mut self, v: u32) -> PyResult<()> {
        self.ctx_mut()?.dt_ns = v;
        Ok(())
    }

    #[getter(rate_hz)]
    fn rate_hz(&self) -> PyResult<f64> {
        Ok(1e9 / self.ctx()?.dt_ns as f64)
    }

    #[setter(rate_hz)]
    fn set_rate_hz(&mut self, v: f64) -> PyResult<()> {
        self.ctx_mut()?.dt_ns = (1e9 / v) as u32;
        Ok(())
    }

    #[getter(peripheral_loss_of_contact_limit)]
    fn peripheral_loss_of_contact_limit(&self) -> PyResult<u16> {
        Ok(self.ctx()?.peripheral_loss_of_contact_limit)
    }

    #[setter(peripheral_loss_of_contact_limit)]
    fn set_peripheral_loss_of_contact_limit(&mut self, v: u16) -> PyResult<()> {
        self.ctx_mut()?.peripheral_loss_of_contact_limit = v;
        Ok(())
    }

    #[getter(controller_loss_of_contact_limit)]
    fn controller_loss_of_contact_limit(&self) -> PyResult<u16> {
        Ok(self.ctx()?.controller_loss_of_contact_limit)
    }

    #[setter(controller_loss_of_contact_limit)]
    fn set_controller_loss_of_contact_limit(&mut self, v: u16) -> PyResult<()> {
        self.ctx_mut()?.controller_loss_of_contact_limit = v;
        Ok(())
    }

    #[getter(termination_criteria)]
    fn termination_criteria(&self, py: Python<'_>) -> PyResult<Vec<Py<crate::Termination>>> {
        self.ctx()?
            .termination_criteria
            .iter()
            .cloned()
            .map(|term| Py::new(py, term))
            .collect()
    }

    #[setter(termination_criteria)]
    fn set_termination_criteria(
        &mut self,
        py: Python<'_>,
        v: Vec<Py<crate::Termination>>,
    ) -> PyResult<()> {
        let criteria = v
            .into_iter()
            .map(|term| term.borrow(py).clone())
            .collect();
        self.ctx_mut()?.termination_criteria = criteria;
        Ok(())
    }

    #[getter(loss_of_contact_policy)]
    fn loss_of_contact_policy(
        &self,
        py: Python<'_>,
    ) -> PyResult<Py<crate::LossOfContactPolicy>> {
        Py::new(py, self.ctx()?.loss_of_contact_policy.clone())
    }

    #[setter(loss_of_contact_policy)]
    fn set_loss_of_contact_policy(
        &mut self,
        py: Python<'_>,
        v: Py<crate::LossOfContactPolicy>,
    ) -> PyResult<()> {
        self.ctx_mut()?.loss_of_contact_policy = v.borrow(py).clone();
        Ok(())
    }

    fn add_peripheral(&mut self, name: &str, p: Box<dyn Peripheral>) -> PyResult<()> {
        self.ctrl()?.add_peripheral(name, p);
        Ok(())
    }

    fn add_calc(&mut self, name: &str, calc: Box<dyn Calc>) -> PyResult<()> {
        self.ctrl()?.add_calc(name, calc);
        Ok(())
    }

    /// Add a dispatcher via a JSON-serializable dispatcher instance.
    fn add_dispatcher(&mut self, dispatcher: Box<dyn Dispatcher>) -> PyResult<()> {
        self.ctrl()?.add_dispatcher(dispatcher);
        Ok(())
    }

    /// Add a socket via a JSON-serializable socket instance.
    fn add_socket(&mut self, socket: Box<dyn Socket>) -> PyResult<()> {
        self.ctrl()?.add_socket(socket);
        Ok(())
    }

    /// Remove all calcs.
    fn clear_calcs(&mut self) -> PyResult<()> {
        self.ctrl()?.clear_calcs();
        Ok(())
    }

    /// Remove all peripherals.
    fn clear_peripherals(&mut self) -> PyResult<()> {
        self.ctrl()?.clear_peripherals();
        Ok(())
    }

    /// Remove all dispatchers.
    fn clear_dispatchers(&mut self) -> PyResult<()> {
        self.ctrl()?.clear_dispatchers();
        Ok(())
    }

    /// Remove all sockets.
    fn clear_sockets(&mut self) -> PyResult<()> {
        self.ctrl()?.clear_sockets();
        Ok(())
    }
}

#[pyclass]
struct RunHandle {
    termination: Arc<AtomicBool>,
    latest: LatestValueHandle,
    join: Option<std::thread::JoinHandle<Result<String, String>>>,
}

#[pymethods]
impl RunHandle {
    /// Signal the controller to stop
    fn stop(&self) {
        self.termination
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Check if the controller thread is still running
    fn is_running(&self) -> bool {
        self.join
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Wait for the controller thread to finish
    fn join(&mut self) -> PyResult<String> {
        match self.join.take() {
            Some(h) => match h.join() {
                Ok(Ok(msg)) => Ok(msg),
                Ok(Err(e)) => Err(BackendErr::RunErr { msg: e }.into()),
                Err(e) => Err(BackendErr::RunErr {
                    msg: format!("Controller thread panicked: {e:?}"),
                }
                .into()),
            },
            None => Err(BackendErr::RunErr {
                msg: "Controller thread already joined or not started".to_string(),
            }
            .into()),
        }
    }

    /// Get the latest row: (system_time, timestamp, channel_values)
    fn latest_row(&self) -> (String, i64, Vec<f64>) {
        let row = self.latest.latest_row();
        (
            row.system_time.clone(),
            row.timestamp,
            row.channel_values.clone(),
        )
    }

    /// Column headers including timestamp/time
    fn headers(&self) -> Vec<String> {
        self.latest.headers()
    }

    /// Read the latest row mapped to header names
    fn read(&self) -> Snapshot {
        let headers = self.latest.headers();
        let row = self.latest.latest_row();
        let mut map = HashMap::new();
        // First two headers are timestamp, time
        if headers.len() >= 2 {
            for (name, val) in headers.iter().skip(2).zip(row.channel_values.iter()) {
                map.insert(name.clone(), *val);
            }
        }
        Snapshot {
            system_time: row.system_time.clone(),
            timestamp: row.timestamp,
            values: map,
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct Snapshot {
    #[pyo3(get)]
    system_time: String,
    #[pyo3(get)]
    timestamp: i64,
    #[pyo3(get)]
    values: HashMap<String, f64>,
}
