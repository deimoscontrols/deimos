from types import ModuleType
from typing import ClassVar, Protocol, Self

class CalcLike(Protocol):
    def to_json(self) -> str: ...

class DispatcherLike(Protocol):
    def to_json(self) -> str: ...

class Overflow:
    Wrap: ClassVar[Overflow]
    NewFile: ClassVar[Overflow]
    Error: ClassVar[Overflow]

class Peripheral:
    serial_number: int

    Experimental: ClassVar[type[_PeripheralExperimental]]
    Unknown: ClassVar[type[_PeripheralUnknown]]
    AnalogIRev2: ClassVar[type[_PeripheralAnalogIRev2]]
    AnalogIRev3: ClassVar[type[_PeripheralAnalogIRev3]]
    AnalogIRev4: ClassVar[type[_PeripheralAnalogIRev4]]
    DeimosDaqRev5: ClassVar[type[_PeripheralDeimosDaqRev5]]
    DeimosDaqRev6: ClassVar[type[_PeripheralDeimosDaqRev6]]

class _PeripheralExperimental(Peripheral):
    def __init__(self, serial_number: int) -> None: ...

class _PeripheralUnknown(Peripheral):
    def __init__(self, serial_number: int) -> None: ...

class _PeripheralAnalogIRev2(Peripheral):
    def __init__(self, serial_number: int) -> None: ...

class _PeripheralAnalogIRev3(Peripheral):
    def __init__(self, serial_number: int) -> None: ...

class _PeripheralAnalogIRev4(Peripheral):
    def __init__(self, serial_number: int) -> None: ...

class _PeripheralDeimosDaqRev5(Peripheral):
    def __init__(self, serial_number: int) -> None: ...

class _PeripheralDeimosDaqRev6(Peripheral):
    def __init__(self, serial_number: int) -> None: ...

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
    def scan(self, timeout_ms: int = 10) -> list[Peripheral]: ...
    def add_peripheral(
        self,
        name: str,
        p: Peripheral,
        sn: int | None = None,
    ) -> None: ...
    def add_calc(self, name: str, calc: CalcLike) -> None: ...
    def add_dispatcher(self, dispatcher: DispatcherLike) -> None: ...
    def add_csv_dispatcher(
        self,
        chunk_size_megabytes: int = 100,
        overflow_behavior: Overflow = Overflow.Wrap,
    ) -> None: ...
    def add_timescaledb_dispatcher(
        self,
        dbname: str,
        host: str,
        user: str,
        token_name: str,
        retention_time_hours: int,
    ) -> None: ...
    def add_unix_socket(self, name: str) -> None: ...
    def add_udp_socket(self) -> None: ...
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
