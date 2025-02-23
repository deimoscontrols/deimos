//! Packetized socket interface for message-passing
//! to/from peripherals on different I/O media.

pub mod udp;
pub mod unix;

use std::time::Instant;

use crate::controller::context::ControllerCtx;
use deimos_shared::peripherals::PeripheralId;

/// Socket index
pub type PSocketId = usize;

/// Address of a peripheral that is communicating on a socket
pub type PSocketAddr = (usize, PeripheralId);

/// Packetized socket interface for message-passing
/// to/from peripherals on different I/O media.
#[cfg_attr(feature = "ser", typetag::serde(tag = "type"))]
pub trait PSocket: Send + Sync {
    /// Check whether the socket is already open
    fn is_open(&self) -> bool;

    /// Do any required stateful one-time setup
    fn open(&mut self, ctx: &ControllerCtx) -> Result<(), String>;

    /// Clear state and release locks
    fn close(&mut self);

    /// Send a packet to a specific peripheral
    fn send(&mut self, id: PeripheralId, msg: &[u8]) -> Result<(), String>;

    /// Receive a packet, if available, along with the associated
    /// a timestamp indicating when the packet was received.
    fn recv(&mut self) -> Option<(Option<PeripheralId>, Instant, &[u8])>;

    /// Send a packet to every reachable peripheral
    fn broadcast(&mut self, msg: &[u8]) -> Result<(), String>;

    /// Update address map to associate the most recent address that
    /// provided a packet via recv() with a peripheral id.
    fn update_map(&mut self, id: PeripheralId) -> Result<(), String>;
}
