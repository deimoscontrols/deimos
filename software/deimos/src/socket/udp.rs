//! Implementation of Socket trait for stdlib UDP socket

use std::collections::BTreeMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::*;
use deimos_shared::peripherals::PeripheralId;
use deimos_shared::{CONTROLLER_RX_PORT, PERIPHERAL_RX_PORT};

#[derive(Serialize, Deserialize, Default)]
pub struct UdpSuperSocket {
    #[serde(skip)]
    socket: Option<UdpSocket>,
    #[serde(skip)]
    txbuf: Vec<u8>,
    #[serde(skip)]
    rxbuf: Vec<u8>,
    #[serde(skip)]
    addrs: BTreeMap<PeripheralId, SocketAddr>,
    #[serde(skip)]
    pids: BTreeMap<SocketAddr, PeripheralId>,
    #[serde(skip)]
    last_received_addr: Option<SocketAddr>,
}

impl UdpSuperSocket {
    pub fn new() -> Self {
        Self {
            txbuf: vec![0; 1522],
            rxbuf: vec![0; 1522],
            socket: None,
            addrs: BTreeMap::new(),
            pids: BTreeMap::new(),
            last_received_addr: None,
        }
    }
}

#[typetag::serde]
impl SuperSocket for UdpSuperSocket {
    fn open(&mut self) -> Result<(), String> {
        if self.socket.is_none() {
            // Socket populated on access
            let socket = UdpSocket::bind(format!("0.0.0.0:{CONTROLLER_RX_PORT}"))
                .map_err(|e| format!("Unable to bind UDP socket: {e}"))?;
            socket
                .set_nonblocking(true)
                .map_err(|e| format!("Unable to set UDP socket to nonblocking mode: {e}"))?;
            self.socket = Some(socket);
        }
        else {
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

    fn send(&mut self, id: PeripheralId, w: &PacketWriter) -> Result<(), String> {
        // Make sure the socket is open
        self.open();

        // Get the IP address
        let addr = *self
            .addrs
            .get(&id)
            .ok_or(format!("Peripheral not present in address map: {id:?}"))?;

        // Get socket
        let sock = self
            .socket
            .as_mut()
            .ok_or(format!("Unable to send before socket is bound"))?;

        // Write bytes to buffer
        let num_to_send = w(&mut self.txbuf)?;
        let txbuf = &mut self.txbuf[..num_to_send];

        // Send unicast
        sock.send_to(txbuf, addr)
            .map_err(|e| format!("Failed to send UDP packet: {e}"))?;

        Ok(())
    }

    fn recv(&mut self) -> Option<(Instant, &[u8])> {
        // Make sure the socket is open
        self.open();

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

        Some((time, &self.rxbuf[..size]))
    }

    fn broadcast(&mut self, w: &PacketWriter) -> Result<(), String> {
        // Make sure the socket is open
        self.open();

        // Get socket
        let sock = self
            .socket
            .as_mut()
            .ok_or(format!("Unable to send before socket is bound"))?;

        // Write bytes to buffer
        let num_to_send = w(&mut self.txbuf)?;
        let txbuf = &mut self.txbuf[..num_to_send];

        // Send broadcast
        sock.set_broadcast(true)
            .map_err(|e| format!("Unable to set UDP socket to broadcast mode: {e}"))?;
        sock.send(txbuf)
            .map_err(|e| format!("Failed to send UDP packet: {e}"))?;

        // Set back to unicast mode
        sock.set_broadcast(false)
            .map_err(|e| format!("Unable to set UDP socket to unicast mode: {e}"))?;

        Ok(())
    }

    fn update_map(&mut self, id: PeripheralId) {
        if let Some(addr) = self.last_received_addr {
            self.addrs.insert(id, addr);
        }
    }
}
