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

/// Source of the board's currently active IPv4 configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IpAssignment {
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
pub(super) struct PendingDhcpConfig {
    /// IPv4 address and prefix length offered by the DHCP server.
    pub address: Ipv4Cidr,
    /// Optional default gateway offered by the DHCP server.
    pub router: Option<Ipv4Address>,
}

/// Convert one shared fallback candidate into a `smoltcp` CIDR value.
pub(super) fn static_fallback_cidr(mac: [u8; 6], index: usize) -> Ipv4Cidr {
    let octets = static_fallback_ipv4_candidate_from_mac(mac, index);
    Ipv4Cidr::new(
        Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]),
        STATIC_FALLBACK_IPV4_PREFIX_LEN,
    )
}

/// Compute the reconnect backoff after a full failed fallback candidate round.
pub(super) fn fallback_backoff_ns(failure_rounds: u8) -> i64 {
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
pub(super) struct ObservedDevice<D> {
    inner: D,
    arp_watch: ArpWatch,
}

impl<D: phy::Device> ObservedDevice<D> {
    /// Wrap the underlying device with ARP conflict tracking for the provided local MAC.
    pub(super) fn new(inner: D, local_mac: EthernetAddress) -> Self {
        Self {
            inner,
            arp_watch: ArpWatch::new(local_mac),
        }
    }

    /// Update which IPv4 address should currently be watched for ARP conflicts.
    pub(super) fn set_monitored_ip(&mut self, monitored_ip: Option<Ipv4Address>) {
        self.arp_watch.set_monitored_ip(monitored_ip);
    }

    /// Return whether a conflicting ARP frame has been observed since the last check.
    pub(super) fn take_conflict(&mut self) -> bool {
        self.arp_watch.take_conflict()
    }

    /// Emit one ARP frame described by the supplied representation.
    pub(super) fn send_arp_repr(
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

    /// Broadcast an ARP probe for a tentative fallback IPv4 address.
    pub(super) fn send_arp_probe(&mut self, timestamp: Instant, target_ip: Ipv4Address) -> bool {
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

    /// Broadcast a gratuitous ARP to announce a claimed fallback IPv4 address.
    pub(super) fn send_gratuitous_arp(
        &mut self,
        timestamp: Instant,
        claimed_ip: Ipv4Address,
    ) -> bool {
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
pub(super) struct ObservedRxToken<'a, T: phy::RxToken> {
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
pub(super) struct ObservedTxToken<T: phy::TxToken> {
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
