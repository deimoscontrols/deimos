use std::sync::atomic::AtomicBool;
use std::time::Duration;

// use numpy::borrow::{PyReadonlyArray1, PyReadwriteArray1};
use pyo3::exceptions;
use pyo3::prelude::*;

use deimos_shared::peripherals::model_numbers;

// Dispatchers
use crate::CsvDispatcher;
use crate::TimescaleDbDispatcher;

// Peripherals
use crate::peripheral::AnalogIRev2;
use crate::peripheral::AnalogIRev3;
use crate::peripheral::AnalogIRev4;
use crate::peripheral::DeimosDaqRev5;
use crate::peripheral::DeimosDaqRev6;

use crate::calc::Calc;

pub use crate::dispatcher::Overflow;
mod calc;
// mod dispatcher;

#[derive(Debug)]
#[allow(dead_code)]
enum BackendErr {
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
    controller: crate::Controller,
}

impl Controller {
    fn ctx(&self) -> &crate::ControllerCtx {
        &self.controller.ctx
    }

    fn ctx_mut(&mut self) -> &mut crate::ControllerCtx {
        &mut self.controller.ctx
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

        Ok(Self { controller })
    }

    /// Run the control program
    fn run(&mut self, py: Python<'_>) -> PyResult<String> {
        // Shared signal indicating whether the controller should exit
        let termination_signal = AtomicBool::new(false);

        std::thread::scope(|s| {
            let handle = s.spawn(|| self.controller.run(&None, Some(&termination_signal)));

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

    /// Scan the local network (and any other attached sockets) for available peripherals.
    #[pyo3(signature=(timeout_ms=10))]
    fn scan(&mut self, timeout_ms: u16) -> PyResult<Vec<Peripheral>> {
        match self.controller.scan(timeout_ms, &None) {
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
    fn op_name(&mut self) -> String {
        self.ctx().op_name.clone()
    }

    #[setter(op_name)]
    fn set_op_name(&mut self, v: &str) {
        self.ctx_mut().op_name = v.to_string();
    }

    #[getter(op_dir)]
    fn op_dir(&self) -> PyResult<String> {
        if let Some(x) = self.ctx().op_dir.to_str() {
            Ok(x.to_string())
        } else {
            let invalid_string = self.ctx().op_dir.to_string_lossy();
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
    fn set_op_dir(&mut self, v: &str) {
        self.ctx_mut().op_dir = v.to_string().into();
    }

    #[getter(dt_ns)]
    fn dt_ns(&self) -> u32 {
        self.ctx().dt_ns
    }

    #[setter(dt_ns)]
    fn set_dt_ns(&mut self, v: u32) {
        self.ctx_mut().dt_ns = v;
    }

    #[getter(rate_hz)]
    fn rate_hz(&self) -> f64 {
        1e9 / self.ctx().dt_ns as f64
    }

    #[setter(rate_hz)]
    fn set_rate_hz(&mut self, v: f64) {
        self.ctx_mut().dt_ns = (1e9 / v) as u32;
    }

    #[getter(peripheral_loss_of_contact_limit)]
    fn peripheral_loss_of_contact_limit(&self) -> u16 {
        self.ctx().peripheral_loss_of_contact_limit
    }

    #[setter(peripheral_loss_of_contact_limit)]
    fn set_peripheral_loss_of_contact_limit(&mut self, v: u16) {
        self.ctx_mut().peripheral_loss_of_contact_limit = v
    }

    #[getter(controller_loss_of_contact_limit)]
    fn controller_loss_of_contact_limit(&self) -> u16 {
        self.ctx().controller_loss_of_contact_limit
    }

    #[setter(controller_loss_of_contact_limit)]
    fn set_controller_loss_of_contact_limit(&mut self, v: u16) {
        self.ctx_mut().controller_loss_of_contact_limit = v
    }

    #[pyo3(signature=(name, p, sn=None))]
    fn add_peripheral(&mut self, name: &str, p: &Peripheral, sn: Option<u64>) -> PyResult<()> {
        match p {
            Peripheral::AnalogIRev2 { serial_number } => {
                let p = Box::new(AnalogIRev2 {
                    serial_number: sn.unwrap_or(*serial_number),
                });
                self.controller.add_peripheral(name, p);
            }
            Peripheral::AnalogIRev3 { serial_number } => {
                let p = Box::new(AnalogIRev3 {
                    serial_number: sn.unwrap_or(*serial_number),
                });
                self.controller.add_peripheral(name, p);
            }
            Peripheral::AnalogIRev4 { serial_number } => {
                let p = Box::new(AnalogIRev4 {
                    serial_number: sn.unwrap_or(*serial_number),
                });
                self.controller.add_peripheral(name, p);
            }
            Peripheral::DeimosDaqRev5 { serial_number } => {
                let p = Box::new(DeimosDaqRev5 {
                    serial_number: sn.unwrap_or(*serial_number),
                });
                self.controller.add_peripheral(name, p);
            }
            Peripheral::DeimosDaqRev6 { serial_number } => {
                let p = Box::new(DeimosDaqRev6 {
                    serial_number: sn.unwrap_or(*serial_number),
                });
                self.controller.add_peripheral(name, p);
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

    /// Write data to a CSV file in `op_dir` with name `{op}.csv`.
    ///
    /// The file is pre-allocated to avoid resizing in the loop,
    /// then trimmed at the end of data collection if the final
    /// size is less than what was allocated.
    #[pyo3(signature=(chunk_size_megabytes=100, overflow_behavior=Overflow::Wrap))]
    fn add_csv_dispatcher(&mut self, chunk_size_megabytes: usize, overflow_behavior: Overflow) {
        let d = CsvDispatcher::new(chunk_size_megabytes, overflow_behavior);
        self.controller.add_dispatcher(Box::new(d));
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
    ) {
        let d = TimescaleDbDispatcher::new(
            dbname,
            host,
            user,
            token_name,
            Duration::from_nanos(1),
            retention_time_hours,
        );
        self.controller.add_dispatcher(Box::new(d));
    }

    fn add_calc(&mut self, name: &str, calc: Box<dyn Calc>) {
        self.controller.add_calc(name, calc);
    }
}

#[pymodule]
#[pyo3(name = "deimos")]
fn deimos<'py>(_py: Python, m: &Bound<'py, PyModule>) -> PyResult<()> {
    // m.add_function(wrap_pyfunction!(interpn_linear_regular_f64, m)?)?;
    m.add_class::<Controller>()?;
    m.add_class::<Peripheral>()?;
    m.add_class::<Overflow>()?;

    Ok(())
}
