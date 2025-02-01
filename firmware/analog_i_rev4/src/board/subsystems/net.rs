use smoltcp::{
    iface::{Config, Interface, SocketSet, SocketStorage},
    socket::{dhcpv4, udp},
    storage::{PacketBuffer, PacketMetadata},
    time::Instant,
    wire::{HardwareAddress, IpListenEndpoint},
};
use stm32h7xx_hal::ethernet;

use deimos_shared::PERIPHERAL_RX_PORT;

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
