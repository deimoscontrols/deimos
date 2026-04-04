use smoltcp::{
    iface::{Config, Interface, SocketSet, SocketStorage},
    socket::{dhcpv4, udp},
    storage::{PacketBuffer, PacketMetadata},
    time::Instant,
    wire::{EthernetAddress, IpListenEndpoint, Ipv4Address, Ipv4Cidr},
};
use stm32h7xx_hal::ethernet;

use deimos_shared::{
    PERIPHERAL_RX_PORT, STATIC_FALLBACK_CANDIDATE_COUNT, STATIC_FALLBACK_IPV4_PREFIX_LEN,
    static_fallback_ipv4_candidate_from_mac,
};

mod arp_scraper;
use arp_scraper::ObservedDevice;

/// Length of the post-claim conflict observation window for a tentative fallback address.
const FALLBACK_VALIDATION_NS: i64 = 250_000_000;

/// Socket storage borrowed by [`Net`] for the lifetime of the firmware.
pub(crate) struct NetStorageStatic<'a> {
    /// Backing storage for sockets registered with the smoltcp interface.
    pub(crate) socket_storage: [SocketStorage<'a>; 8],
    /// Receive-packet metadata ring for the board UDP socket.
    pub(crate) rx_metadata_storage: [PacketMetadata<udp::UdpMetadata>; 4],
    /// Receive-packet payload buffer for the board UDP socket.
    pub(crate) rx_payload_storage: [u8; 1522],
    /// Transmit-packet metadata ring for the board UDP socket.
    pub(crate) tx_metadata_storage: [PacketMetadata<udp::UdpMetadata>; 4],
    /// Transmit-packet payload buffer for the board UDP socket.
    pub(crate) tx_payload_storage: [u8; 1522],
}

/// How aggressively the address manager may change the board's network identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AddressMode {
    /// Acquire any usable address, including claiming a fallback candidate immediately.
    Connect,
    /// Keep setup traffic stable; if the address changes, the caller should reconnect.
    SessionSetup,
    /// Keep the current session stable and defer DHCP replacement while fallback is active.
    Operating,
}

/// Whether the caller can keep using the current network identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AddressStatus {
    /// No usable address is currently available, or the caller must reconnect.
    Missing,
    /// The current address remains usable for the caller's mode.
    Ready,
}

/// Source of the board's currently active IPv4 configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AddressState {
    /// No IPv4 address is currently configured.
    Unconfigured,
    /// A fallback address is being validated against conflicting ARP traffic.
    TentativeFallback {
        /// Tentative fallback CIDR currently installed on the interface.
        cidr: Ipv4Cidr,
        /// Which deterministic fallback candidate this tentative address came from.
        candidate_index: u8,
        /// Time when the tentative address becomes stable if no conflict is observed.
        validation_deadline_ns: i64,
    },
    /// A fallback address is active and may be holding a deferred DHCP lease.
    ActiveFallback {
        /// Stable fallback CIDR currently installed on the interface.
        cidr: Ipv4Cidr,
        /// DHCP lease to apply later once the caller allows endpoint changes again.
        deferred_dhcp: Option<PendingDhcpConfig>,
    },
    /// A DHCP lease is active.
    ActiveDhcp {
        /// DHCP CIDR currently installed on the interface.
        cidr: Ipv4Cidr,
    },
}

/// DHCP configuration that may need to be applied immediately or deferred until reconnect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingDhcpConfig {
    /// IPv4 address and prefix length offered by the DHCP server.
    address: Ipv4Cidr,
    /// Optional default gateway offered by the DHCP server.
    router: Option<Ipv4Address>,
}

/// DHCP events reduced to the fields the address manager actually consumes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnedDhcpEvent {
    Configured(PendingDhcpConfig),
    Deconfigured,
}

/// Progress through the deterministic fallback candidate list and its retry backoff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FallbackProgress {
    /// Candidate index to claim next within the current fallback round.
    next_candidate: u8,
    /// Number of complete fallback rounds that have already failed.
    failure_rounds: u8,
    /// Earliest time when fallback claiming may resume after backoff.
    retry_at_ns: Option<i64>,
}

