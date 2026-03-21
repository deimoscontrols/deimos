//! Implementation of Socket trait for stdlib UDP socket on IPV4

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;
use tracing::info;

use crate::controller::context::ControllerCtx;
use crate::{LoopMethod, py_json_methods};

use super::*;
use deimos_shared::peripherals::PeripheralId;
use deimos_shared::{CONTROLLER_RX_PORT, PERIPHERAL_RX_PORT};

fn broadcast_from_addr_and_mask(addr: Ipv4Addr, netmask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(addr) | !u32::from(netmask))
}

fn dedup_broadcast_targets(targets: impl IntoIterator<Item = Ipv4Addr>) -> Vec<Ipv4Addr> {
    BTreeSet::from_iter(targets).into_iter().collect()
}

/// Enumerate the IPv4 broadcast addresses that this host can plausibly use for
/// UDP peripheral discovery.
///
/// This always includes `255.255.255.255` as a fallback, then adds any
/// directed broadcast addresses that can be derived from visible non-loopback
/// IPv4 interfaces.
pub fn possible_broadcast_targets() -> Vec<Ipv4Addr> {
    // Always include the limited broadcast address as a lowest-common-denominator fallback.
    let mut targets = BTreeSet::from([Ipv4Addr::BROADCAST]);

    // Query the OS for all visible interfaces using a cross-platform safe wrapper.
    let interfaces = match NetworkInterface::show() {
        Ok(interfaces) => interfaces,
        Err(_) => return targets.into_iter().collect(),
    };

    for interface in interfaces {
        for addr in interface.addr {
            // Only IPv4 addresses participate in the current UDP peripheral transport.
            let ip = match addr.ip() {
                IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => ip,
                _ => continue,
            };

            // Prefer an OS-reported directed broadcast address, but compute one from
            // the IPv4 netmask when the platform only reports address + mask.
            let target = match addr.broadcast() {
                Some(IpAddr::V4(broadcast)) if broadcast != ip && !broadcast.is_unspecified() => {
                    Some(broadcast)
                }
                _ => match addr.netmask() {
                    Some(IpAddr::V4(netmask)) => {
                        let broadcast = broadcast_from_addr_and_mask(ip, netmask);
                        (broadcast != ip && !broadcast.is_unspecified()).then_some(broadcast)
                    }
                    _ => None,
                },
            };

            if let Some(target) = target {
                targets.insert(target);
            }
        }
    }

    targets.into_iter().collect()
}

/// Implementation of [`Socket`] trait for stdlib UDP sockets on IPv4.
///
/// By default, discovery broadcasts are sent to every local IPv4 broadcast
/// domain that can be enumerated from the host network interfaces, plus the
/// limited broadcast address `255.255.255.255` as a fallback. This allows the
/// controller to scan across multiple attached subnets without assuming that a
/// single limited broadcast will egress on every interface.
///
/// To force discovery traffic onto a specific interface, construct the socket
/// with [`UdpSocket::with_broadcast_targets`] and provide that interface's
/// directed broadcast address, such as `192.168.10.255` for a `/24` network on
/// `192.168.10.0/24`. In that mode, discovery packets are sent only to the
/// provided broadcast address or addresses rather than the auto-enumerated set.
#[derive(Serialize, Deserialize, Default)]
#[cfg_attr(feature = "python", pyclass)]
pub struct UdpSocket {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    broadcast_targets: Vec<Ipv4Addr>,
    #[serde(skip)]
    socket: Option<std::net::UdpSocket>,
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
            // An empty list means "auto-detect from local interfaces" at send time.
            broadcast_targets: Vec::new(),
            socket: None,
            nonblocking: false,
            addrs: BTreeMap::new(),
            pids: BTreeMap::new(),
            addr_tokens: BTreeMap::new(),
            token_addrs: BTreeMap::new(),
            next_addr_token: 0,
        }
    }

    /// Create a UDP socket that broadcasts only to the provided IPv4 targets.
    pub fn with_broadcast_targets(targets: Vec<Ipv4Addr>) -> Self {
        Self {
            broadcast_targets: dedup_broadcast_targets(targets),
            ..Self::new()
        }
    }

    fn broadcast_targets(&self) -> Vec<Ipv4Addr> {
        if self.broadcast_targets.is_empty() {
            // Default behavior is to fan out discovery onto every local IPv4
            // broadcast domain we can identify.
            possible_broadcast_targets()
        } else {
            // Callers can pin discovery to a specific set of broadcast addresses.
            self.broadcast_targets.clone()
        }
    }
}

