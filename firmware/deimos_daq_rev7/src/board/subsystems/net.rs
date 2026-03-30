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

/// Length of the post-claim conflict observation window for a tentative fallback address.
const FALLBACK_VALIDATION_NS: i64 = 250_000_000;

/// Source of the board's currently active IPv4 configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IpAssignment {
    /// No IPv4 address is currently configured on the interface.
    Unconfigured,
    /// A recently claimed fallback address that is still inside its conflict validation window.
    TentativeLinkLocal {
        /// The tentative fallback CIDR currently configured on the interface.
        cidr: Ipv4Cidr,
        /// Which deterministic fallback candidate this address came from.
        candidate_index: u8,
        /// Time when the tentative address should be promoted to stable if no conflict is seen.
        validation_deadline_ns: i64,
    },
    /// A stable deterministic link-local fallback address.
    LinkLocalFallback(Ipv4Cidr),
    /// A lease or configuration that came from DHCP.
    Dhcp(Ipv4Cidr),
}

/// Result of evaluating the board's current IPv4 configuration state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IpConfigStatus {
    /// No usable IPv4 address is currently configured.
    Missing,
    /// The current IPv4 configuration remains usable and unchanged.
    Ready,
    /// A DHCP configuration was just applied and callers should treat the address as changed.
    DhcpApplied,
    /// A DHCP configuration was observed but intentionally deferred until reconnect.
    DhcpDeferred,
}

/// DHCP configuration that may need to be applied immediately or deferred until reconnect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingDhcpConfig {
    /// IPv4 address and prefix length offered by the DHCP server.
    pub address: Ipv4Cidr,
    /// Optional default gateway offered by the DHCP server.
    pub router: Option<Ipv4Address>,
}

/// Convert one shared fallback candidate into an `smoltcp` CIDR value.
fn static_fallback_cidr(mac: [u8; 6], index: usize) -> Ipv4Cidr {
    let octets = static_fallback_ipv4_candidate_from_mac(mac, index);
    Ipv4Cidr::new(
        Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]),
        STATIC_FALLBACK_IPV4_PREFIX_LEN,
    )
}

/// Compute the reconnect backoff after a full failed fallback candidate round.
fn fallback_backoff_ns(failure_rounds: u8) -> i64 {
    match failure_rounds {
        0 => 0,
        1 => 5_000_000_000,
        2 => 30_000_000_000,
        3 => 60_000_000_000,
        _ => 300_000_000_000,
    }
}

/// Tracks whether the currently tentative fallback IPv4 address has been challenged by ARP traffic.
#[derive(Clone, Copy, Debug)]
struct ArpWatch {
    local_mac: EthernetAddress,
    monitored_ip: Option<Ipv4Address>,
    conflict_seen: bool,
}

impl ArpWatch {
    /// Create a watcher for ARP conflicts against addresses claimed by the local MAC.
    fn new(local_mac: EthernetAddress) -> Self {
        Self {
            local_mac,
            monitored_ip: None,
            conflict_seen: false,
        }
    }

    /// Update which IPv4 address should currently be monitored for ARP conflicts.
    fn set_monitored_ip(&mut self, monitored_ip: Option<Ipv4Address>) {
        self.monitored_ip = monitored_ip;
        self.conflict_seen = false;
    }

    /// Return whether a conflict was seen since the last check and clear the latched flag.
    fn take_conflict(&mut self) -> bool {
        let conflict = self.conflict_seen;
        self.conflict_seen = false;
        conflict
    }

