//! Packetized socket interface for message-passing
//! to/from peripherals on different I/O media.

pub mod orchestrator;
pub mod thread_channel;
pub mod udp;
pub mod unix;
pub mod worker;

use std::time::{Duration, Instant};

use crate::controller::context::ControllerCtx;
use deimos_shared::peripherals::PeripheralId;

/// Socket index
pub type SocketId = usize;

/// Address of a peripheral that is communicating on a socket
pub type SocketAddr = (usize, PeripheralId);

/// Opaque token for a socket-specific address seen by recv().
pub type SocketAddrToken = u64;

pub use orchestrator::SocketOrchestrator;
pub use worker::{SocketWorker, SocketWorkerCommand, SocketWorkerEvent, SocketWorkerHandle};

/// Packet with metadata for Socket interface.
pub struct SocketPacketMeta {
    /// Peripheral ID
    pub pid: Option<PeripheralId>,

    /// Internal address index
    pub token: SocketAddrToken,

    /// Packet arrival time.
    /// This is used for time synchronization
    /// and should be as accurate as possible.
    pub time: Instant,

    /// Packet size in bytes
    pub size: usize,
}

pub struct SocketRecvMeta {
    pub socket_id: SocketId,
    pub pid: Option<PeripheralId>,
    pub token: SocketAddrToken,
    pub time: Instant,
    pub size: usize,
}

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
    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Option<SocketPacketMeta>;

    /// Send a packet to every reachable peripheral
    fn broadcast(&mut self, msg: &[u8]) -> Result<(), String>;

    /// Update address map to associate the address identified by `token`
    /// (received via recv()) with a peripheral id.
    fn update_map(&mut self, id: PeripheralId, token: SocketAddrToken) -> Result<(), String>;
}