impl Default for FallbackProgress {
    /// Start fallback selection from the first candidate with no accumulated backoff.
    fn default() -> Self {
        Self {
            next_candidate: 0,
            failure_rounds: 0,
            retry_at_ns: None,
        }
    }
}

/// Convert one shared fallback candidate into an `smoltcp` CIDR value.
fn static_fallback_cidr(mac: [u8; 6], index: usize) -> Ipv4Cidr {
    // Derive the deterministic fallback octets from the local MAC and candidate index.
    let octets = static_fallback_ipv4_candidate_from_mac(mac, index);
    // Wrap those octets in the subnet prefix the firmware uses for direct-connect fallback.
    Ipv4Cidr::new(
        Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]),
        STATIC_FALLBACK_IPV4_PREFIX_LEN,
    )
}

/// Compute the reconnect backoff after a full failed fallback candidate round.
fn fallback_backoff_ns(failure_rounds: u8) -> i64 {
    // Use a short initial retry and then quickly stretch out to avoid ARP spam on a busy link.
    match failure_rounds {
        0 => 0,
        1 => 5_000_000_000,
        2 => 30_000_000_000,
        3 => 60_000_000_000,
        _ => 300_000_000_000,
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

/// Owns the Ethernet interface, sockets, and IPv4 configuration state for the board.
pub(crate) struct Net<'a> {
    /// Smoltcp interface.
    iface: Interface,
    /// Ethernet device wrapper with ARP scraping.
    ethdev: ObservedDevice<ethernet::EthernetDMA<4, 4>>,
    /// Socket storage backing the board's UDP and DHCP sockets.
    sockets: SocketSet<'a>,
    /// UDP socket handle used for controller-to-board traffic.
    udp_handle: smoltcp::iface::SocketHandle,
    /// DHCP socket handle.
    dhcp_handle: smoltcp::iface::SocketHandle,
    /// Local MAC address used for deterministic fallback candidate generation.
    local_mac: [u8; 6],
    /// Current address assignment state.
    address_state: AddressState,
    /// Progress through fallback candidate selection and backoff.
    fallback: FallbackProgress,
}

impl<'a> Net<'a> {
    /// Build the Ethernet interface, UDP socket, DHCP socket, and address state machine.
    pub(crate) fn new(
        store: &'a mut NetStorageStatic<'a>,
        ethdev: ethernet::EthernetDMA<4, 4>,
        ethernet_addr: EthernetAddress,
        now: Instant,
    ) -> Self {
        let NetStorageStatic {
            socket_storage,
            rx_metadata_storage,
            rx_payload_storage,
            tx_metadata_storage,
            tx_payload_storage,
        } = store;

        // Wrap the DMA device so fallback ARP traffic can be inspected and injected.
        let mut ethdev = ObservedDevice::new(ethdev, ethernet_addr);
        let config = Config::new(ethernet_addr.into());
        let iface = Interface::new(config, &mut ethdev, now);

        // Reserve socket slots up front because the firmware keeps them for its full lifetime.
        let mut sockets = SocketSet::new(&mut socket_storage[..]);

        // Add the UDP command/data socket used by the controller.
        let rx_packet_buffer =
            PacketBuffer::new(&mut rx_metadata_storage[..], &mut rx_payload_storage[..]);
        let tx_packet_buffer =
            PacketBuffer::new(&mut tx_metadata_storage[..], &mut tx_payload_storage[..]);

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

        // Cache the local MAC in plain bytes so fallback candidate generation stays local to Net.
        let mut local_mac = [0u8; 6];
        local_mac.copy_from_slice(ethernet_addr.as_bytes());

        Net::<'a> {
            iface,
            ethdev,
            sockets,
            udp_handle,
            dhcp_handle,
            local_mac,
            address_state: AddressState::Unconfigured,
            fallback: FallbackProgress::default(),
        }
    }

    /// Polls the Ethernet interface and socket set.
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

    /// Advance the full address manager and report whether the caller may continue.
    pub(crate) fn step_address(&mut self, time_ns: i64, mode: AddressMode) -> AddressStatus {
        let mut reconnect_required = false;

        // First, resolve any in-flight tentative fallback claim before touching DHCP state.
        self.advance_tentative_fallback(time_ns);

        // If a DHCP lease was deferred while operating on fallback, apply it as
        // soon as the caller allows address changes again.
        if !matches!(mode, AddressMode::Operating) {
            if let Some(config) = self.take_deferred_dhcp() {
                self.apply_dhcp_config(config);
                reconnect_required = matches!(mode, AddressMode::SessionSetup);
            }
        }

        // Poll DHCP and collapse the borrowed smoltcp event into an owned form.
        let event = {
            let dhcp_socket = self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp_handle);
            match dhcp_socket.poll() {
                Some(dhcpv4::Event::Configured(config)) => {
                    Some(OwnedDhcpEvent::Configured(PendingDhcpConfig {
                        address: config.address,
                        router: config.router,
                    }))
                }
                Some(dhcpv4::Event::Deconfigured) => Some(OwnedDhcpEvent::Deconfigured),
                None => None,
            }
        };

        // Merge the DHCP event into the address manager's state and policy.
        reconnect_required |= self.handle_dhcp_event(mode, event);

        // While connecting, claim the next fallback candidate immediately if DHCP
        // has not already produced a usable address.
        if matches!(mode, AddressMode::Connect)
            && matches!(self.address_state, AddressState::Unconfigured)
        {
            let _ = self.claim_next_fallback(time_ns);
        }

        // Keep the smoltcp interface consistent with the state machine's source of truth.
        self.sync_iface_to_state();

        // Collapse all internal details back down to the simple public Ready/Missing API.
        if reconnect_required || matches!(self.address_state, AddressState::Unconfigured) {
            AddressStatus::Missing
        } else {
            AddressStatus::Ready
        }
    }

    /// Remove any configured IPv4 address, route, and tentative fallback watch state.
    fn clear_ipv4_config(&mut self) {
        // Drop the interface configuration itself.
        clear_ipv4_addr(&mut self.iface);
        self.iface.routes_mut().remove_default_ipv4_route();

        // Stop ARP monitoring and reset the logical address state.
        self.end_tentative_watch();
        self.address_state = AddressState::Unconfigured;
    }

    /// Reset fallback candidate selection and backoff after a successful address transition.
    fn reset_fallback_progress(&mut self) {
        self.fallback = FallbackProgress::default();
    }

    /// Promote a tentative fallback address to a stable fallback assignment.
    fn promote_tentative_fallback(&mut self, cidr: Ipv4Cidr) {
        // The address survived validation, so stop conflict watching.
        self.end_tentative_watch();

        // Keep the claimed address and clear any stale deferred-DHCP state.
        self.address_state = AddressState::ActiveFallback {
            cidr,
            deferred_dhcp: None,
        };
        self.reset_fallback_progress();
    }

    /// Record a fallback conflict and advance to the next candidate or backoff interval.
    fn note_fallback_conflict(&mut self, time_ns: i64, candidate_index: u8) {
        // Drop the conflicting tentative claim before choosing what to try next.
        self.clear_ipv4_config();

        // Either advance within this round or back off before restarting from candidate zero.
        if (candidate_index as usize + 1) < STATIC_FALLBACK_CANDIDATE_COUNT {
            self.fallback.next_candidate = candidate_index + 1;
            self.fallback.retry_at_ns = None;
        } else {
            self.fallback.next_candidate = 0;
            self.fallback.failure_rounds = self.fallback.failure_rounds.saturating_add(1);
            self.fallback.retry_at_ns =
                Some(time_ns + fallback_backoff_ns(self.fallback.failure_rounds));
        }
    }

    /// Apply a DHCP-provided IPv4 address and optional default route immediately.
    fn apply_dhcp_config(&mut self, config: PendingDhcpConfig) {
        // Install the leased address and route information on the interface.
        set_ipv4_addr(&mut self.iface, config.address);
        if let Some(router) = config.router {
            self.iface
                .routes_mut()
                .add_default_ipv4_route(router)
                .unwrap();
        } else {
            self.iface.routes_mut().remove_default_ipv4_route();
        }

        // DHCP is now authoritative, so stop tentative ARP watching and reset fallback retries.
        self.end_tentative_watch();
        self.address_state = AddressState::ActiveDhcp {
            cidr: config.address,
        };
        self.reset_fallback_progress();
    }

    /// Start watching a tentative fallback address for ARP conflicts.
    fn begin_tentative_watch(&mut self, ip: Ipv4Address) {
        self.ethdev.set_monitored_ip(Some(ip));
    }

    /// Stop ARP conflict monitoring for a tentative fallback address.
    fn end_tentative_watch(&mut self) {
        self.ethdev.set_monitored_ip(None);
    }

    /// Returns true if a conflicting ARP probe or announcement was observed for the tentative address.
    fn take_tentative_conflict(&mut self) -> bool {
        self.ethdev.take_conflict()
    }

    /// Send one ARP probe for the target fallback address.
    fn send_arp_probe(&mut self, time_ns: i64, target_ip: Ipv4Address) -> bool {
        self.ethdev
            .send_arp_probe(Instant::from_micros(time_ns / 1000), target_ip)
    }

    /// Return true if a new fallback candidate can be claimed immediately.
    fn fallback_attempt_ready(&self, time_ns: i64) -> bool {
        match self.fallback.retry_at_ns {
            Some(retry_time_ns) => time_ns >= retry_time_ns,
            None => true,
        }
    }

    /// Return true if there are more deterministic fallback candidates left in the current round.
    fn has_more_fallback_candidates(&self) -> bool {
        (self.fallback.next_candidate as usize) < STATIC_FALLBACK_CANDIDATE_COUNT
    }

    /// Claim the next deterministic fallback candidate immediately and begin conflict observation.
    fn claim_next_fallback(&mut self, time_ns: i64) -> bool {
        // Do nothing while backoff is active or once this round's candidates are exhausted.
        if !self.fallback_attempt_ready(time_ns) || !self.has_more_fallback_candidates() {
            return false;
        }

        // Derive and install the next fallback candidate as a tentative address.
        let candidate_index = self.fallback.next_candidate as usize;
        let cidr = static_fallback_cidr(self.local_mac, candidate_index);
        set_ipv4_addr(&mut self.iface, cidr);
        self.iface.routes_mut().remove_default_ipv4_route();
        self.begin_tentative_watch(cidr.address());
        self.address_state = AddressState::TentativeFallback {
            cidr,
            candidate_index: candidate_index as u8,
            validation_deadline_ns: time_ns + FALLBACK_VALIDATION_NS,
        };

        // Probe once, then rely on ARP scraping during validation.
        let _ = self.send_arp_probe(time_ns, cidr.address());
        true
    }

    /// Resolve whether the current tentative fallback address survived its validation window.
    fn advance_tentative_fallback(&mut self, time_ns: i64) {
        let AddressState::TentativeFallback {
            cidr,
            candidate_index,
            validation_deadline_ns,
        } = self.address_state
        else {
            return;
        };

        // A conflicting ARP probe or announcement means this candidate is taken.
        if self.take_tentative_conflict() {
            self.note_fallback_conflict(time_ns, candidate_index);
            return;
        }

        // Otherwise the candidate becomes stable once its validation window expires.
        if time_ns >= validation_deadline_ns {
            self.promote_tentative_fallback(cidr);
        }
    }

    /// Extract any DHCP lease that was deferred while operating on a fallback address.
    fn take_deferred_dhcp(&mut self) -> Option<PendingDhcpConfig> {
        match &mut self.address_state {
            AddressState::ActiveFallback { deferred_dhcp, .. } => deferred_dhcp.take(),
            _ => None,
        }
    }

    /// Merge one DHCP event into the address manager and report whether setup callers must reconnect.
    fn handle_dhcp_event(&mut self, mode: AddressMode, event: Option<OwnedDhcpEvent>) -> bool {
        let mut reconnect_required = false;

        match event {
            Some(OwnedDhcpEvent::Configured(config)) => match mode {
                AddressMode::Connect => {
                    // During discovery, DHCP can immediately win over any fallback attempt.
                    self.apply_dhcp_config(config);
                }
                AddressMode::SessionSetup => match self.address_state {
                    AddressState::ActiveFallback { .. }
                    | AddressState::TentativeFallback { .. } => {
                        // Setup traffic must stay on one endpoint, so force a reconnect after swapping.
                        self.apply_dhcp_config(config);
                        reconnect_required = true;
                    }
                    _ => {
                        // If DHCP was already authoritative, just refresh it in place.
                        self.apply_dhcp_config(config);
                    }
                },
                AddressMode::Operating => match self.address_state {
                    AddressState::ActiveFallback { cidr, .. } => {
                        // While operating on fallback, remember the lease but do not change endpoints yet.
                        self.address_state = AddressState::ActiveFallback {
                            cidr,
                            deferred_dhcp: Some(config),
                        };
                    }
                    AddressState::ActiveDhcp { .. } => {
                        // If DHCP is already active, refreshing the lease is non-disruptive.
                        self.apply_dhcp_config(config);
                    }
                    AddressState::TentativeFallback { .. } | AddressState::Unconfigured => {
                        // If the endpoint changes while operating, let the caller reconnect cleanly.
                        self.apply_dhcp_config(config);
                        reconnect_required = true;
                    }
                },
            },
            Some(OwnedDhcpEvent::Deconfigured) => match self.address_state {
                AddressState::ActiveDhcp { .. } => {
                    // Losing an active lease leaves the interface without a usable address.
                    self.clear_ipv4_config();
                }
                AddressState::ActiveFallback { cidr, .. } => {
                    // Dropping a deferred lease does not affect the active fallback address.
                    self.address_state = AddressState::ActiveFallback {
                        cidr,
                        deferred_dhcp: None,
                    };
                }
                // A fallback claim in progress stays in charge until validation resolves it.
                AddressState::TentativeFallback { .. } => {}
                AddressState::Unconfigured => {
                    // Stay explicitly empty if no other address source is active.
                    self.clear_ipv4_config();
                }
            },
            None => {}
        }

        reconnect_required
    }

    /// Keep the smoltcp interface aligned with the address state machine's source of truth.
    fn sync_iface_to_state(&mut self) {
        match self.address_state {
            AddressState::Unconfigured => {
                // Explicitly clear the interface so nothing survives from an old address source.
                clear_ipv4_addr(&mut self.iface);
                self.iface.routes_mut().remove_default_ipv4_route();
                self.end_tentative_watch();
            }
            AddressState::TentativeFallback { cidr, .. } => {
                // Tentative fallback keeps the candidate address installed and ARP watch armed.
                if self.iface.ipv4_addr() != Some(cidr.address()) {
                    set_ipv4_addr(&mut self.iface, cidr);
                }
                self.iface.routes_mut().remove_default_ipv4_route();
                self.begin_tentative_watch(cidr.address());
            }
            AddressState::ActiveFallback { cidr, .. } => {
                // Stable fallback keeps the address but removes tentative ARP monitoring.
                if self.iface.ipv4_addr() != Some(cidr.address()) {
                    set_ipv4_addr(&mut self.iface, cidr);
                }
                self.iface.routes_mut().remove_default_ipv4_route();
                self.end_tentative_watch();
            }
            AddressState::ActiveDhcp { cidr } => {
                // DHCP owns both the address and route state, so just keep the address installed here.
                if self.iface.ipv4_addr() != Some(cidr.address()) {
                    set_ipv4_addr(&mut self.iface, cidr);
                }
                self.end_tentative_watch();
            }
        }
    }
}