    /// Inspect one received Ethernet frame and flag ARP probes or announcements for the monitored IP.
    fn observe_frame(&mut self, frame_bytes: &[u8]) {
        let monitored_ip = match self.monitored_ip {
            Some(ip) => ip,
            None => return,
        };

        let frame = match EthernetFrame::new_checked(frame_bytes) {
            Ok(frame) => frame,
            Err(_) => return,
        };
        if frame.ethertype() != EthernetProtocol::Arp {
            return;
        }

        let packet = match ArpPacket::new_checked(frame.payload()) {
            Ok(packet) => packet,
            Err(_) => return,
        };
        let repr = match ArpRepr::parse(&packet) {
            Ok(repr) => repr,
            Err(_) => return,
        };

        let ArpRepr::EthernetIpv4 {
            source_hardware_addr,
            source_protocol_addr,
            target_protocol_addr,
            ..
        } = repr
        else {
            return;
        };

        if source_hardware_addr == self.local_mac {
            return;
        }

        let announced_our_ip = source_protocol_addr == monitored_ip;
        let probed_our_ip = source_protocol_addr == Ipv4Address::UNSPECIFIED
            && target_protocol_addr == monitored_ip;

        if announced_our_ip || probed_our_ip {
            self.conflict_seen = true;
        }
    }
}

/// Wraps the Ethernet DMA device so fallback ARP traffic can be observed and emitted.
struct ObservedDevice<D> {
    inner: D,
    arp_watch: ArpWatch,
}

impl<D> ObservedDevice<D> {
    /// Wrap the underlying device with ARP conflict tracking for the provided local MAC.
    fn new(inner: D, local_mac: EthernetAddress) -> Self {
        Self {
            inner,
            arp_watch: ArpWatch::new(local_mac),
        }
    }
}

impl<D: phy::Device> ObservedDevice<D> {
    /// Update which IPv4 address should currently be watched for ARP conflicts.
    fn set_monitored_ip(&mut self, monitored_ip: Option<Ipv4Address>) {
        self.arp_watch.set_monitored_ip(monitored_ip);
    }

    /// Return whether a conflicting ARP frame has been observed since the last check.
    fn take_conflict(&mut self) -> bool {
        self.arp_watch.take_conflict()
    }

    /// Emit one ARP frame described by the supplied representation.
    fn send_arp_repr(
        &mut self,
        timestamp: Instant,
        dst_mac: EthernetAddress,
        repr: ArpRepr,
    ) -> bool {
        let local_mac = self.arp_watch.local_mac;
        let arp_len = repr.buffer_len();
        let frame_len = EthernetFrame::<&[u8]>::buffer_len(arp_len);
        let Some(tx) = phy::Device::transmit(self, timestamp) else {
            return false;
        };

        tx.consume(frame_len, |buf| {
            let mut frame = EthernetFrame::new_unchecked(buf);
            frame.set_dst_addr(dst_mac);
            frame.set_src_addr(local_mac);
            frame.set_ethertype(EthernetProtocol::Arp);

            let mut packet = ArpPacket::new_unchecked(frame.payload_mut());
            repr.emit(&mut packet);
        });
        true
    }

    fn send_arp_probe(&mut self, timestamp: Instant, target_ip: Ipv4Address) -> bool {
        self.send_arp_repr(
            timestamp,
            EthernetAddress::BROADCAST,
            ArpRepr::EthernetIpv4 {
                operation: ArpOperation::Request,
                source_hardware_addr: self.arp_watch.local_mac,
                source_protocol_addr: Ipv4Address::UNSPECIFIED,
                target_hardware_addr: EthernetAddress([0; 6]),
                target_protocol_addr: target_ip,
            },
        )
    }

    fn send_gratuitous_arp(&mut self, timestamp: Instant, claimed_ip: Ipv4Address) -> bool {
        self.send_arp_repr(
            timestamp,
            EthernetAddress::BROADCAST,
            ArpRepr::EthernetIpv4 {
                operation: ArpOperation::Request,
                source_hardware_addr: self.arp_watch.local_mac,
                source_protocol_addr: claimed_ip,
                target_hardware_addr: EthernetAddress([0; 6]),
                target_protocol_addr: claimed_ip,
            },
        )
    }
}

/// Receive token wrapper that inspects incoming ARP frames before handing them to `smoltcp`.
struct ObservedRxToken<'a, T: phy::RxToken> {
    inner: T,
    arp_watch: &'a mut ArpWatch,
}

