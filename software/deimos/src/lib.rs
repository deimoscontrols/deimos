#![doc = include_str!("../README.md")]
#![allow(clippy::needless_range_loop)]

pub mod calc;
pub mod controller;
pub mod dispatcher;
pub mod peripheral;
pub mod socket;

pub use controller::{
    context::{ControllerCtx, LossOfContactPolicy, Termination},
    Controller,
};
pub use dispatcher::{CsvDispatcher, Dispatcher};
pub use socket::{
    udp::UdpSuperSocket, unix::UnixSuperSocket, SuperSocket, SuperSocketAddr, SuperSocketId,
};


#[cfg(feature="tsdb")]
pub use dispatcher::TimescaleDbDispatcher;

#[cfg(feature="df")]
pub use dispatcher::DataFrameDispatcher;