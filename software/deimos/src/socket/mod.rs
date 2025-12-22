//! Packetized socket interface for message-passing
//! to/from peripherals on different I/O media.

pub mod thread_channel;
pub mod udp;
pub mod unix;

use std::time::Instant;

use crate::controller::context::ControllerCtx;
use deimos_shared::peripherals::PeripheralId;

/// Socket index
pub type SocketId = usize;

/// Address of a peripheral that is communicating on a socket
pub type SocketAddr = (usize, PeripheralId);

/// Opaque token for a socket-specific address seen by recv().
pub type SocketAddrToken = u64;

/// Packetized socket interface for message-passing
/// to/from peripherals on different I/O media.
#[typetag::serde(tag = "type")]
pub trait Socket: Send + Sync {
    /// Check whether the socket is already open
    fn is_open(&self) -> bool;

    /// Do any required stateful one-time setup
    fn open(&mut self, ctx: &ControllerCtx) -> Result<(), String>;

    /// Clear state and release locks
    fn close(&mut self);

    /// Send a packet to a specific peripheral
    fn send(&mut self, id: PeripheralId, msg: &[u8]) -> Result<(), String>;

    /// Receive a packet, if available, along with an address token
    /// and a timestamp indicating when the packet was received.
    fn recv(&mut self) -> Option<(Option<PeripheralId>, SocketAddrToken, Instant, &[u8])>;

    /// Send a packet to every reachable peripheral
    fn broadcast(&mut self, msg: &[u8]) -> Result<(), String>;

    /// Update address map to associate the address identified by `token`
    /// (received via recv()) with a peripheral id.
    fn update_map(&mut self, id: PeripheralId, token: SocketAddrToken) -> Result<(), String>;
}
