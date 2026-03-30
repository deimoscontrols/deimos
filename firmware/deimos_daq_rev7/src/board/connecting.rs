use super::*;

use smoltcp::socket::udp;
use smoltcp::wire::IpListenEndpoint;

use deimos_shared::{
    peripherals::deimos_daq_rev6::operating_roundtrip::OperatingRoundtripInput, PERIPHERAL_RX_PORT,
};

impl<'a> Board<'a> {
    /// Ensure the board has a usable IPv4 address before entering `Binding`.
    pub fn connect(&mut self) -> BoardState {
        // Initialize
        self.set_outputs(&OperatingRoundtripInput::default());
        self.dt_ns = 1_000_000;
        self.systick_init();
        self.watchdog.feed();

        // Unbind if previously bound
        self.controller = None;
        let udpsocket = self.net.sockets.get_mut::<udp::Socket>(self.net.udp_handle);
        udpsocket.close();
        udpsocket
            .bind(IpListenEndpoint {
                addr: None,
                port: PERIPHERAL_RX_PORT,
            })
            .unwrap();

        // Set status LEDs
        self.led0.set_low();
        self.led1.set_low();
        self.led2.set_low();
        self.led3.set_low();

        // Poll once so an already-available DHCP lease can win immediately; otherwise
        // install the static fallback and proceed without waiting.
        self.net.poll(self.time_ns);
        match self.poll_ip_config(true) {
            IpConfigStatus::Ready | IpConfigStatus::DhcpApplied => {}
            IpConfigStatus::Missing | IpConfigStatus::DhcpDeferred => {
                self.apply_static_fallback();
            }
        }
        self.watchdog.feed();

        return BoardState::Binding;
    }
}
