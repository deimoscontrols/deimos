//! Packetized socket interface for message-passing
//! to/from peripherals on different I/O media.

pub mod udp;
pub mod unix;

use deimos_shared::peripherals::PeripheralId;
use std::time::Instant;

/// Socket index
pub type SuperSocketId = usize;

/// Address of a peripheral that is communicating on a socket
pub type SuperSocketAddr = (usize, PeripheralId);

/// Packetized socket interface for message-passing
/// to/from peripherals on different I/O media.
#[typetag::serde(tag = "type")]
pub trait SuperSocket: Send + Sync {
    /// Do any required stateful one-time setup
    fn open(&mut self) -> Result<(), String>;

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
    fn update_map(&mut self, id: PeripheralId);
}
