use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, SystemTime};

use pyo3::prelude::*;
use tracing::warn;

// Dispatchers
use crate::RunHandle;
use crate::Socket;
use crate::calc::Calc;
use crate::dispatcher::{DataFrameDispatcher, DataFrameHandle, Dispatcher, Overflow};
use crate::peripheral::{HootlRunHandle, MockupTransport, Peripheral};

use super::*;

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
            let handle = std::thread::Builder::new()
                .name("py-controller-run".to_string())
                .spawn_scoped(s, move || ctrl.run(&None, Some(&*term_for_thread)))
                .map_err(|e| BackendErr::RunErr {
                    msg: format!("Failed to spawn controller thread: {e}"),
                })?;

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
    #[pyo3(signature=(latest_value_cutoff_freq=None))]
    fn run_nonblocking(&mut self, latest_value_cutoff_freq: Option<f64>) -> PyResult<RunHandle> {
        let controller = self.controller.take().ok_or_else(|| BackendErr::RunErr {
            msg: "Controller has already been moved into a running thread".to_string(),
        })?;
        Ok(controller.run_nonblocking(None, latest_value_cutoff_freq))
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

    /// List peripheral inputs that can be written manually.
    fn available_inputs(&mut self) -> PyResult<Vec<String>> {
        Ok(self.ctrl()?.manual_input_names())
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
    fn termination_criteria(&self, _py: Python<'_>) -> PyResult<Option<crate::Termination>> {
        Ok(self.ctx()?.termination_criteria.clone())
    }

    #[setter(termination_criteria)]
    fn set_termination_criteria(
        &mut self,
        _py: Python<'_>,
        v: Option<crate::Termination>,
    ) -> PyResult<()> {
        let criteria = v.map(|term| term.clone());
        self.ctx_mut()?.termination_criteria = criteria;
        Ok(())
    }

    #[getter(loss_of_contact_policy)]
    fn loss_of_contact_policy(&self, _py: Python<'_>) -> PyResult<crate::LossOfContactPolicy> {
        Ok(self.ctx()?.loss_of_contact_policy.clone())
    }

    #[setter(loss_of_contact_policy)]
    fn set_loss_of_contact_policy(
        &mut self,
        _py: Python<'_>,
        v: crate::LossOfContactPolicy,
    ) -> PyResult<()> {
        let dt_ns = self.ctx()?.dt_ns;
        let policy = v.clone();
        if let crate::LossOfContactPolicy::Reconnect(Some(timeout)) = &policy {
            let min_timeout = Duration::from_millis(40);
            let cycle_timeout = Duration::from_nanos(u64::from(dt_ns) * 4);
            if *timeout < min_timeout || *timeout < cycle_timeout {
                warn!(
                    "Reconnect timeout {:?} is less than 40ms or less than 4x cycle dt; unlikely to succeed.",
                    timeout
                );
            }
        }
        self.ctx_mut()?.loss_of_contact_policy = policy;
        Ok(())
    }

    #[getter(loop_method)]
    fn loop_method(&self, _py: Python<'_>) -> PyResult<crate::LoopMethod> {
        Ok(self.ctx()?.loop_method.clone())
    }

    #[setter(loop_method)]
    fn set_loop_method(&mut self, _py: Python<'_>, v: crate::LoopMethod) -> PyResult<()> {
        self.ctx_mut()?.loop_method = v.clone();
        Ok(())
    }

    #[getter(enable_manual_inputs)]
    fn enable_manual_inputs(&self) -> PyResult<bool> {
        Ok(self.ctx()?.enable_manual_inputs)
    }

    #[setter(enable_manual_inputs)]
    fn set_enable_manual_inputs(&mut self, v: bool) -> PyResult<()> {
        self.ctx_mut()?.enable_manual_inputs = v;
        Ok(())
    }

    fn add_peripheral(&mut self, name: &str, p: Box<dyn Peripheral>) -> PyResult<()> {
        self.ctrl()?.add_peripheral(name, p);
        Ok(())
    }

    #[pyo3(signature=(peripheral_name, transport, end_epoch_ns=None))]
    fn attach_hootl_driver(
        &mut self,
        peripheral_name: &str,
        transport: MockupTransport,
        end_epoch_ns: Option<u64>,
    ) -> PyResult<HootlRunHandle> {
        let end = match end_epoch_ns {
            Some(ns) => Some(
                SystemTime::UNIX_EPOCH
                    .checked_add(Duration::from_nanos(ns))
                    .ok_or_else(|| {
                        pyo3::exceptions::PyValueError::new_err("Invalid end_epoch_ns")
                    })?,
            ),
            None => None,
        };
        self.ctrl()?
            .attach_hootl_driver(peripheral_name, transport, end)
            .map_err(|e| BackendErr::InvalidPeripheralErr { msg: e }.into())
    }

    fn add_calc(&mut self, name: &str, calc: Box<dyn Calc>) -> PyResult<()> {
        self.ctrl()?.add_calc(name, calc);
        Ok(())
    }

    /// Add an in-memory dataframe dispatcher and return its shared handle.
    fn add_dataframe_dispatcher(
        &mut self,
        name: &str,
        max_size_megabytes: usize,
        overflow_behavior: Overflow,
    ) -> PyResult<DataFrameHandle> {
        let (dispatcher, _df_handle) =
            DataFrameDispatcher::new(max_size_megabytes, overflow_behavior, None);
        let handle = dispatcher.handle();
        self.ctrl()?.add_dispatcher(name, dispatcher);
        Ok(handle)
    }

    /// Add a dispatcher via a JSON-serializable dispatcher instance.
    fn add_dispatcher(&mut self, name: &str, dispatcher: Box<dyn Dispatcher>) -> PyResult<()> {
        self.ctrl()?.add_dispatcher(name, dispatcher);
        Ok(())
    }

    fn dispatcher_names(&mut self) -> PyResult<Vec<String>> {
        Ok(self.ctrl()?.dispatcher_names())
    }

    fn remove_dispatcher(&mut self, name: &str) -> PyResult<bool> {
        Ok(self.ctrl()?.remove_dispatcher(name).is_some())
    }

    /// Add a socket via a JSON-serializable socket instance.
    fn add_socket(&mut self, name: &str, socket: Box<dyn Socket>) -> PyResult<()> {
        self.ctrl()?.add_socket(name, socket);
        Ok(())
    }

    fn remove_socket(&mut self, name: &str) -> PyResult<bool> {
        Ok(self.ctrl()?.remove_socket(name).is_some())
    }

    fn set_peripheral_input_source(
        &mut self,
        input_field: &str,
        source_field: &str,
    ) -> PyResult<()> {
        self.ctrl()?
            .set_peripheral_input_source(input_field, source_field);
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
