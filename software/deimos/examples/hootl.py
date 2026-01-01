"""
A showcase of drivers for imitating hardware from software
to test the software's interface with hardware.
"""

import time
from contextlib import ExitStack
from pathlib import Path
from deimos import Controller, peripheral, socket, Termination, LoopMethod

HAS_UNIX_SOCKET = hasattr(socket, "UnixSocket") and hasattr(
    peripheral.HootlTransport, "unix_socket"
)


def main() -> None:
    here = Path(__file__).parent.resolve()

    for loop_method in [LoopMethod.performant(), LoopMethod.efficient()]:
        print(f"Testing with loop method {loop_method}")

        # Set up HOOTL peripherals (drivers are attached via the controller)
        ctrl = Controller(op_name="mockup_demo", op_dir=str(here / "op"), rate_hz=20.0)
        ctrl.termination_criteria = Termination.timeout_s(3.0)
        ctrl.loop_method = loop_method

        ctrl.clear_sockets()
        ctrl.add_socket("mockup_chan", socket.ThreadChannelSocket("mockup_chan"))
        if HAS_UNIX_SOCKET:
            ctrl.add_socket("ctrl", socket.UnixSocket("ctrl"))
        ctrl.add_socket("udp", socket.UdpSocket())  # Included by default, but cleared

        ctrl.add_peripheral("mock_thread", peripheral.DeimosDaqRev6(1))
        if HAS_UNIX_SOCKET:
            ctrl.add_peripheral("mock_unix", peripheral.DeimosDaqRev6(2))
        ctrl.add_peripheral("mock_udp", peripheral.DeimosDaqRev6(3))

        # Run
        with ExitStack() as stack:
            # Run the peripheral mockups, which will wait for the controller
            # to send a request to bind
            stack.enter_context(
                ctrl.attach_hootl_driver(
                    "mock_thread",
                    peripheral.HootlTransport.thread_channel("mockup_chan"),
                )
            )
            if HAS_UNIX_SOCKET:
                stack.enter_context(
                    ctrl.attach_hootl_driver(
                        "mock_unix",
                        peripheral.HootlTransport.unix_socket("mockup_unix"),
                    )
                )
            stack.enter_context(
                ctrl.attach_hootl_driver(
                    "mock_udp",
                    peripheral.HootlTransport.udp(),
                )
            )

            # Get list of inputs available to set manually.
            # This is also available from the RunHandle during operation.
            manual_inputs = ctrl.available_inputs()
            print("Manual inputs available:")
            for name in manual_inputs[:3]:  # Adjust slicing to list more input names
                print(f"    {name}")
            print(f"    ...and {len(manual_inputs) - 3} more")

            # Run the controller, which will bind the peripheral mockups
            start = time.perf_counter()
            handle = ctrl.run_nonblocking()

            try:
                time.sleep(0.5)
                handle.write({"mock_thread.dac0": 0.0})

                # Make sure we had stable communication with all the peripheral mockups
                for k, v in handle.read().values.items():
                    if "loss_of_contact_counter" in k:
                        # When testing on macos runners, we're not able to set
                        # core affinity, which causes sporadic packet loss.
                        assert v < 2.0, f"Missed packet: {k} = {v:.0f}"
            except Exception:
                end = time.perf_counter()
                print(
                    "Sending termination signal to run handle"
                    f" from Python after {end - start:.2f}s."
                )
                handle.stop()
                raise
            finally:
                handle.join()


if __name__ == "__main__":
    main()
