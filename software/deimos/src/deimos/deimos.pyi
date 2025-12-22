from types import ModuleType
from typing import ClassVar, Protocol, Self

class CalcLike(Protocol):
    def to_json(self) -> str: ...

class DispatcherLike(Protocol):
    def to_json(self) -> str: ...

class PeripheralLike(Protocol):
    serial_number: int
    def to_json(self) -> str: ...

class SocketLike(Protocol):
    def to_json(self) -> str: ...

class Overflow:
    Wrap: ClassVar[Overflow]
    NewFile: ClassVar[Overflow]
    Error: ClassVar[Overflow]

class Snapshot:
    @property
    def system_time(self) -> str: ...
    @property
    def timestamp(self) -> int: ...
    @property
    def values(self) -> dict[str, float]: ...

class RunHandle:
    def stop(self) -> None: ...
    def is_running(self) -> bool: ...
    def join(self) -> str: ...
    def latest_row(self) -> tuple[str, int, list[float]]: ...
    def headers(self) -> list[str]: ...
    def read(self) -> Snapshot: ...

class Controller:
    def __init__(self, op_name: str, op_dir: str, rate_hz: float) -> None: ...
    def run(self) -> str: ...
    def run_nonblocking(self) -> RunHandle: ...
    def scan(self, timeout_ms: int = 10) -> list[PeripheralLike]: ...
    def add_peripheral(self, name: str, p: PeripheralLike) -> None: ...
    def add_calc(self, name: str, calc: CalcLike) -> None: ...
    def add_dispatcher(self, dispatcher: DispatcherLike) -> None: ...
    def add_socket(self, socket: SocketLike) -> None: ...
    def clear_calcs(self) -> None: ...
    def clear_peripherals(self) -> None: ...
    def clear_dispatchers(self) -> None: ...
    def clear_sockets(self) -> None: ...
    @property
    def op_name(self) -> str: ...
    @op_name.setter
    def op_name(self, v: str) -> None: ...
    @property
    def op_dir(self) -> str: ...
    @op_dir.setter
    def op_dir(self, v: str) -> None: ...
    @property
    def dt_ns(self) -> int: ...
    @dt_ns.setter
    def dt_ns(self, v: int) -> None: ...
    @property
    def rate_hz(self) -> float: ...
    @rate_hz.setter
    def rate_hz(self, v: float) -> None: ...
    @property
    def peripheral_loss_of_contact_limit(self) -> int: ...
    @peripheral_loss_of_contact_limit.setter
    def peripheral_loss_of_contact_limit(self, v: int) -> None: ...
    @property
    def controller_loss_of_contact_limit(self) -> int: ...
    @controller_loss_of_contact_limit.setter
    def controller_loss_of_contact_limit(self, v: int) -> None: ...

class _CalcBase:
    def to_json(self) -> str: ...
    @classmethod
    def from_json(cls, s: str) -> Self: ...

class _DispatcherBase:
    def to_json(self) -> str: ...
    @classmethod
    def from_json(cls, s: str) -> Self: ...

class _PeripheralBase:
    serial_number: int
    def to_json(self) -> str: ...
    @classmethod
    def from_json(cls, s: str) -> Self: ...

class _SocketBase:
    def to_json(self) -> str: ...
    @classmethod
    def from_json(cls, s: str) -> Self: ...

class _CalcModule(ModuleType):
    class Affine(_CalcBase):
        def __init__(
            self,
            input_name: str,
            slope: float,
            offset: float,
            save_outputs: bool,
        ) -> None: ...

    class Butter2(_CalcBase):
        def __init__(
            self, input_name: str, cutoff_hz: float, save_outputs: bool
        ) -> None: ...

    class Constant(_CalcBase):
        def __init__(self, y: float, save_outputs: bool) -> None: ...

    class InverseAffine(_CalcBase):
        def __init__(
            self,
            input_name: str,
            slope: float,
            offset: float,
            save_outputs: bool,
        ) -> None: ...

    class Pid(_CalcBase):
        def __init__(
            self,
            measurement_name: str,
            setpoint_name: str,
            kp: float,
            ki: float,
            kd: float,
            max_integral: float,
            save_outputs: bool,
        ) -> None: ...

    class Polynomial(_CalcBase):
        def __init__(
            self,
            input_name: str,
            coefficients: list[float],
            note: str,
            save_outputs: bool,
        ) -> None: ...

    class RtdPt100(_CalcBase):
        def __init__(self, resistance_name: str, save_outputs: bool) -> None: ...

    class Sin(_CalcBase):
        def __init__(
            self,
            period_s: float,
            offset_s: float,
            low: float,
            high: float,
            save_outputs: bool,
        ) -> None: ...

    class TcKtype(_CalcBase):
        def __init__(
            self,
            voltage_name: str,
            cold_junction_temperature_name: str,
            save_outputs: bool,
        ) -> None: ...

calc: _CalcModule

class _PeripheralModule(ModuleType):
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

    class HootlMockupPeripheral(_PeripheralBase):
        def __init__(
            self,
            inner: PeripheralLike,
            configuring_timeout_ms: int = 250,
            end_epoch_ns: int | None = None,
        ) -> None: ...

    class MockupTransport:
        @staticmethod
        def thread_channel(name: str) -> Self: ...

        @staticmethod
        def unix_socket(name: str) -> Self: ...

    class MockupDriver:
        def __init__(
            self,
            inner: PeripheralLike,
            transport: MockupTransport,
            end_epoch_ns: int | None = None,
        ) -> None: ...
        def run_with(self, controller: Controller) -> None: ...

peripheral: _PeripheralModule

class _SocketModule(ModuleType):
    class UnixSocket(_SocketBase):
        def __init__(self, name: str) -> None: ...

    class UdpSocket(_SocketBase):
        def __init__(self) -> None: ...

    class ThreadChannelSocket(_SocketBase):
        def __init__(self, name: str) -> None: ...

socket: _SocketModule

class _DispatcherModule(ModuleType):
    class CsvDispatcher(_DispatcherBase):
        def __init__(
            self,
            chunk_size_megabytes: int,
            overflow_behavior: Overflow,
        ) -> None: ...

    class TimescaleDbDispatcher(_DispatcherBase):
        def __init__(
            self,
            dbname: str,
            host: str,
            user: str,
            token_name: str,
            buffer_time_ns: int,
            retention_time_hours: int,
        ) -> None: ...

    class DataFrameDispatcher(_DispatcherBase):
        def __init__(
            self,
            max_size_megabytes: int,
            overflow_behavior: Overflow,
        ) -> None: ...

    class LatestValueDispatcher(_DispatcherBase):
        def __init__(self) -> None: ...

    class ChannelFilter(_DispatcherBase):
        def __init__(
            self,
            inner: DispatcherLike,
            channels: list[str],
        ) -> None: ...

    class DecimationDispatcher(_DispatcherBase):
        def __init__(
            self,
            inner: DispatcherLike,
            nth: int,
        ) -> None: ...

    class LowPassDispatcher(_DispatcherBase):
        def __init__(
            self,
            inner: DispatcherLike,
            cutoff_hz: float,
        ) -> None: ...

dispatcher: _DispatcherModule
