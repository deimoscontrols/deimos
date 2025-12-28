"""
A showcase of drivers for imitating hardware from software
to test the software's interface with hardware.
"""

import time
from contextlib import ExitStack
from pathlib import Path
from deimos import Controller, peripheral, socket, Termination, LoopMethod


def main() -> None:
    here = Path(__file__).parent.resolve()

    mock_thread = peripheral.DeimosDaqRev6(1)
    mock_unix = peripheral.DeimosDaqRev6(2)
    mock_udp = peripheral.DeimosDaqRev6(3)

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

    for loop_method in [LoopMethod.Performant, LoopMethod.Efficient]:
        print(f"Testing with loop method {loop_method}")

        ctrl = Controller(op_name="mockup_demo", op_dir=str(here / "op"), rate_hz=20.0)
        ctrl.termination_criteria = Termination.timeout_s(1.0)
        ctrl.loop_method = loop_method

        ctrl.clear_sockets()
        ctrl.add_socket(socket.ThreadChannelSocket("mockup_chan"))
        ctrl.add_socket(socket.UnixSocket("ctrl"))
        ctrl.add_socket(socket.UdpSocket())

        ctrl.add_peripheral("mock_thread", mock_thread)
        ctrl.add_peripheral("mock_unix", mock_unix)
        ctrl.add_peripheral("mock_udp", mock_udp)

        with ExitStack() as stack:
            # Run the peripheral mockups, which will wait for the controller
            # to send a request to bind
            stack.enter_context(driver_thread.run_with(ctrl))
            stack.enter_context(driver_unix.run_with(ctrl))
            stack.enter_context(driver_udp.run_with(ctrl))

            # Run the controller, which will bind the peripheral mockups
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
