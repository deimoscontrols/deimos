use smoltcp::{
    iface::{Config, Interface, SocketSet, SocketStorage},
    socket::{dhcpv4, udp},
    storage::{PacketBuffer, PacketMetadata},
    time::Instant,
    wire::{HardwareAddress, IpListenEndpoint, Ipv4Address, Ipv4Cidr},
};
use stm32h7xx_hal::ethernet;

use deimos_shared::{
    static_fallback_ipv4_from_mac, PERIPHERAL_RX_PORT, STATIC_FALLBACK_IPV4_PREFIX_LEN,
};

/// Source of the board's currently active IPv4 configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpAssignment {
    /// No IPv4 address is currently configured on the interface.
    Unconfigured,
    /// A deterministic direct-connect fallback address derived from the board MAC.
    StaticFallback(Ipv4Cidr),
    /// A lease or configuration that came from DHCP.
    Dhcp(Ipv4Cidr),
}

/// DHCP configuration that may need to be applied immediately or deferred until reconnect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingDhcpConfig {
    /// IPv4 address and prefix length offered by the DHCP server.
    pub address: Ipv4Cidr,
    /// Optional default gateway offered by the DHCP server.
    pub router: Option<Ipv4Address>,
}

/// Convert the shared fallback IPv4 policy into an `smoltcp` CIDR value.
pub fn static_fallback_cidr(mac: [u8; 6]) -> Ipv4Cidr {
    let octets = static_fallback_ipv4_from_mac(mac);
    Ipv4Cidr::new(
        Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]),
        STATIC_FALLBACK_IPV4_PREFIX_LEN,
    )
}

// This data will be held by Net through a mutable reference
pub struct NetStorageStatic<'a> {
    pub(crate) socket_storage: [SocketStorage<'a>; 8],
}

// TODO: move these to startup and initialize with MaybeUninit
static mut RX_METADATA_STORAGE: [PacketMetadata<udp::UdpMetadata>; 4] = [PacketMetadata::EMPTY; 4];
static mut RX_PAYLOAD_STORAGE: [u8; 1522] = [0u8; 1522];

static mut TX_METADATA_STORAGE: [PacketMetadata<udp::UdpMetadata>; 4] = [PacketMetadata::EMPTY; 4];
static mut TX_PAYLOAD_STORAGE: [u8; 1522] = [0u8; 1522];

pub struct Net<'a> {
    pub iface: Interface,
    pub ethdev: ethernet::EthernetDMA<4, 4>,
    pub sockets: SocketSet<'a>,
    pub udp_handle: smoltcp::iface::SocketHandle,
    pub dhcp_handle: smoltcp::iface::SocketHandle,
    pub ip_assignment: IpAssignment,
    pub pending_dhcp: Option<PendingDhcpConfig>,
}
impl<'a> Net<'a> {
    pub fn new(
        store: &'a mut NetStorageStatic<'a>,
        mut ethdev: ethernet::EthernetDMA<4, 4>,
        ethernet_addr: HardwareAddress,
        now: Instant,
    ) -> Self {
        let config = Config::new(ethernet_addr);
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
        }
    }

    /// Polls on the ethernet interface. You should refer to the smoltcp
    /// documentation for poll() to understand how to call poll efficiently
    pub fn poll(&mut self, time_ns: i64) -> bool {
        let timestamp = Instant::from_micros(time_ns / 1000);
        self.iface
            .poll(timestamp, &mut self.ethdev, &mut self.sockets)
    }
}
