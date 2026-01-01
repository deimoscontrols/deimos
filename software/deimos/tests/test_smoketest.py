"""Smoke test for HOOTL mockups with full peripheral/socket/dispatcher coverage."""

from __future__ import annotations
from math import isnan

import tempfile
import time
from contextlib import ExitStack
from pathlib import Path

from deimos import (
    Controller,
    LoopMethod,
    Overflow,
    Termination,
    dispatcher,
    peripheral,
    socket,
)

THREAD_CHANNEL1 = "hootl_thread1"
THREAD_CHANNEL2 = "hootl_thread2"
UNIX_SOCKET = "ctrl"
RATE_HZ = 20.0
RUN_TIMEOUT_S = 10.0  # Accommodate slow CI runners (we don't actually run this long)
LATEST_FILTER_HZ = 5.0
HAS_UNIX_SOCKET = hasattr(socket, "UnixSocket") and hasattr(
    peripheral.HootlTransport, "unix_socket"
)


def _metric_channels(peripheral_name: str) -> list[str]:
    """Limited channel list to reduce disk I/O during testing"""
    return [
        f"{peripheral_name}.metrics.cycle_time_ns",
        f"{peripheral_name}.metrics.loss_of_contact_counter",
    ]


def _make_transport(kind: str, name: str | None) -> peripheral.HootlTransport:
    if kind == "thread":
        if name is None:
            raise ValueError("thread transport requires a name")
        return peripheral.HootlTransport.thread_channel(name)
    if kind == "unix":
        if not HAS_UNIX_SOCKET:
            raise RuntimeError(
                "unix socket transport is not available on this platform"
            )
        if name is None:
            raise ValueError("unix transport requires a name")
        return peripheral.HootlTransport.unix_socket(name)
    if kind == "udp":
        return peripheral.HootlTransport.udp()
    raise ValueError(f"Unknown transport kind: {kind}")


def _build_controller(
    op_dir: Path, loop_method: LoopMethod
) -> tuple[Controller, list[tuple[str, peripheral.HootlTransport]]]:
    ctrl = Controller(op_name="smoketest", op_dir=str(op_dir), rate_hz=RATE_HZ)
    ctrl.termination_criteria = Termination.timeout_s(RUN_TIMEOUT_S)
    ctrl.loop_method = loop_method

    ctrl.clear_sockets()
    ctrl.add_socket("thread1", socket.ThreadChannelSocket(THREAD_CHANNEL1))
    ctrl.add_socket("thread2", socket.ThreadChannelSocket(THREAD_CHANNEL2))
    if HAS_UNIX_SOCKET:
        ctrl.add_socket("unix", socket.UnixSocket(UNIX_SOCKET))
    ctrl.add_socket("udp", socket.UdpSocket())

    ctrl.add_dispatcher("csv", dispatcher.CsvDispatcher(1, Overflow.wrap()))
    ctrl.add_dispatcher("latest_value", dispatcher.LatestValueDispatcher())
    ctrl.add_dispatcher(
        "channel_filter",
        dispatcher.ChannelFilter(
            dispatcher.LatestValueDispatcher(),
            _metric_channels("analog_rev2"),
        ),
    )
    ctrl.add_dispatcher(
        "decimation",
        dispatcher.DecimationDispatcher(dispatcher.LatestValueDispatcher(), 2),
    )
    ctrl.add_dispatcher(
        "low_pass",
        dispatcher.LowPassDispatcher(
            dispatcher.LatestValueDispatcher(), LATEST_FILTER_HZ
        ),
    )
    ctrl.add_dataframe_dispatcher("dataframe", 1, Overflow.wrap())

    specs = [
        ("analog_rev2", peripheral.AnalogIRev2, 1001, ("thread", THREAD_CHANNEL1)),
        ("analog_rev3", peripheral.AnalogIRev3, 1002, ("thread", THREAD_CHANNEL2)),
    ]
    if HAS_UNIX_SOCKET:
        specs.extend(
            [
                ("analog_rev4", peripheral.AnalogIRev4, 1003, ("unix", "per_analog4")),
                ("daq_rev5", peripheral.DeimosDaqRev5, 1004, ("unix", "per_daq5")),
            ]
        )
    specs.append(("daq_rev6", peripheral.DeimosDaqRev6, 1005, ("udp", None)))

    attachments: list[tuple[str, peripheral.HootlTransport]] = []
    for name, cls, serial, (transport_kind, transport_name) in specs:
        ctrl.add_peripheral(name, cls(serial))
        transport = _make_transport(transport_kind, transport_name)
        attachments.append((name, transport))

    return ctrl, attachments


def _run_controller(
    loop_method: LoopMethod,
    blocking: bool,
    latest_value_cutoff: float | None,
) -> None:
    with tempfile.TemporaryDirectory(prefix="deimos-smoketest-") as tmp_dir:
        op_dir = Path(tmp_dir)
        ctrl, attachments = _build_controller(op_dir, loop_method)

        with ExitStack() as stack:
            for name, transport in attachments:
                stack.enter_context(ctrl.attach_hootl_driver(name, transport))

            if blocking:
                # Use shorter timeout for blocking mode, since we don't need to read
                # values live.
                ctrl.termination_criteria = Termination.timeout_s(0.2)
                ctrl.run()
                return

            handle = ctrl.run_nonblocking(latest_value_cutoff)
            try:
                time.sleep(0.2)  # Accommodate slow CI runners
                snapshot = handle.read()
                expected = _metric_channels("analog_rev2")
                for channel in expected:
                    assert channel in snapshot.values
                
                for k, v in snapshot.values.items():
                    assert not isnan(v), f"Channel {k} did not read successfully."
                
            finally:
                if handle.is_running():
                    print("Sending controller stop signal from Python.")
                    handle.stop()
                handle.join()

    # Pause for hootl drivers to shut down
    time.sleep(0.1)


def test_hootl_smoketest() -> None:
    for loop_method in [LoopMethod.performant(), LoopMethod.efficient()]:
        _run_controller(loop_method, blocking=True, latest_value_cutoff=None)
        _run_controller(loop_method, blocking=False, latest_value_cutoff=None)
        _run_controller(
            loop_method, blocking=False, latest_value_cutoff=LATEST_FILTER_HZ
        )
