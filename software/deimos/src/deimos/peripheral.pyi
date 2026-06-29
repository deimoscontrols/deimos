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
