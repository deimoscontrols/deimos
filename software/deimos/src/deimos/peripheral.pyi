from typing import Protocol, Self

from .deimos import Controller

class PeripheralLike(Protocol):
    serial_number: int
    def to_json(self) -> str: ...

class _PeripheralBase:
    serial_number: int
    def to_json(self) -> str: ...
    @classmethod
    def from_json(cls, s: str) -> Self: ...

class AnalogIRev2(_PeripheralBase):
    def __init__(self, serial_number: int) -> None: ...

class AnalogIRev3(_PeripheralBase):
    def __init__(self, serial_number: int) -> None: ...

class AnalogIRev4(_PeripheralBase):
    def __init__(self, serial_number: int) -> None: ...

class DeimosDaqRev5(_PeripheralBase):
    def __init__(self, serial_number: int) -> None: ...

class DeimosDaqRev6(_PeripheralBase):
    def __init__(self, serial_number: int) -> None: ...

class DeimosDaqRev7(_PeripheralBase):
    """Software interface for a Deimos DAQ rev7 peripheral.

    Modbus addresses are zero-based. Multi-register values put the most
    significant register first and use network byte order.

    Input registers (FC04, read 0 with count 75 for a coherent snapshot):
      0..1: ``u32 magic`` (``0xD7000002``)
      2..5: ``u64 metrics.id`` (snapshot count)
      6..9: ``i64 metrics.sent_time_ns``
      10..13: ``u64 metrics.last_input_id``
      14..17: ``i64 metrics.last_input_received_time_ns``
      18..21: ``i64 metrics.cycle_time_margin_ns``
      22..25: ``i64 sample_time_ns``
      26..27: ``f32 module_bus_current_a``
      28..29: ``f32 module_bus_voltage_v``
      30..31: ``f32 board_temperature_k``
      32..39: ``f32[4] current_4_20_a``
      40..45: ``f32[3] rtd_resistance_ohm``
      46..49: ``f32[2] thermocouple_temperature_k``
      50..61: ``f32[6] voltage_v``
      62..65: ``i64 encoder``
      66..69: ``i64 pulse_counter``
      70..73: ``f32[2] frequency_meas``
      74: ``u16 gpio`` (input bits 0..1)

    Holding registers (FC03 reads; FC16 and FC23 write complete fields):
      0..1: R/W ``f32 cycle_rate_hz`` (5..500 Hz)
      2: R/W ``u16 loss_of_contact_limit`` (1..65535 cycles)
      3..4: R ``u32 cycle_period_ns``
      5: R ``u16 loss_of_contact_counter``
      6..13: R/W ``f32[4] pwm_duty_frac`` (0..1)
      14..21: R/W ``u32[4] pwm_frequency_hz`` (nonzero)
      22..25: R/W ``f32[2] dac_v`` (0..2.5 V)
      26: R/W ``u16 gpio`` (output bits 0..3)
      27..30: R/W ``i64 period_delta_ns`` (persistent)
      31..34: R/W ``i64 phase_delta_ns`` (one cycle)
      256..330: R coherent snapshot mirror of input registers 0..74

    FC23 should read address 256 with count 75 while writing one complete
    writable block. Partial in-range reads are valid, but a full snapshot read
    is required for synchronized measurements.
    """

    def __init__(self, serial_number: int) -> None: ...

class HootlTransport:
    @staticmethod
    def thread_channel(name: str) -> Self:
        """A thread channel with this name."""
        ...
    @staticmethod
    def unix_socket(name: str) -> Self:
        """A unix socket with this name."""
        ...
    @staticmethod
    def udp() -> Self:
        """UDP transport bound to PERIPHERAL_RX_PORT."""
        ...

class HootlRunHandle:
    def stop(self) -> None: ...
    def is_running(self) -> bool: ...
    def join(self) -> None: ...
    def __enter__(self) -> Self: ...
    def __exit__(self, exc_type: object, exc: object, tb: object) -> bool: ...

class HootlPeripheral(_PeripheralBase):
    """Peripheral wrapper that emits mock outputs using driver-owned state."""

    ...

class HootlDriver:
    """A way to operate a hootl driver from outside the control program."""
    def __init__(
        self,
        inner: PeripheralLike,
        transport: HootlTransport,
        end_epoch_ns: int | None = None,
    ) -> None: ...
    def run_with(self, controller: Controller) -> HootlRunHandle:
        """Start the driver attached to this controller."""
        ...

__all__ = [
    "AnalogIRev2",
    "AnalogIRev3",
    "AnalogIRev4",
    "DeimosDaqRev5",
    "DeimosDaqRev6",
    "DeimosDaqRev7",
    "HootlDriver",
    "HootlPeripheral",
    "HootlRunHandle",
    "HootlTransport",
]
