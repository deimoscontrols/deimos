use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

// use numpy::borrow::{PyReadonlyArray1, PyReadwriteArray1};
use pyo3::exceptions;
use pyo3::prelude::*;
use std::collections::HashMap;

use deimos_shared::peripherals::model_numbers;
use pyo3::wrap_pymodule;

// Dispatchers
use crate::CsvDispatcher;
use crate::TimescaleDbDispatcher;

use crate::UdpSocket;
use crate::UnixSocket;
use crate::dispatcher::LatestValueDispatcher;
use crate::dispatcher::LatestValueHandle;
// Peripherals
use crate::peripheral::AnalogIRev2;
use crate::peripheral::AnalogIRev3;
use crate::peripheral::AnalogIRev4;
use crate::peripheral::DeimosDaqRev5;
use crate::peripheral::DeimosDaqRev6;

use crate::calc::Calc;

pub use crate::dispatcher::Overflow;
mod transfer;
// mod dispatcher;

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum BackendErr {
    InvalidPathErr { msg: String },
    RunErr { msg: String },
    InvalidPeripheralErr { msg: String },
    InvalidCalcErr { msg: String },
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
        }
    }
}

#[pyclass]
#[non_exhaustive]
#[derive(Debug)]
enum Peripheral {
    Experimental { serial_number: u64 },
    Unknown { serial_number: u64 },
    AnalogIRev2 { serial_number: u64 },
    AnalogIRev3 { serial_number: u64 },
    AnalogIRev4 { serial_number: u64 },
    DeimosDaqRev5 { serial_number: u64 },
    DeimosDaqRev6 { serial_number: u64 },
}

#[pyclass]
struct Controller {
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

