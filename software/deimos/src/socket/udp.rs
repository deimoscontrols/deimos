//! Implementation of Socket trait for stdlib UDP socket on IPV4

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::controller::context::ControllerCtx;

use super::*;
use deimos_shared::peripherals::PeripheralId;
use deimos_shared::{CONTROLLER_RX_PORT, PERIPHERAL_RX_PORT};

/// Implementation of Socket trait for stdlib UDP socket on IPV4
#[derive(Serialize, Deserialize, Default)]
pub struct UdpSocket {
    #[serde(skip)]
    socket: Option<std::net::UdpSocket>,
    #[serde(skip)]
    rxbuf: Vec<u8>,
    #[serde(skip)]
    addrs: BTreeMap<PeripheralId, std::net::SocketAddr>,
    #[serde(skip)]
    pids: BTreeMap<std::net::SocketAddr, PeripheralId>,
    #[serde(skip)]
    last_received_addr: Option<std::net::SocketAddr>,
}

impl UdpSocket {
    pub fn new() -> Self {
        Self {
            rxbuf: vec![0; 1522],
            socket: None,
            addrs: BTreeMap::new(),
            pids: BTreeMap::new(),
            last_received_addr: None,
        }
    }
}

#[typetag::serde]
impl Socket for UdpSocket {
    fn is_open(&self) -> bool {
        self.socket.is_some()
    }

    fn open(&mut self, _ctx: &ControllerCtx) -> Result<(), String> {
        if self.socket.is_none() {
            // Socket populated on access
            let socket = std::net::UdpSocket::bind(format!("0.0.0.0:{CONTROLLER_RX_PORT}"))
                .map_err(|e| format!("Unable to bind UDP socket: {e}"))?;
            socket
                .set_nonblocking(true)
                .map_err(|e| format!("Unable to set UDP socket to nonblocking mode: {e}"))?;
            self.socket = Some(socket);
        } else {
            // If the socket is already open, do nothing
        }

        Ok(())
    }

    fn close(&mut self) {
        // Drop inner socket, releasing port
        self.socket = None;
        self.addrs.clear();
        self.pids.clear();
        self.last_received_addr = None;
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

    fn recv(&mut self) -> Option<(Option<PeripheralId>, Instant, &[u8])> {
        // Check if there is anything to receive,
        // and filter out packets from unexpected source port
        let (size, addr, time) = match self.socket.as_mut() {
            Some(sock) => match sock.recv_from(&mut self.rxbuf).ok() {
                Some((size, addr)) => {
                    // Mark the time ASAP
                    let now = Instant::now();
                    // Make sure the source port is consistent with a peripheral
                    if addr.port() != PERIPHERAL_RX_PORT {
                        return None;
                    }
                    (size, addr, now)
                }
                None => return None,
            },
            None => return None,
        };

        self.last_received_addr = Some(addr);

        // Check if we already know which peripheral this is
        let pid = self.pids.get(&addr).copied();

        Some((pid, time, &self.rxbuf[..size]))
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

    fn update_map(&mut self, id: PeripheralId) -> Result<(), String> {
        if let Some(addr) = self.last_received_addr {
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
        }

        Ok(())
    }
}
