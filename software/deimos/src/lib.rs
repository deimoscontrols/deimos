#![doc = include_str!("../README.md")]
#![allow(clippy::needless_range_loop)]

pub mod controller;
pub mod dispatcher;
pub mod orchestrator;
pub mod socket;

pub use controller::Controller;
pub use dispatcher::{CsvDispatcher, Dispatcher, TimescaleDbDispatcher};