    fn ctx(&self) -> PyResult<&crate::ControllerCtx> {
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
    fn scan(&mut self, timeout_ms: u16) -> PyResult<Vec<Peripheral>> {
        match self.ctrl()?.scan(timeout_ms, &None) {
            Err(msg) => Err(BackendErr::RunErr { msg }.into()),
            Ok(map) => {
                let mut peripherals = vec![];

                for (_, id) in map.keys() {
                    let m = id.model_number;

                    let p = match m {
                        model_numbers::EXPERIMENTAL_MODEL_NUMBER => Peripheral::Experimental {
                            serial_number: id.serial_number,
                        },

                        model_numbers::ANALOG_I_REV_2_MODEL_NUMBER => Peripheral::AnalogIRev2 {
                            serial_number: id.serial_number,
                        },

                        model_numbers::ANALOG_I_REV_3_MODEL_NUMBER => Peripheral::AnalogIRev3 {
                            serial_number: id.serial_number,
                        },

                        model_numbers::ANALOG_I_REV_4_MODEL_NUMBER => Peripheral::AnalogIRev4 {
                            serial_number: id.serial_number,
                        },

                        model_numbers::DEIMOS_DAQ_REV_5_MODEL_NUMBER => Peripheral::DeimosDaqRev5 {
                            serial_number: id.serial_number,
                        },

                        model_numbers::DEIMOS_DAQ_REV_6_MODEL_NUMBER => Peripheral::DeimosDaqRev6 {
                            serial_number: id.serial_number,
                        },

                        _ => Peripheral::Unknown {
                            serial_number: id.serial_number,
                        },
                    };

                    peripherals.push(p);
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

    #[pyo3(signature=(name, p, sn=None))]
    fn add_peripheral(&mut self, name: &str, p: &Peripheral, sn: Option<u64>) -> PyResult<()> {
        let ctrl = self.ctrl()?;
        match p {
            Peripheral::AnalogIRev2 { serial_number } => {
                let p = Box::new(AnalogIRev2 {
                    serial_number: sn.unwrap_or(*serial_number),
                });
                ctrl.add_peripheral(name, p);
            }
            Peripheral::AnalogIRev3 { serial_number } => {
                let p = Box::new(AnalogIRev3 {
                    serial_number: sn.unwrap_or(*serial_number),
                });
                ctrl.add_peripheral(name, p);
            }
            Peripheral::AnalogIRev4 { serial_number } => {
                let p = Box::new(AnalogIRev4 {
                    serial_number: sn.unwrap_or(*serial_number),
                });
                ctrl.add_peripheral(name, p);
            }
            Peripheral::DeimosDaqRev5 { serial_number } => {
                let p = Box::new(DeimosDaqRev5 {
                    serial_number: sn.unwrap_or(*serial_number),
                });
                ctrl.add_peripheral(name, p);
            }
            Peripheral::DeimosDaqRev6 { serial_number } => {
                let p = Box::new(DeimosDaqRev6 {
                    serial_number: sn.unwrap_or(*serial_number),
                });
                ctrl.add_peripheral(name, p);
            }
            _ => {
                return Err(BackendErr::InvalidPeripheralErr {
                    msg: format!(
                        "Peripherals used by controller must be known, specific models, not {:?}",
                        p
                    ),
                }
                .into());
            }
        };

        Ok(())
    }

    fn add_calc(&mut self, name: &str, calc: Box<dyn Calc>) -> PyResult<()> {
        self.ctrl()?.add_calc(name, calc);
        Ok(())
    }

    /// Write data to a CSV file in `op_dir` with name `{op}.csv`.
    ///
    /// The file is pre-allocated to avoid resizing in the loop,
    /// then trimmed at the end of data collection if the final
    /// size is less than what was allocated.
    #[pyo3(signature=(chunk_size_megabytes=100, overflow_behavior=Overflow::Wrap))]
    fn add_csv_dispatcher(
        &mut self,
        chunk_size_megabytes: usize,
        overflow_behavior: Overflow,
    ) -> PyResult<()> {
        let d = CsvDispatcher::new(chunk_size_megabytes, overflow_behavior);
        self.ctrl()?.add_dispatcher(Box::new(d));
        Ok(())
    }

    /// Send unencrypted data to a TimescaleDB postgres database.
    ///
    /// The host can be either an IP address like `192.168.8.231`,
    /// an IP address with a port like `192.168.8.231:5432`,
    /// or a unix socket address like `/run/postgresql/`.
    /// If not port is specified, the default port 5432 is used.
    ///
    /// `token_name` must match an environment variable containing the
    /// required auth credentials for the postgres user.
    fn add_timescaledb_dispatcher(
        &mut self,
        dbname: &str,
        host: &str,
        user: &str,
        token_name: &str,
        retention_time_hours: u64,
    ) -> PyResult<()> {
        let d = TimescaleDbDispatcher::new(
            dbname,
            host,
            user,
            token_name,
            Duration::from_nanos(1),
            retention_time_hours,
        );
        self.ctrl()?.add_dispatcher(Box::new(d));
        Ok(())
    }

    /// Add a unix socket in {op_dir}/sock/{name} for
    /// peripherals to send data to the controller.
    ///
    /// Sockets for communicating to from the controller to
    /// each peripheral are expected in {op_dir}/sock/per/{...}.
    fn add_unix_socket(&mut self, name: &str) -> PyResult<()> {
        self.ctrl()?.add_socket(Box::new(UnixSocket::new(&name)));
        Ok(())
    }

    /// Add a UDP socket receiving on port 12368, sending on port 12367.
    /// The should not be needed often, since a fresh Controller comes with
    /// a UDP socket by default.
    fn add_udp_socket(&mut self) -> PyResult<()> {
        self.ctrl()?.add_socket(Box::new(UdpSocket::new()));
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

#[pymodule]
#[pyo3(name = "deimos")]
fn deimos<'py>(_py: Python, m: &Bound<'py, PyModule>) -> PyResult<()> {
    m.add_class::<Controller>()?;
    m.add_class::<Peripheral>()?;
    m.add_class::<Overflow>()?;
    m.add_class::<RunHandle>()?;
    m.add_class::<Snapshot>()?;

    #[pymodule]
    #[pyo3(name = "calc")]
    mod calc_ {

        #[pymodule_export]
        pub use crate::calc::{
            Affine, Butter2, Constant, InverseAffine, Pid, Polynomial, RtdPt100, Sin, TcKtype,
        };
    }

    m.add_wrapped(wrap_pymodule!(calc_))?;

    Ok(())
}
