use smoltcp::{
    iface::{Config, Interface, SocketSet, SocketStorage},
    phy::{self, DeviceCapabilities, TxToken as _},
    socket::{dhcpv4, udp},
    storage::{PacketBuffer, PacketMetadata},
    time::Instant,
    wire::{
        ArpOperation, ArpPacket, ArpRepr, EthernetAddress, EthernetFrame, EthernetProtocol,
        IpListenEndpoint, Ipv4Address, Ipv4Cidr,
    },
};
use stm32h7xx_hal::ethernet;

use deimos_shared::{
    static_fallback_ipv4_candidate_from_mac, PERIPHERAL_RX_PORT, STATIC_FALLBACK_CANDIDATE_COUNT,
    STATIC_FALLBACK_IPV4_PREFIX_LEN,
};

mod arp_scraper;
use arp_scraper::{ObservedDevice, IpAssignment, PendingDhcpConfig, static_fallback_cidr, fallback_backoff_ns};
pub(crate) use arp_scraper::IpConfigStatus;

/// Length of the post-claim conflict observation window for a tentative fallback address.
const FALLBACK_VALIDATION_NS: i64 = 250_000_000;


/// Socket storage borrowed by [`Net`] for the lifetime of the firmware.
pub(crate) struct NetStorageStatic<'a> {
    pub(crate) socket_storage: [SocketStorage<'a>; 8],
}

// TODO: move these to startup and initialize with MaybeUninit
static mut RX_METADATA_STORAGE: [PacketMetadata<udp::UdpMetadata>; 4] = [PacketMetadata::EMPTY; 4];
static mut RX_PAYLOAD_STORAGE: [u8; 1522] = [0u8; 1522];

static mut TX_METADATA_STORAGE: [PacketMetadata<udp::UdpMetadata>; 4] = [PacketMetadata::EMPTY; 4];
static mut TX_PAYLOAD_STORAGE: [u8; 1522] = [0u8; 1522];

