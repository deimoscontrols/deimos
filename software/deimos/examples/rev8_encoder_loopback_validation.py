"""Validate rev8 Encoder0 by wiring normal GPIO outputs back to its phases.

Wire DO0 to Encoder0 A and DO1 to Encoder0 B, including the corresponding
signal reference/return connections required by the front-panel wiring. The
module must be running the normal rev8 firmware.
"""

import argparse
import math
import time
from pathlib import Path

import deimos
from deimos import LoopMethod, RunHandle, Snapshot


QUADRATURE_SEQUENCE = (
    (1.0, 0.0),
    (1.0, 1.0),
    (0.0, 1.0),
    (0.0, 0.0),
)


def wait_for_snapshots(
    handle: RunHandle,
    previous_timestamp: int,
    required: int,
    timeout_s: float,
) -> Snapshot:
    """Wait for a number of distinct nonblocking controller snapshots."""
    deadline = time.monotonic() + timeout_s
    observed = 0
    while True:
        if not handle.is_running():
            raise RuntimeError("Controller stopped during encoder loopback validation")

        snapshot = handle.read()
        if snapshot.timestamp > previous_timestamp:
            previous_timestamp = snapshot.timestamp
            observed += 1
            if observed == required:
                return snapshot

        if time.monotonic() >= deadline:
            raise TimeoutError(
                f"Timed out waiting for {required} controller snapshot(s)"
            )
        time.sleep(0.001)


def write_phase_state(
    handle: RunHandle,
    do0: str,
    do1: str,
    a: float,
    b: float,
    transition_timeout_s: float,
) -> Snapshot:
    """Write one A/B state and wait until it has reached the board."""
    previous_timestamp = handle.read().timestamp
    handle.write({do0: a, do1: b})

    # Two publications cover a write racing with the current control cycle and
    # ensure this state reaches the board before the next edge is requested.
    return wait_for_snapshots(handle, previous_timestamp, 2, transition_timeout_s)


def encoder_count(snapshot: Snapshot, channel: str) -> int:
    """Extract one integer-valued encoder count from a snapshot."""
    try:
        value = snapshot.values[channel]
    except KeyError as error:
        raise RuntimeError(f"Snapshot is missing rev8 channel `{channel}`") from error
    if not math.isfinite(value) or not value.is_integer():
        raise RuntimeError(f"Invalid encoder count {value!r} on `{channel}`")
    return int(value)


def validate_encoder_loopback(
    handle: RunHandle,
    peripheral_name: str,
    cycles: int = 64,
    transition_timeout_s: float = 2.0,
) -> int:
    """Drive DO0/DO1 and require Encoder0 to increment four times per cycle."""
    if cycles <= 0:
        raise ValueError("cycles must be positive")
    if not math.isfinite(transition_timeout_s) or transition_timeout_s <= 0.0:
        raise ValueError("transition_timeout_s must be finite and positive")

    do0 = f"{peripheral_name}.do0"
    do1 = f"{peripheral_name}.do1"
    encoder0 = f"{peripheral_name}.encoder0"
    available_inputs = set(handle.available_inputs())
    missing_inputs = {do0, do1} - available_inputs
    if missing_inputs:
        missing = ", ".join(sorted(missing_inputs))
        raise RuntimeError(f"Loopback GPIO inputs are not manually writable: {missing}")

    try:
        snapshot = write_phase_state(
            handle, do0, do1, 0.0, 0.0, transition_timeout_s
        )
        baseline = encoder_count(snapshot, encoder0)

        for _ in range(cycles):
            for a, b in QUADRATURE_SEQUENCE:
                snapshot = write_phase_state(
                    handle, do0, do1, a, b, transition_timeout_s
                )

        # Firmware samples the counter before applying outputs from that
        # transaction. Observe another publication to include the final edge.
        snapshot = wait_for_snapshots(
            handle, snapshot.timestamp, 1, transition_timeout_s
        )
        final = encoder_count(snapshot, encoder0)
        actual_delta = final - baseline
        expected_delta = cycles * 4
        if actual_delta != expected_delta:
            raise RuntimeError(
                f"Expected +{expected_delta} counts, observed {actual_delta:+d} "
                f"(baseline {baseline}, final {final})"
            )
        return actual_delta
    finally:
        if handle.is_running():
            handle.write({do0: 0.0, do1: 0.0})


def parse_args() -> argparse.Namespace:
    """Parse command-line settings for the hardware procedure."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial-number", type=int)
    parser.add_argument("--peripheral-name", default="daq")
    parser.add_argument("--rate-hz", type=float, default=100.0)
    parser.add_argument("--cycles", type=int, default=64)
    parser.add_argument("--transition-timeout-s", type=float, default=2.0)
    parser.add_argument("--scan-timeout-ms", type=int, default=100)
    return parser.parse_args()


def main() -> None:
    """Discover one rev8 DAQ and run its Encoder0 loopback procedure."""
    args = parse_args()
    controller = deimos.Controller(
        "rev8_encoder_loopback_validation",
        str(Path(__file__).parent),
        args.rate_hz,
    )
    controller.loop_method = LoopMethod.efficient()

    candidates = [
        peripheral
        for peripheral in controller.scan(args.scan_timeout_ms)
        if isinstance(peripheral, deimos.peripheral.DeimosDaqRev8)
        and (
            args.serial_number is None
            or peripheral.serial_number == args.serial_number
        )
    ]
    if len(candidates) != 1:
        serials = [peripheral.serial_number for peripheral in candidates]
        raise RuntimeError(
            "Expected exactly one matching DeimosDaqRev8; "
            f"found serial numbers {serials}"
        )

    controller.add_peripheral(args.peripheral_name, candidates[0])
    handle = controller.run_nonblocking()
    try:
        delta = validate_encoder_loopback(
            handle,
            args.peripheral_name,
            args.cycles,
            args.transition_timeout_s,
        )
        print(f"PASS: Encoder0 advanced by {delta} counts")
    finally:
        handle.stop()
        handle.join()


if __name__ == "__main__":
    main()
