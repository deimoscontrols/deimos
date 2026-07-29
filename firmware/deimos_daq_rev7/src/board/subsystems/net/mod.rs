use smoltcp::{
    iface::{Config, Interface, SocketSet, SocketStorage},
    socket::{dhcpv4, tcp, udp},
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

/// Standard port exposed by the calibrated Modbus/TCP operating path.
pub(crate) const MODBUS_TCP_PORT: u16 = 502;
/// Two complete four-entry DMA descriptor rings per ordinary network poll.
const STANDARD_POLL_FRAME_BUDGET: usize = 8;
/// Modbus IRQ work limit in each direction across all polls in one cycle.
const MODBUS_POLL_FRAME_BUDGET: usize = 2;

/// Remaining Ethernet-frame work allowed across bounded polls in one cycle.
pub(crate) struct NetPollBudget {
    /// Receive frames which smoltcp may still take from the DMA ring.
    rx_remaining: usize,
    /// Transmit frames which smoltcp may still emit through the DMA ring.
    tx_remaining: usize,
}

impl NetPollBudget {
    /// Build the fixed two-RX/two-TX budget used by one Modbus-capable cycle.
    pub(crate) fn modbus_cycle() -> Self {
        Self {
            rx_remaining: MODBUS_POLL_FRAME_BUDGET,
            tx_remaining: MODBUS_POLL_FRAME_BUDGET,
        }
    }

    /// Build a two-descriptor-ring allowance for an ordinary network poll.
    fn standard_poll() -> Self {
        Self {
            rx_remaining: STANDARD_POLL_FRAME_BUDGET,
            tx_remaining: STANDARD_POLL_FRAME_BUDGET,
        }
    }
}

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
    /// Receive-byte storage for the Modbus/TCP socket with shape `(512,)`.
    pub(crate) tcp_rx_storage: [u8; 512],
    /// Transmit-byte storage for the Modbus/TCP socket with shape `(512,)`.
    pub(crate) tcp_tx_storage: [u8; 512],
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
    /// Socket storage backing the board's UDP, TCP, and DHCP sockets.
    sockets: SocketSet<'a>,
    /// UDP socket handle used for controller-to-board traffic.
    udp_handle: smoltcp::iface::SocketHandle,
    /// TCP socket used for the calibrated Modbus/TCP connection.
    tcp_handle: smoltcp::iface::SocketHandle,
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
    /// Builds the Ethernet interface, sockets, and address state machine.
    ///
    /// The TCP socket and its backing storage are reserved here but remain
    /// closed until the Modbus operating path explicitly calls [`Self::tcp_listen`].
    ///
    /// Args:
    ///   store: Static socket metadata and payload storage.
    ///   ethdev: Initialized Ethernet DMA device.
    ///   ethernet_addr: Board MAC address.
    ///   now: Current smoltcp time in `ms`.
    ///
    /// Returns:
    ///   Initialized network subsystem.
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
            tcp_rx_storage,
            tcp_tx_storage,
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

        // Reserve the stream socket and backing buffers. Binding starts the
        // listener only when the installed calibration is marked valid.
        let tcp_socket = tcp::Socket::new(
            tcp::SocketBuffer::new(&mut tcp_rx_storage[..]),
            tcp::SocketBuffer::new(&mut tcp_tx_storage[..]),
        );
        let tcp_handle = sockets.add(tcp_socket);

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
            tcp_handle,
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
    /// repeated polls. One call processes at most two complete four-entry DMA
    /// descriptor rings in each direction.
    ///
    /// Args:
    ///   time_ns: Current board time in `ns`.
    ///
    /// Returns:
    ///   Whether smoltcp processed or emitted at least one frame.
    pub(crate) fn poll(&mut self, time_ns: i64) -> bool {
        let mut budget = NetPollBudget::standard_poll();
        self.poll_bounded(time_ns, &mut budget)
    }

    /// Poll smoltcp without allowing a packet storm to monopolize the IRQ.
    ///
    /// The same budget can be passed to a second poll later in the cycle; only
    /// the unconsumed portion remains available. The device wrapper makes
    /// smoltcp's otherwise draining internal loop terminate when either finite
    /// frame allowance is exhausted.
    ///
    /// Args:
    ///   time_ns: Current board time in `ns`.
    ///   budget: Remaining receive/transmit frame counts for this cycle.
    ///
    /// Returns:
    ///   Whether smoltcp processed or emitted at least one frame.
    #[inline(never)]
    #[unsafe(link_section = ".itcm.net_poll")]
    pub(crate) fn poll_bounded(&mut self, time_ns: i64, budget: &mut NetPollBudget) -> bool {
        self.ethdev
            .set_io_budget(budget.rx_remaining, budget.tx_remaining);
        let timestamp = Instant::from_micros(time_ns / 1000);
        let changed = self
            .iface
            .poll(timestamp, &mut self.ethdev, &mut self.sockets);
        budget.rx_remaining = self.ethdev.rx_budget();
        budget.tx_remaining = self.ethdev.tx_budget();
        self.ethdev.clear_io_budget();
        changed
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

    /// Drop any stream state when the board returns to connection discovery.
    pub(crate) fn reset_tcp_socket(&mut self) {
        self.sockets.get_mut::<tcp::Socket>(self.tcp_handle).abort();
    }

    /// Begin accepting the one calibrated Modbus/TCP connection.
    ///
    /// Returns:
    ///   `Ok(())` after binding the socket to TCP port `502`, or smoltcp's
    ///   listen error if the socket cannot enter the listening state.
    pub(crate) fn tcp_listen(&mut self) -> Result<(), tcp::ListenError> {
        self.sockets
            .get_mut::<tcp::Socket>(self.tcp_handle)
            .listen(MODBUS_TCP_PORT)
    }

    /// Aborts any current stream and returns it to the listening state.
    ///
    /// Returns:
    ///   `Ok(())` after rebinding TCP port `502`, or smoltcp's listen error.
    pub(crate) fn tcp_relisten(&mut self) -> Result<(), tcp::ListenError> {
        self.reset_tcp_socket();
        self.tcp_listen()
    }

    /// Copies currently buffered TCP receive data without blocking.
    ///
    /// Args:
    ///   bytes: Destination byte buffer with shape `(capacity,)`.
    ///
    /// Returns:
    ///   Number of bytes copied, or smoltcp's receive error when no stream data
    ///   can be read.
    pub(crate) fn tcp_recv(&mut self, bytes: &mut [u8]) -> Result<usize, tcp::RecvError> {
        self.sockets
            .get_mut::<tcp::Socket>(self.tcp_handle)
            .recv_slice(bytes)
    }

    /// Queues TCP response bytes without blocking.
    ///
    /// Args:
    ///   bytes: Response byte buffer with shape `(n_bytes,)`.
    ///
    /// Returns:
    ///   Number of bytes queued, or smoltcp's send error when the stream cannot
    ///   accept data.
    pub(crate) fn tcp_send(&mut self, bytes: &[u8]) -> Result<usize, tcp::SendError> {
        self.sockets
            .get_mut::<tcp::Socket>(self.tcp_handle)
            .send_slice(bytes)
    }

    /// Return smoltcp's broad active-session state.
    ///
    /// This includes handshake and closing states; callers needing peer-close
    /// detection use [`Self::tcp_connection_ended`] instead.
    pub(crate) fn tcp_is_active(&self) -> bool {
        self.sockets.get::<tcp::Socket>(self.tcp_handle).is_active()
    }

    /// Return whether at least one application byte is ready to receive.
    pub(crate) fn tcp_can_recv(&self) -> bool {
        self.sockets.get::<tcp::Socket>(self.tcp_handle).can_recv()
    }

    /// Return whether the TCP state can no longer receive a request from its peer.
    ///
    /// The explicit state test distinguishes an orderly peer close from
    /// `SynReceived`, where neither [`tcp::Socket::can_recv`] nor
    /// [`tcp::Socket::may_recv`] is true while the handshake is still valid.
    pub(crate) fn tcp_connection_ended(&self) -> bool {
        matches!(
            self.sockets.get::<tcp::Socket>(self.tcp_handle).state(),
            tcp::State::Closed
                | tcp::State::CloseWait
                | tcp::State::Closing
                | tcp::State::LastAck
                | tcp::State::TimeWait
        )
    }

    /// Advance the full address manager and report whether the caller may continue.
    pub(crate) fn step_address(&mut self, time_ns: i64, mode: AddressMode) -> AddressStatus {
        let mut reconnect_required = false;

        // First, resolve any in-flight tentative fallback claim before touching DHCP state.
        self.advance_tentative_fallback(time_ns);

        // If a DHCP lease was deferred while operating on fallback, apply it as
        // soon as the caller allows address changes again.
        if !matches!(mode, AddressMode::Operating)
            && let Some(config) = self.take_deferred_dhcp()
        {
            self.apply_dhcp_config(config);
            reconnect_required = matches!(mode, AddressMode::SessionSetup);
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
