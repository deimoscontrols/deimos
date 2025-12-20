#![doc = include_str!("../README.md")]
#![allow(clippy::needless_range_loop)]

pub mod calc;
pub mod controller;
pub mod dispatcher;
pub mod logging;
pub mod math;
pub mod peripheral;
pub mod socket;

pub use controller::{
    Controller,
    context::{ControllerCtx, LossOfContactPolicy, Termination},
};
pub use dispatcher::{ChannelFilter, CsvDispatcher, Dispatcher};
pub use socket::{Socket, SocketAddr, SocketId, udp::UdpSocket, unix::UnixSocket};

pub use dispatcher::DataFrameDispatcher;
pub use dispatcher::TimescaleDbDispatcher;

#[cfg(feature = "python")]
pub mod python;