/// Owns the Ethernet interface, sockets, and IPv4 configuration state for the board.
pub(crate) struct Net<'a> {
    iface: Interface,
    ethdev: ObservedDevice<ethernet::EthernetDMA<4, 4>>,
    sockets: SocketSet<'a>,
    udp_handle: smoltcp::iface::SocketHandle,
    dhcp_handle: smoltcp::iface::SocketHandle,
    ip_assignment: IpAssignment,
    pending_dhcp: Option<PendingDhcpConfig>,
    next_fallback_candidate: u8,
    fallback_failure_rounds: u8,
    fallback_backoff_until_ns: Option<i64>,
}
impl<'a> Net<'a> {
    /// Build the Ethernet interface, UDP socket, DHCP socket, and fallback IP state machine.
    pub(crate) fn new(
        store: &'a mut NetStorageStatic<'a>,
        ethdev: ethernet::EthernetDMA<4, 4>,
        ethernet_addr: EthernetAddress,
        now: Instant,
    ) -> Self {
        // Wrap the DMA device so fallback ARP traffic can be inspected and injected.
        let mut ethdev = ObservedDevice::new(ethdev, ethernet_addr);
        let config = Config::new(ethernet_addr.into());
        let iface = Interface::new(config, &mut ethdev, now);

        // Reserve socket slots up front because the firmware keeps them for its full lifetime.
        let mut sockets = SocketSet::new(&mut store.socket_storage[..]);

        // Add the UDP command/data socket used by the controller.
        let rx_packet_buffer =
            unsafe { PacketBuffer::new(&mut RX_METADATA_STORAGE[..], &mut RX_PAYLOAD_STORAGE[..]) };
        let tx_packet_buffer =
            unsafe { PacketBuffer::new(&mut TX_METADATA_STORAGE[..], &mut TX_PAYLOAD_STORAGE[..]) };

        let mut udp_socket = udp::Socket::new(rx_packet_buffer, tx_packet_buffer);
        udp_socket
            .bind(IpListenEndpoint {
                addr: None,
                port: PERIPHERAL_RX_PORT,
            })
            .unwrap();
        let udp_handle = sockets.add(udp_socket);

        // Add a DHCP client socket for dynamic IPv4 configuration when available.
        let dhcp_socket = dhcpv4::Socket::new();
        let dhcp_handle: smoltcp::iface::SocketHandle = sockets.add(dhcp_socket);

        // Start unconfigured and let the fallback/DHCP state machine claim an address later.
        Net::<'a> {
            iface,
            ethdev,
            sockets,
            udp_handle,
            dhcp_handle,
            ip_assignment: IpAssignment::Unconfigured,
            pending_dhcp: None,
            next_fallback_candidate: 0,
            fallback_failure_rounds: 0,
            fallback_backoff_until_ns: None,
        }
    }

    /// Polls on the ethernet interface.
    ///
    /// If polled at the same `time_ns` multiple times, this will process
    /// incoming UDP packets for the UDP socket, but will not advance the
    /// DHCP state machine. This can reduce timing uncertainty under
    /// repeated polls.
    pub(crate) fn poll(&mut self, time_ns: i64) -> bool {
        let timestamp = Instant::from_micros(time_ns / 1000);
        self.iface
            .poll(timestamp, &mut self.ethdev, &mut self.sockets)
    }

    /// Receive one UDP packet directly from the socket buffer.
    pub(crate) fn udp_recv(&mut self) -> Result<(&[u8], udp::UdpMetadata), udp::RecvError> {
        self.sockets.get_mut::<udp::Socket>(self.udp_handle).recv()
    }

    /// Enqueue one UDP packet by writing directly into the socket transmit buffer.
    pub(crate) fn udp_send_with<F>(
        &mut self,
        max_size: usize,
        meta: impl Into<udp::UdpMetadata>,
        f: F,
    ) -> Result<usize, udp::SendError>
    where
        F: FnOnce(&mut [u8]) -> usize,
    {
        self.sockets
            .get_mut::<udp::Socket>(self.udp_handle)
            .send_with(max_size, meta, f)
    }

    /// Close and rebind the board's UDP socket to its standard listen endpoint.
    pub(crate) fn reset_udp_socket(&mut self) {
        let socket = self.sockets.get_mut::<udp::Socket>(self.udp_handle);
        socket.close();
        socket
            .bind(IpListenEndpoint {
                addr: None,
                port: PERIPHERAL_RX_PORT,
            })
            .unwrap();
    }

    /// Remove any configured IPv4 address, route, and tentative fallback watch state.
    fn clear_ipv4_config(&mut self) {
        clear_ipv4_addr(&mut self.iface);
        self.end_tentative_watch();
        self.iface.routes_mut().remove_default_ipv4_route();
        self.ip_assignment = IpAssignment::Unconfigured;
    }

    /// Reset fallback candidate selection and backoff after a successful address transition.
    fn reset_fallback_progress(&mut self) {
        self.next_fallback_candidate = 0;
        self.fallback_failure_rounds = 0;
        self.fallback_backoff_until_ns = None;
    }

    /// Promote a tentative fallback address to a stable link-local fallback assignment.
    fn promote_tentative_fallback(&mut self, cidr: Ipv4Cidr) {
        self.end_tentative_watch();
        self.ip_assignment = IpAssignment::LinkLocalFallback(cidr);
        self.reset_fallback_progress();
    }

    /// Record a fallback conflict and advance to the next candidate or backoff interval.
    fn note_fallback_conflict(&mut self, time_ns: i64, candidate_index: u8) {
        // Drop the tentative claim immediately before choosing what to try next.
        self.clear_ipv4_config();
        if (candidate_index as usize + 1) < STATIC_FALLBACK_CANDIDATE_COUNT {
            // Stay in the current round and move to the next deterministic candidate.
            self.next_fallback_candidate = candidate_index + 1;
            self.fallback_backoff_until_ns = None;
        } else {
            // After exhausting the round, wait before retrying from the first candidate again.
            self.next_fallback_candidate = 0;
            self.fallback_failure_rounds = self.fallback_failure_rounds.saturating_add(1);
            self.fallback_backoff_until_ns =
                Some(time_ns + fallback_backoff_ns(self.fallback_failure_rounds));
        }
    }

    /// Apply a DHCP-provided IPv4 address and optional default route immediately.
    fn apply_dhcp_config(&mut self, config: PendingDhcpConfig) {
        // Replace any fallback state with the DHCP-provided address.
        set_ipv4_addr(&mut self.iface, config.address);
        if let Some(router) = config.router {
            self.iface
                .routes_mut()
                .add_default_ipv4_route(router)
                .unwrap();
        } else {
            self.iface.routes_mut().remove_default_ipv4_route();
        }
        // Clear fallback bookkeeping now that DHCP is authoritative.
        self.end_tentative_watch();
        self.ip_assignment = IpAssignment::Dhcp(config.address);
        self.pending_dhcp = None;
        self.reset_fallback_progress();
    }

    /// Start watching a tentative IPv4 fallback address for ARP conflicts.
    fn begin_tentative_watch(&mut self, ip: Ipv4Address) {
        self.ethdev.set_monitored_ip(Some(ip));
    }

    /// Stop ARP conflict monitoring for a tentative IPv4 fallback address.
    fn end_tentative_watch(&mut self) {
        self.ethdev.set_monitored_ip(None);
    }

    /// Returns true if a conflicting ARP probe or announcement was observed for the active tentative address.
    fn take_tentative_conflict(&mut self) -> bool {
        self.ethdev.take_conflict()
    }

    /// Send one ARP probe for the target fallback address.
    fn send_arp_probe(&mut self, time_ns: i64, target_ip: Ipv4Address) -> bool {
        self.ethdev
            .send_arp_probe(Instant::from_micros(time_ns / 1000), target_ip)
    }

    /// Send one gratuitous ARP after claiming the fallback address.
    fn send_gratuitous_arp(&mut self, time_ns: i64, claimed_ip: Ipv4Address) -> bool {
        self.ethdev
            .send_gratuitous_arp(Instant::from_micros(time_ns / 1000), claimed_ip)
    }

    /// Return true if a new fallback candidate can be claimed immediately.
    fn fallback_attempt_ready(&self, time_ns: i64) -> bool {
        match self.fallback_backoff_until_ns {
            Some(retry_time_ns) => time_ns >= retry_time_ns,
            None => true,
        }
    }

    /// Return true if there are more deterministic fallback candidates left in the current round.
    fn has_more_fallback_candidates(&self) -> bool {
        (self.next_fallback_candidate as usize) < STATIC_FALLBACK_CANDIDATE_COUNT
    }

    /// Claim the next deterministic fallback candidate immediately and begin conflict observation.
    pub(crate) fn try_claim_fallback(&mut self, time_ns: i64, mac: [u8; 6]) -> bool {
        if !self.fallback_attempt_ready(time_ns) || !self.has_more_fallback_candidates() {
            return false;
        }

        // Pick the next MAC-derived fallback candidate and install it as tentative.
        let candidate_index = self.next_fallback_candidate as usize;
        let cidr = static_fallback_cidr(mac, candidate_index);
        set_ipv4_addr(&mut self.iface, cidr);
        self.iface.routes_mut().remove_default_ipv4_route();
        self.begin_tentative_watch(cidr.address());
        self.ip_assignment = IpAssignment::TentativeLinkLocal {
            cidr,
            candidate_index: candidate_index as u8,
            validation_deadline_ns: time_ns + FALLBACK_VALIDATION_NS,
        };
        // Probe and announce once, then watch for conflicting ARP traffic during validation.
        let _ = self.send_arp_probe(time_ns, cidr.address());
        let _ = self.send_gratuitous_arp(time_ns, cidr.address());
        true
    }

    /// Poll DHCP and reconcile the active IPv4 configuration.
    pub(crate) fn poll_ip_config(&mut self, time_ns: i64, allow_dhcp_swap: bool) -> IpConfigStatus {
        // First, decide whether the current tentative fallback claim survived its validation window.
        if let IpAssignment::TentativeLinkLocal {
            cidr,
            candidate_index,
            validation_deadline_ns,
        } = self.ip_assignment
        {
            if self.take_tentative_conflict() {
                self.note_fallback_conflict(time_ns, candidate_index);
                return IpConfigStatus::Missing;
            }

            if time_ns >= validation_deadline_ns {
                self.promote_tentative_fallback(cidr);
            }
        }

        // Apply a deferred DHCP lease only when the caller allows mid-connection address swaps.
        if allow_dhcp_swap {
            if let Some(config) = self.pending_dhcp.take() {
                self.apply_dhcp_config(config);
                return IpConfigStatus::DhcpApplied;
            }
        }

        // Poll the DHCP socket for lease changes and merge them into the state machine.
        let event = self
            .sockets
            .get_mut::<dhcpv4::Socket>(self.dhcp_handle)
            .poll();

        match event {
            Some(dhcpv4::Event::Configured(config)) => {
                let config = PendingDhcpConfig {
                    address: config.address,
                    router: config.router,
                };
                match self.ip_assignment {
                    IpAssignment::LinkLocalFallback(_)
                    | IpAssignment::TentativeLinkLocal { .. }
                        if !allow_dhcp_swap =>
                    {
                        self.pending_dhcp = Some(config);
                        IpConfigStatus::DhcpDeferred
                    }
                    IpAssignment::Dhcp(_) => {
                        self.apply_dhcp_config(config);
                        IpConfigStatus::Ready
                    }
                    _ => {
                        self.apply_dhcp_config(config);
                        IpConfigStatus::DhcpApplied
                    }
                }
            }
            Some(dhcpv4::Event::Deconfigured) => {
                self.pending_dhcp = None;
                match self.ip_assignment {
                    IpAssignment::Dhcp(_) => {
                        self.clear_ipv4_config();
                        IpConfigStatus::Missing
                    }
                    IpAssignment::LinkLocalFallback(_)
                    | IpAssignment::TentativeLinkLocal { .. } => IpConfigStatus::Ready,
                    IpAssignment::Unconfigured => {
                        self.clear_ipv4_config();
                        IpConfigStatus::Missing
                    }
                }
            }
            None => {
                // No DHCP event arrived, so just keep the interface aligned with the tracked state.
                match self.ip_assignment {
                    IpAssignment::Unconfigured => match self.iface.ipv4_addr() {
                        Some(x) if x != Ipv4Address::UNSPECIFIED => IpConfigStatus::Ready,
                        _ => IpConfigStatus::Missing,
                    },
                    IpAssignment::TentativeLinkLocal { cidr, .. }
                    | IpAssignment::LinkLocalFallback(cidr) => {
                        if self.iface.ipv4_addr() != Some(cidr.address()) {
                            set_ipv4_addr(&mut self.iface, cidr);
                        }
                        IpConfigStatus::Ready
                    }
                    IpAssignment::Dhcp(cidr) => {
                        if self.iface.ipv4_addr() != Some(cidr.address()) {
                            set_ipv4_addr(&mut self.iface, cidr);
                        }
                        IpConfigStatus::Ready
                    }
                }
            }
        }
    }
}

/// Replace the interface's IPv4 address list with the supplied CIDR.
fn set_ipv4_addr(iface: &mut Interface, cidr: Ipv4Cidr) {
    iface.update_ip_addrs(|addrs| {
        addrs.clear();
        addrs.push(smoltcp::wire::IpCidr::Ipv4(cidr)).unwrap();
    });
}

/// Remove all IPv4 addresses from the interface.
fn clear_ipv4_addr(iface: &mut Interface) {
    iface.update_ip_addrs(|addrs| addrs.clear());
}
