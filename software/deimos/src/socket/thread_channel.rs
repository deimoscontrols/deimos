//! One-to-one in-process socket keyed by a peripheral identity.
//!
//! This transport uses the dedicated SPSC channel registry in
//! [`ControllerCtx`] rather than the MPMC user channels used for sideloading.

use std::time::{Duration, Instant};

use deimos_shared::peripherals::PeripheralId;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::{Socket, SocketAddrToken, SocketPacketMeta};
use crate::controller::context::ControllerCtx;
use crate::controller::socket_channel::SocketEndpoint;
use crate::py_json_methods;

/// Controller-side socket for one in-process peripheral responder.
///
/// The matching responder claims the peripheral endpoint for the same
/// [`PeripheralId`]. The registry rejects a second live claimant on either
/// side, and dropping this socket releases its controller endpoint.
#[derive(Serialize, Deserialize, Default)]
#[cfg_attr(feature = "python", pyclass)]
pub struct ThreadChannelSocket {
    model_number: u64,
    serial_number: u64,
    #[serde(skip)]
    endpoint: Option<SocketEndpoint>,
}

impl ThreadChannelSocket {
    /// Create a socket dedicated to `id`.
    ///
    /// Args:
    ///   id: The only peripheral identity reachable through this socket.
    ///
    /// Returns:
    ///   A closed socket ready to claim its controller endpoint during open.
    pub fn new(id: PeripheralId) -> Self {
        Self {
            model_number: id.model_number,
            serial_number: id.serial_number,
            endpoint: None,
        }
    }

    /// Return the only peripheral identity reachable through this socket.
    ///
    /// Returns:
    ///   The model and serial number used to key the socket channel.
    pub fn id(&self) -> PeripheralId {
        PeripheralId {
            model_number: self.model_number,
            serial_number: self.serial_number,
        }
    }

    /// Return the canonical controller socket-map name for `id`.
    ///
    /// Args:
    ///   id: Peripheral identity to encode in the diagnostic socket name.
    ///
    /// Returns:
    ///   A deterministic name suitable for [`Controller::add_socket`].
    ///
    /// [`Controller::add_socket`]: crate::controller::Controller::add_socket
    pub fn socket_name(id: PeripheralId) -> String {
        format!("thread-{:016x}-{:016x}", id.model_number, id.serial_number)
    }
}

py_json_methods!(
    ThreadChannelSocket,
    Socket,
    #[new]
    fn py_new(model_number: u64, serial_number: u64) -> PyResult<Self> {
        Ok(Self::new(PeripheralId {
            model_number,
            serial_number,
        }))
    }
);

#[typetag::serde]
impl Socket for ThreadChannelSocket {
    fn is_open(&self) -> bool {
        self.endpoint.is_some()
    }

    fn open(&mut self, ctx: &ControllerCtx) -> Result<(), String> {
        if self.endpoint.is_none() {
            self.endpoint = Some(ctx.controller_socket_endpoint(self.id())?);
            info!("Opened thread channel socket for {:?}", self.id());
        }
        Ok(())
    }

    fn close(&mut self) {
        if self.endpoint.take().is_some() {
            info!("Closed thread channel socket for {:?}", self.id());
        }
    }

    fn send(&mut self, id: PeripheralId, msg: &[u8]) -> Result<(), String> {
        let expected = self.id();
        if id != expected {
            return Err(format!(
                "ThreadChannelSocket for {expected:?} cannot send to {id:?}"
            ));
        }
        self.endpoint
            .as_ref()
            .ok_or_else(|| "Unable to send before ThreadChannelSocket is open".to_owned())?
            .send(msg.to_vec())
            .map_err(|err| format!("Failed to send thread-channel packet: {err}"))
    }

    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Option<SocketPacketMeta> {
        let endpoint = self.endpoint.as_ref()?;
        let bytes = if timeout.is_zero() {
            endpoint.try_recv().ok()?
        } else {
            endpoint.recv_timeout(timeout).ok()?
        };
        let size = bytes.len().min(buf.len());
        buf[..size].copy_from_slice(&bytes[..size]);
        Some(SocketPacketMeta {
            pid: Some(self.id()),
            token: 0,
            time: Instant::now(),
            size,
        })
    }

    fn broadcast(&mut self, msg: &[u8]) -> Result<(), String> {
        self.send(self.id(), msg)
    }

    fn update_map(&mut self, id: PeripheralId, _token: SocketAddrToken) -> Result<(), String> {
        let expected = self.id();
        if id == expected {
            Ok(())
        } else {
            Err(format!(
                "ThreadChannelSocket for {expected:?} received identity {id:?}"
            ))
        }
    }
}

impl Drop for ThreadChannelSocket {
    fn drop(&mut self) {
        self.close();
    }
}
