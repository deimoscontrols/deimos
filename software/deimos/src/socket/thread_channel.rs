//! Socket implementation backed by a controller user channel.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;
use tracing::info;

use crate::controller::channel::{Endpoint, Msg};
use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

use super::{Socket, SocketAddrToken, SocketPacketMeta};
use deimos_shared::peripherals::PeripheralId;
use deimos_shared::states::{ByteStruct, ByteStructLen};

/// Socket implementation that communicates over a named user channel.
#[derive(Serialize, Deserialize, Default)]
#[cfg_attr(feature = "python", pyclass)]
pub struct ThreadChannelSocket {
    name: String,
    #[serde(skip)]
    endpoint: Option<Endpoint>,
}

impl ThreadChannelSocket {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            endpoint: None,
        }
    }

    /// Channel name used to resolve the user channel.
    pub fn name(&self) -> &str {
        &self.name
    }
}

py_json_methods!(
    ThreadChannelSocket,
    Socket,
    #[new]
    fn py_new(name: &str) -> PyResult<Self> {
        Ok(Self::new(name))
    }
);

#[typetag::serde]
impl Socket for ThreadChannelSocket {
    fn is_open(&self) -> bool {
        self.endpoint.is_some()
    }

    fn open(&mut self, ctx: &ControllerCtx) -> Result<(), String> {
        self.endpoint = Some(ctx.source_endpoint(&self.name));
        info!(
            "Opened thread channel socket on user channel {}",
            &self.name
        );
        Ok(())
    }

    fn close(&mut self) {
        self.endpoint = None;
        info!(
            "Closed thread channel socket on user channel {}",
            &self.name
        );
    }

    fn send(&mut self, id: PeripheralId, msg: &[u8]) -> Result<(), String> {
        let endpoint = self
            .endpoint
            .as_ref()
            .ok_or_else(|| "Unable to send before socket is open".to_string())?;
        let mut buf = vec![0u8; PeripheralId::BYTE_LEN + msg.len()];
        id.write_bytes(&mut buf[..PeripheralId::BYTE_LEN]);
        buf[PeripheralId::BYTE_LEN..].copy_from_slice(msg);
        endpoint
            .tx()
            .send(Msg::Packet(buf))
            .map_err(|e| format!("Failed to send user channel packet: {e}"))
    }

    fn recv_into(&mut self, buf: &mut [u8], timeout: Duration) -> Option<SocketPacketMeta> {
        let endpoint = self.endpoint.as_ref()?;
        let msg = if timeout.is_zero() {
            endpoint.rx().try_recv().ok()?
        } else {
            endpoint.rx().recv_timeout(timeout).ok()?
        };
        match msg {
            Msg::Packet(bytes) => {
                if bytes.len() < PeripheralId::BYTE_LEN {
                    return None;
                }
                let pid = PeripheralId::read_bytes(&bytes[..PeripheralId::BYTE_LEN]);
                let payload = &bytes[PeripheralId::BYTE_LEN..];
                let size = payload.len().min(buf.len());
                buf[..size].copy_from_slice(&payload[..size]);
                Some(SocketPacketMeta {
                    pid: Some(pid),
                    token: 0,
                    time: Instant::now(),
                    size,
                })
            }
            _ => None,
        }
    }

    fn broadcast(&mut self, msg: &[u8]) -> Result<(), String> {
        // Single channel: best-effort broadcast by sending once.
        self.send(PeripheralId::default(), msg)
    }

    fn update_map(&mut self, _id: PeripheralId, _token: SocketAddrToken) -> Result<(), String> {
        Ok(())
    }
}
