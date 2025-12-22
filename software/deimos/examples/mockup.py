import time
from pathlib import Path
from deimos import Controller, peripheral, socket


def main() -> None:
    here =(Path(__file__).parent / "op").resolve()
    ctrl = Controller(
        op_name="mockup_demo", op_dir=str(here), rate_hz=100.0
    )

    ctrl.clear_sockets()
    ctrl.add_socket(socket.ThreadChannelSocket("mockup_chan"))
    ctrl.add_socket(socket.UnixSocket("ctrl"))

    ctrl.add_peripheral("mock_thread", peripheral.DeimosDaqRev6(1))
    ctrl.add_peripheral("mock_unix", peripheral.DeimosDaqRev6(2))

    driver_thread = peripheral.MockupDriver(
        peripheral.DeimosDaqRev6(1),
        peripheral.MockupTransport.thread_channel("mockup_chan"),
    )
    driver_unix = peripheral.MockupDriver(
        peripheral.DeimosDaqRev6(2),
        peripheral.MockupTransport.unix_socket("mockup_unix"),
    )
    driver_thread.run_with(ctrl)
    driver_unix.run_with(ctrl)

    handle = ctrl.run_nonblocking()
    time.sleep(0.5)
    handle.stop()
    handle.join()


if __name__ == "__main__":
    main()
