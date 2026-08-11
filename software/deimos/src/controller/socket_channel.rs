//! One-to-one in-process channels used by peripheral [`ThreadChannelSocket`]s.
//!
//! Unlike user channels, each side of a socket channel can be claimed only
//! once. Dropping an endpoint returns its sender/receiver pair to the channel,
//! allowing a later controller run or driver attachment to reuse the identity.
//!
//! [`ThreadChannelSocket`]: crate::socket::thread_channel::ThreadChannelSocket

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crossbeam::channel::{
    Receiver, RecvTimeoutError, SendError, SendTimeoutError, Sender, TryRecvError, bounded,
};
use deimos_shared::peripherals::PeripheralId;

const CHANNEL_CAPACITY: usize = 10;

type EndpointInner = (Sender<Vec<u8>>, Receiver<Vec<u8>>);

/// Runtime registry of one-to-one socket channels, keyed by peripheral ID.
///
/// The map is private so callers cannot replace an entry while one of its
/// endpoints is active.
#[derive(Clone, Debug, Default)]
pub struct SocketChannels {
    channels: Arc<RwLock<BTreeMap<PeripheralId, Arc<SocketChannel>>>>,
}

impl SocketChannels {
    pub(crate) fn claim_controller(&self, id: PeripheralId) -> Result<SocketEndpoint, String> {
        self.channel(id)?.claim(id, Side::Controller)
    }

    pub(crate) fn claim_peripheral(&self, id: PeripheralId) -> Result<SocketEndpoint, String> {
        self.channel(id)?.claim(id, Side::Peripheral)
    }

    fn channel(&self, id: PeripheralId) -> Result<Arc<SocketChannel>, String> {
        let mut channels = self
            .channels
            .write()
            .map_err(|_| "ThreadChannelSocket channel registry is poisoned".to_owned())?;
        Ok(channels
            .entry(id)
            .or_insert_with(|| Arc::new(SocketChannel::default()))
            .clone())
    }
}

#[derive(Clone, Copy, Debug)]
enum Side {
    Controller,
    Peripheral,
}

#[derive(Debug)]
struct ChannelEnds {
    controller: Option<EndpointInner>,
    peripheral: Option<EndpointInner>,
}

impl ChannelEnds {
    fn new() -> Self {
        let (controller_tx, peripheral_rx) = bounded(CHANNEL_CAPACITY);
        let (peripheral_tx, controller_rx) = bounded(CHANNEL_CAPACITY);
        Self {
            controller: Some((controller_tx, controller_rx)),
            peripheral: Some((peripheral_tx, peripheral_rx)),
        }
    }

    fn slot(&mut self, side: Side) -> &mut Option<EndpointInner> {
        match side {
            Side::Controller => &mut self.controller,
            Side::Peripheral => &mut self.peripheral,
        }
    }
}

#[derive(Debug)]
struct SocketChannel {
    ends: Mutex<ChannelEnds>,
}

impl Default for SocketChannel {
    fn default() -> Self {
        Self {
            ends: Mutex::new(ChannelEnds::new()),
        }
    }
}

impl SocketChannel {
    fn claim(self: &Arc<Self>, id: PeripheralId, side: Side) -> Result<SocketEndpoint, String> {
        let mut ends = self
            .ends
            .lock()
            .map_err(|_| format!("ThreadChannelSocket channel {id:?} is poisoned"))?;
        let inner = ends
            .slot(side)
            .take()
            .ok_or_else(|| format!("ThreadChannelSocket {side:?} endpoint for {id:?} is active"))?;
        Ok(SocketEndpoint {
            side,
            channel: self.clone(),
            inner: Some(inner),
        })
    }
}

/// One exclusively owned side of a bidirectional socket channel.
///
/// This type is intentionally not cloneable. Dropping it releases the role so
/// another socket or peripheral responder can claim the same identity.
#[derive(Debug)]
pub struct SocketEndpoint {
    side: Side,
    channel: Arc<SocketChannel>,
    inner: Option<EndpointInner>,
}

impl SocketEndpoint {
    /// Send one complete packet to the opposite endpoint.
    ///
    /// Args:
    ///   packet: Owned packet bytes to enqueue.
    ///
    /// Returns:
    ///   Success after the packet enters the bounded channel.
    ///
    /// Errors:
    ///   Returns the unsent packet when the opposite endpoint is disconnected.
    pub fn send(&self, packet: Vec<u8>) -> Result<(), SendError<Vec<u8>>> {
        self.inner.as_ref().unwrap().0.send(packet)
    }

    /// Send one packet, waiting no longer than `timeout` for buffer capacity.
    ///
    /// Args:
    ///   packet: Owned packet bytes to enqueue.
    ///   timeout: Maximum time to wait for channel capacity.
    ///
    /// Returns:
    ///   Success after the packet enters the bounded channel.
    ///
    /// Errors:
    ///   Returns the unsent packet after timeout or disconnection.
    pub fn send_timeout(
        &self,
        packet: Vec<u8>,
        timeout: Duration,
    ) -> Result<(), SendTimeoutError<Vec<u8>>> {
        self.inner.as_ref().unwrap().0.send_timeout(packet, timeout)
    }

    /// Receive one packet without waiting.
    ///
    /// Returns:
    ///   The next complete packet from the opposite endpoint.
    ///
    /// Errors:
    ///   Returns immediately when the channel is empty or disconnected.
    pub fn try_recv(&self) -> Result<Vec<u8>, TryRecvError> {
        self.inner.as_ref().unwrap().1.try_recv()
    }

    /// Receive one packet, waiting no longer than `timeout`.
    ///
    /// Args:
    ///   timeout: Maximum time to wait for a packet.
    ///
    /// Returns:
    ///   The next complete packet from the opposite endpoint.
    ///
    /// Errors:
    ///   Returns after timeout or disconnection.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Vec<u8>, RecvTimeoutError> {
        self.inner.as_ref().unwrap().1.recv_timeout(timeout)
    }
}

impl Drop for SocketEndpoint {
    fn drop(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };
        let Ok(mut ends) = self.channel.ends.lock() else {
            return;
        };
        let slot = ends.slot(self.side);
        debug_assert!(slot.is_none());
        if slot.is_none() {
            *slot = Some(inner);
        }
        // Once neither side is active, replace the queues so a later pairing
        // cannot observe stale packets from the completed connection.
        if ends.controller.is_some() && ends.peripheral.is_some() {
            *ends = ChannelEnds::new();
        }
    }
}

pub(crate) fn socket_channels_default() -> SocketChannels {
    SocketChannels::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: PeripheralId = PeripheralId {
        model_number: 1,
        serial_number: 2,
    };

    #[test]
    fn endpoint_drop_releases_the_role_and_clears_completed_connection_packets() {
        let channels = SocketChannels::default();
        let controller = channels.claim_controller(ID).unwrap();
        let peripheral = channels.claim_peripheral(ID).unwrap();
        assert!(channels.claim_peripheral(ID).is_err());

        controller.send(vec![1]).unwrap();
        assert_eq!(peripheral.recv_timeout(Duration::ZERO).unwrap(), vec![1]);
        peripheral.send(vec![2]).unwrap();
        drop(controller);
        drop(peripheral);

        let controller = channels.claim_controller(ID).unwrap();
        let peripheral = channels.claim_peripheral(ID).unwrap();
        assert!(controller.try_recv().is_err());
        controller.send(vec![3]).unwrap();
        assert_eq!(peripheral.recv_timeout(Duration::ZERO).unwrap(), vec![3]);
    }
}
