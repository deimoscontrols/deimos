#![doc = include_str!("../README.md")]
#![allow(clippy::needless_range_loop)]

pub mod calcs;
pub mod controller;
pub mod dispatcher;
pub mod orchestrator;
pub mod peripherals;
pub mod socket;

pub use controller::{
    context::{ControllerCtx, LossOfContactPolicy, Termination},
    Controller,
};
pub use dispatcher::{CsvDispatcher, Dispatcher, TimescaleDbDispatcher};
pub use socket::{
    udp::UdpSuperSocket, unix::UnixSuperSocket, SuperSocket, SuperSocketAddr, SuperSocketId,
};
