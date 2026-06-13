//! Ethernet device wrapper to watch for and emit ARP packets without
//! interfering with other smoltcp activity. This is required in order
//! to support unicast ARP probes while resolving a static address.

use smoltcp::{
    phy::{self, DeviceCapabilities, TxToken as _},
    time::Instant,
    wire::{
        ArpOperation, ArpPacket, ArpRepr, EthernetAddress, EthernetFrame, EthernetProtocol,
        Ipv4Address,
    },
};

/// Tracks whether the currently tentative fallback IPv4 address has been challenged by ARP traffic.
#[derive(Clone, Copy, Debug)]
struct ArpWatch {
    /// Local Ethernet MAC so self-generated ARP traffic can be ignored.
    local_mac: EthernetAddress,
    /// Tentative IPv4 address currently being watched for conflicts.
    monitored_ip: Option<Ipv4Address>,
    /// Latched indicator that another host challenged the tentative address.
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
        // Ignore all traffic when no tentative fallback address is being validated.
        let monitored_ip = match self.monitored_ip {
            Some(ip) => ip,
            None => return,
        };

        // Parse just enough Ethernet and ARP structure to inspect the sender and target IPs.
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

        // Ignore our own ARP traffic because tentative fallback emits a probe for the candidate IP.
        if source_hardware_addr == self.local_mac {
            return;
        }

        // Treat either an ARP announcement or an ARP probe for our tentative IP as a conflict.
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
    /// Underlying Ethernet DMA device that actually owns the descriptors and frame IO.
    inner: D,
    /// ARP watcher state layered on top of the underlying device.
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
        // Allocate a transmit token from the underlying device for this synthetic ARP frame.
        let local_mac = self.arp_watch.local_mac;
        let arp_len = repr.buffer_len();
        let frame_len = EthernetFrame::<&[u8]>::buffer_len(arp_len);
        let Some(tx) = phy::Device::transmit(self, timestamp) else {
            return false;
        };

        // Build the Ethernet header and ARP payload directly into the DMA-owned buffer.
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
        // Observe the raw frame before handing it to smoltcp so fallback conflicts are latched early.
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
        // Wrap received frames so ARP conflict tracking runs transparently alongside smoltcp.
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
        // Forward transmit capability unchanged because only receive-side inspection is needed here.
        self.inner
            .transmit(timestamp)
            .map(|tx| ObservedTxToken { inner: tx })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }
}
