//! Socket implementation backed by a controller user channel.

use std::time::Instant;

use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;
use tracing::info;

use crate::controller::channel::{Endpoint, Msg};
use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

use super::Socket;
use deimos_shared::peripherals::PeripheralId;
use deimos_shared::states::{ByteStruct, ByteStructLen};

/// Socket implementation that communicates over a named user channel.
#[derive(Serialize, Deserialize, Default)]
#[cfg_attr(feature = "python", pyclass)]
pub struct ThreadChannelSocket {
    name: String,
    #[serde(skip)]
    endpoint: Option<Endpoint>,
    #[serde(skip)]
    rxbuf: Vec<u8>,
}

impl ThreadChannelSocket {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            endpoint: None,
            rxbuf: Vec::new(),
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
        self.rxbuf.clear();
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

    fn recv(&mut self) -> Option<(Option<PeripheralId>, Instant, &[u8])> {
        let endpoint = self.endpoint.as_ref()?;
        let msg = endpoint.rx().try_recv().ok()?;
        match msg {
            Msg::Packet(bytes) => {
                if bytes.len() < PeripheralId::BYTE_LEN {
                    return None;
                }
                let pid = PeripheralId::read_bytes(&bytes[..PeripheralId::BYTE_LEN]);
                self.rxbuf.clear();
                self.rxbuf
                    .extend_from_slice(&bytes[PeripheralId::BYTE_LEN..]);
                Some((Some(pid), Instant::now(), &self.rxbuf))
            }
            _ => None,
        }
    }

    fn broadcast(&mut self, msg: &[u8]) -> Result<(), String> {
        // Single channel: best-effort broadcast by sending once.
        self.send(PeripheralId::default(), msg)
    }

    fn update_map(&mut self, _id: PeripheralId) -> Result<(), String> {
        Ok(())
    }
}
