"""
A showcase of drivers for imitating hardware from software
to test the software's interface with hardware.
"""

import time
from pathlib import Path
from deimos import Controller, context, peripheral, socket


def main() -> None:
    here = Path(__file__).parent.resolve()
    ctrl = Controller(op_name="mockup_demo", op_dir=str(here / "op"), rate_hz=100.0)
    ctrl.termination_criteria = [
        context.Termination.timeout_ns(1_000_000_000)
    ]

    ctrl.clear_sockets()
    ctrl.add_socket(socket.ThreadChannelSocket("mockup_chan"))
    ctrl.add_socket(socket.UnixSocket("ctrl"))
    ctrl.add_socket(socket.UdpSocket())

    ctrl.add_peripheral("mock_thread", peripheral.DeimosDaqRev6(1))
    ctrl.add_peripheral("mock_unix", peripheral.DeimosDaqRev6(2))
    ctrl.add_peripheral("mock_udp", peripheral.DeimosDaqRev6(3))

    driver_thread = peripheral.HootlDriver(
        peripheral.DeimosDaqRev6(1),
        peripheral.MockupTransport.thread_channel("mockup_chan"),
    )
    driver_unix = peripheral.HootlDriver(
        peripheral.DeimosDaqRev6(2),
        peripheral.MockupTransport.unix_socket("mockup_unix"),
    )
    driver_udp = peripheral.HootlDriver(
        peripheral.DeimosDaqRev6(3),
        peripheral.MockupTransport.udp(),
    )
    driver_thread.run_with(ctrl)
    driver_unix.run_with(ctrl)
    driver_udp.run_with(ctrl)

    handle = ctrl.run_nonblocking()
    try:
        time.sleep(0.5)

        # Make sure we had stable communication with all the peripheral mockups
        for k, v in handle.read().values.items():
            if "loss_of_contact_counter" in k:
                assert v == 0.0, f"Missed packet: {k} = {v:.0f}"
    except Exception:
        handle.stop()
        raise
    finally:
        handle.join()


if __name__ == "__main__":
    main()