impl<'a, T: phy::RxToken> phy::RxToken for ObservedRxToken<'a, T> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let arp_watch = self.arp_watch;
        self.inner.consume(|buf| {
            arp_watch.observe_frame(buf);
            f(buf)
        })
    }
}

/// Transmit token wrapper used by [`ObservedDevice`].
struct ObservedTxToken<T: phy::TxToken> {
    inner: T,
}

impl<T: phy::TxToken> phy::TxToken for ObservedTxToken<T> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.inner.consume(len, f)
    }
}

impl<D: phy::Device> phy::Device for ObservedDevice<D> {
    type RxToken<'a>
        = ObservedRxToken<'a, D::RxToken<'a>>
    where
        Self: 'a;
    type TxToken<'a>
        = ObservedTxToken<D::TxToken<'a>>
    where
        Self: 'a;

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.inner.receive(timestamp).map(|(rx, tx)| {
            (
                ObservedRxToken {
                    inner: rx,
                    arp_watch: &mut self.arp_watch,
                },
                ObservedTxToken { inner: tx },
            )
        })
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        self.inner
            .transmit(timestamp)
            .map(|tx| ObservedTxToken { inner: tx })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }
}

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
    pub(crate) sockets: SocketSet<'a>,
    pub(crate) udp_handle: smoltcp::iface::SocketHandle,
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
        let mut ethdev = ObservedDevice::new(ethdev, ethernet_addr);
        let config = Config::new(ethernet_addr.into());
        let iface = Interface::new(config, &mut ethdev, now);

        let mut sockets = SocketSet::new(&mut store.socket_storage[..]);

        // Add UDP socket
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

        // Add DHCP socket
        let dhcp_socket = dhcpv4::Socket::new();
        let dhcp_handle: smoltcp::iface::SocketHandle = sockets.add(dhcp_socket);

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

    /// Polls on the ethernet interface. You should refer to the smoltcp
    /// documentation for poll() to understand how to call poll efficiently
    pub(crate) fn poll(&mut self, time_ns: i64) -> bool {
        let timestamp = Instant::from_micros(time_ns / 1000);
        self.iface
            .poll(timestamp, &mut self.ethdev, &mut self.sockets)
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
        self.clear_ipv4_config();
        if (candidate_index as usize + 1) < STATIC_FALLBACK_CANDIDATE_COUNT {
            self.next_fallback_candidate = candidate_index + 1;
            self.fallback_backoff_until_ns = None;
        } else {
            self.next_fallback_candidate = 0;
            self.fallback_failure_rounds = self.fallback_failure_rounds.saturating_add(1);
            self.fallback_backoff_until_ns =
                Some(time_ns + fallback_backoff_ns(self.fallback_failure_rounds));
        }
    }

    /// Apply a DHCP-provided IPv4 address and optional default route immediately.
    fn apply_dhcp_config(&mut self, config: PendingDhcpConfig) {
        set_ipv4_addr(&mut self.iface, config.address);
        if let Some(router) = config.router {
            self.iface
                .routes_mut()
                .add_default_ipv4_route(router)
                .unwrap();
        } else {
            self.iface.routes_mut().remove_default_ipv4_route();
        }
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
        let _ = self.send_arp_probe(time_ns, cidr.address());
        let _ = self.send_gratuitous_arp(time_ns, cidr.address());
        true
    }

    /// Poll DHCP and reconcile the active IPv4 configuration.
    pub(crate) fn poll_ip_config(&mut self, time_ns: i64, allow_dhcp_swap: bool) -> IpConfigStatus {
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

        if allow_dhcp_swap {
            if let Some(config) = self.pending_dhcp.take() {
                self.apply_dhcp_config(config);
                return IpConfigStatus::DhcpApplied;
            }
        }

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
            None => match self.ip_assignment {
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
            },
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
