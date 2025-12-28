//! Implementation of Socket trait for stdlib UDP socket on IPV4

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;
use tracing::info;

use crate::buffer_pool::{BufferPool, SOCKET_BUFFER_LEN, SocketBuffer, default_socket_buffer_pool};
use crate::controller::context::ControllerCtx;
use crate::{LoopMethod, py_json_methods};

use super::*;
use deimos_shared::peripherals::PeripheralId;
use deimos_shared::{CONTROLLER_RX_PORT, PERIPHERAL_RX_PORT};

/// Implementation of Socket trait for stdlib UDP socket on IPV4
#[derive(Serialize, Deserialize, Default)]
#[cfg_attr(feature = "python", pyclass)]
pub struct UdpSocket {
    #[serde(skip)]
    socket: Option<std::net::UdpSocket>,
    #[serde(skip)]
    buffer_pool: Option<BufferPool<SocketBuffer>>,
    #[serde(skip)]
    nonblocking: bool,
    #[serde(skip)]
    addrs: BTreeMap<PeripheralId, std::net::SocketAddr>,
    #[serde(skip)]
    pids: BTreeMap<std::net::SocketAddr, PeripheralId>,
    #[serde(skip)]
    addr_tokens: BTreeMap<std::net::SocketAddr, SocketAddrToken>,
    #[serde(skip)]
    token_addrs: BTreeMap<SocketAddrToken, std::net::SocketAddr>,
    #[serde(skip)]
    next_addr_token: SocketAddrToken,
}

impl UdpSocket {
    pub fn new() -> Self {
        Self {
            socket: None,
            buffer_pool: None,
            nonblocking: false,
            addrs: BTreeMap::new(),
            pids: BTreeMap::new(),
            addr_tokens: BTreeMap::new(),
            token_addrs: BTreeMap::new(),
            next_addr_token: 0,
        }
    }
}

py_json_methods!(
    UdpSocket,
    Socket,
    #[new]
    fn py_new() -> PyResult<Self> {
        Ok(Self::new())
    }
);

#[typetag::serde]
impl Socket for UdpSocket {
    fn is_open(&self) -> bool {
        self.socket.is_some()
    }

    fn open(&mut self, ctx: &ControllerCtx) -> Result<(), String> {
        // If it's already open, close it and reopen to clear settings
        if self.socket.is_some() {
            self.close();
        }

        // Set up a fresh buffer pool
        self.buffer_pool = Some(default_socket_buffer_pool());
        self.nonblocking = false;

        // Inner socket populated on access
        let addr = format!("0.0.0.0:{CONTROLLER_RX_PORT}");
        let socket = std::net::UdpSocket::bind(&addr)
            .map_err(|e| format!("Unable to bind UDP socket: {e}"))?;

        // Set nonblocking if we're in Performant mode and will be busywaiting
        if matches!(ctx.loop_method, LoopMethod::Performant) {
            socket
                .set_nonblocking(true)
                .map_err(|e| format!("Failed to set socket to nonblocking mode: {e}"))?;
            self.nonblocking = true;
        }

        // Done
        self.socket = Some(socket);
        info!("Opened controller UDP socket at {addr:?}");

        Ok(())
    }

    fn close(&mut self) {
        // Drop inner socket, releasing port
        self.socket = None;
        self.buffer_pool = None;
        self.nonblocking = false;
        self.addrs.clear();
        self.pids.clear();
        self.addr_tokens.clear();
        self.token_addrs.clear();
        self.next_addr_token = 0;
        info!("Closed controller UDP socket at 0.0.0.0:{CONTROLLER_RX_PORT}");
    }

    fn send(&mut self, id: PeripheralId, msg: &[u8]) -> Result<(), String> {
        // Get the IP address
        let addr = *self
            .addrs
            .get(&id)
            .ok_or(format!("Peripheral not present in address map: {id:?}"))?;

        // Get socket
        let sock = self
            .socket
            .as_mut()
            .ok_or("Unable to send before socket is bound".to_string())?;

        // Send unicast
        sock.send_to(msg, addr)
            .map_err(|e| format!("Failed to send UDP packet: {e}"))?;

        Ok(())
    }

    fn recv(&mut self, timeout: Duration) -> Option<SocketPacket> {
        // Check if there is anything to receive,
        // and filter out packets from unexpected source port
        let sock = self.socket.as_mut()?;
        let pool = self.buffer_pool.as_ref()?;
        let mut lease = pool.lease_or_create(|| Box::new([0u8; SOCKET_BUFFER_LEN]));
        if timeout.is_zero() {
            if !self.nonblocking {
                // Avoid blocking forever when polling with zero timeout.
                let _ = sock.set_read_timeout(Some(Duration::from_nanos(1)));
            }
        } else {
            let _ = sock.set_read_timeout(Some(timeout)); // Guaranteed nonzero duration
        }

        let (size, addr, time) = match {
            let buf = lease.as_mut();
            sock.recv_from(&mut buf[..])
        } {
            Ok((size, addr)) => {
                // Mark the time ASAP
                let now = Instant::now();
                // Make sure the source port is consistent with a peripheral
                if addr.port() != PERIPHERAL_RX_PORT {
                    return None;
                }
                (size, addr, now)
            }
            Err(_) => return None, // Nothing to receive yet
        };

        // Update address index map
        let token = match self.addr_tokens.get(&addr).copied() {
            Some(token) => token,
            None => {
                let token = self.next_addr_token;
                self.next_addr_token = self.next_addr_token.wrapping_add(1);
                self.addr_tokens.insert(addr, token);
                self.token_addrs.insert(token, addr);
                token
            }
        };

        // Check if we already know which peripheral this is
        let pid = self.pids.get(&addr).copied();

        Some(SocketPacket {
            pid,
            token,
            time,
            buffer: lease,
            size,
        })
    }

    fn broadcast(&mut self, msg: &[u8]) -> Result<(), String> {
        // Get socket
        let sock = self
            .socket
            .as_mut()
            .ok_or("Unable to send before socket is bound".to_string())?;

        // Send broadcast
        sock.set_broadcast(true)
            .map_err(|e| format!("Unable to set UDP socket to broadcast mode: {e}"))?;
        sock.send_to(msg, (Ipv4Addr::BROADCAST, PERIPHERAL_RX_PORT))
            .map_err(|e| format!("Failed to send UDP packet: {e}"))?;

        // Set back to unicast mode
        sock.set_broadcast(false)
            .map_err(|e| format!("Unable to set UDP socket to unicast mode: {e}"))?;

        Ok(())
    }

    fn update_map(&mut self, id: PeripheralId, token: SocketAddrToken) -> Result<(), String> {
        if let Some(addr) = self.token_addrs.get(&token).copied() {
            self.addrs.insert(id, addr);
            self.pids.insert(addr, id);

            // Check for duplicate addresses, which can happen in a mac address collision.
            // If there are duplicate addresses _or_ duplicate IDs, the number of entries
            // in the maps won't match.
            if self.addrs.len() != self.pids.len() {
                return Err(format!(
                    "Duplicate addresses or peripheral IDs detected.\nAddress map: {:?}\nPeripheral ID map: {:?}",
                    &self.addrs, &self.pids
                ));
            }
        } else {
            return Err(format!("Unknown address token {token}"));
        }

        Ok(())
    }
}
