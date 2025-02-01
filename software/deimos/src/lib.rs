#![doc = include_str!("../README.md")]
#![allow(clippy::needless_range_loop)]

pub mod controller;
pub mod dispatcher;
pub mod orchestrator;
pub mod socket;

pub use controller::{Controller, context::{ControllerCtx, LossOfContactPolicy, Termination}};
pub use dispatcher::{CsvDispatcher, Dispatcher, TimescaleDbDispatcher};
pub use socket::{SuperSocket, SuperSocketAddr, SuperSocketId, udp::UdpSuperSocket, unix::UnixSuperSocket};