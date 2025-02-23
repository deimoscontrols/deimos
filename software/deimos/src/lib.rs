#![doc = include_str!("../README.md")]
#![allow(clippy::needless_range_loop)]

pub mod calc;
pub mod controller;
pub mod dispatcher;
pub mod peripheral;
pub mod socket;

pub use controller::{
    Controller,
    context::{ControllerCtx, LossOfContactPolicy, Termination},
};
pub use dispatcher::{CsvDispatcher, Dispatcher};
pub use socket::{Socket, SocketAddr, SocketId, udp::UdpSocket, unix::UnixSocket};

#[cfg(feature = "tsdb")]
pub use dispatcher::TimescaleDbDispatcher;

#[cfg(feature = "df")]
pub use dispatcher::DataFrameDispatcher;