py_json_methods!(
    UdpSocket,
    Socket,
    #[new]
    fn py_new() -> PyResult<Self> {
        Ok(Self::new())
    },
    #[staticmethod]
    #[pyo3(name = "with_broadcast_targets")]
    fn py_with_broadcast_targets(targets: Vec<String>) -> PyResult<Self> {
        let targets = targets
            .into_iter()
            .map(|target| {
                target.parse::<Ipv4Addr>().map_err(|e| {
                    pyo3::exceptions::PyValueError::new_err(format!(
                        "Invalid IPv4 broadcast target `{target}`: {e}"
                    ))
                })
            })
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Self::with_broadcast_targets(targets))
    },
    #[staticmethod]
    #[pyo3(name = "possible_broadcast_targets")]
    fn py_possible_broadcast_targets() -> Vec<String> {
        possible_broadcast_targets()
            .into_iter()
            .map(|addr| addr.to_string())
            .collect()
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

    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Option<SocketPacketMeta> {
        // Check if there is anything to receive,
        // and filter out packets from unexpected source port
        let sock = self.socket.as_mut()?;
        if timeout.is_zero() {
            if !self.nonblocking {
                // Avoid blocking forever when polling with zero timeout.
                let _ = sock.set_read_timeout(Some(Duration::from_nanos(1)));
            }
        } else {
            let _ = sock.set_read_timeout(Some(timeout)); // Guaranteed nonzero duration
        }

        let (size, addr, time) = match sock.recv_from(buf) {
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

        Some(SocketPacketMeta {
            pid,
            token,
            time,
            size,
        })
    }

    fn broadcast(&mut self, msg: &[u8]) -> Result<(), String> {
        // Resolve the target list up front so the mutable socket borrow stays simple.
        let targets = self.broadcast_targets();

        // Get socket
        let sock = self
            .socket
            .as_mut()
            .ok_or("Unable to send before socket is bound".to_string())?;

        // Send broadcast
        sock.set_broadcast(true)
            .map_err(|e| format!("Unable to set UDP socket to broadcast mode: {e}"))?;
        let mut sent_any = false;
        let mut errors = Vec::new();
        // Send one packet per broadcast domain instead of relying on a single
        // limited broadcast to reach every attached interface.
        for target in targets {
            match sock.send_to(msg, (target, PERIPHERAL_RX_PORT)) {
                Ok(_) => sent_any = true,
                Err(e) => errors.push(format!("{target}: {e}")),
            }
        }

        // Set back to unicast mode
        sock.set_broadcast(false)
            .map_err(|e| format!("Unable to set UDP socket to unicast mode: {e}"))?;

        if sent_any {
            Ok(())
        } else {
            Err(format!(
                "Failed to send UDP packet on any broadcast target: {}",
                errors.join("; ")
            ))
        }
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn broadcast_address_uses_interface_mask() {
        let addr = Ipv4Addr::new(10, 42, 7, 9);
        let netmask = Ipv4Addr::new(255, 255, 255, 0);
        assert_eq!(
            broadcast_from_addr_and_mask(addr, netmask),
            Ipv4Addr::new(10, 42, 7, 255)
        );
    }

    #[test]
    fn explicit_broadcast_targets_are_deduplicated() {
        let socket = UdpSocket::with_broadcast_targets(vec![
            Ipv4Addr::new(192, 168, 1, 255),
            Ipv4Addr::new(192, 168, 1, 255),
            Ipv4Addr::new(10, 0, 0, 255),
        ]);
        assert_eq!(
            socket.broadcast_targets(),
            vec![
                Ipv4Addr::new(10, 0, 0, 255),
                Ipv4Addr::new(192, 168, 1, 255),
            ]
        );
    }

    #[test]
    fn possible_broadcast_targets_include_limited_broadcast() {
        assert!(possible_broadcast_targets().contains(&Ipv4Addr::BROADCAST));
    }
}
