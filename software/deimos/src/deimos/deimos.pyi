from types import ModuleType
from typing import ClassVar, Protocol, Self, Literal

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
    """Choice of behavior when the current file is full.

    Wrap: Wrap back to the beginning of the file and overwrite, starting with the oldest
          data.
    NewFile: Create a new file.
    Error: Error on overflow if neither wrapping nor creating a new file is viable.
    """

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
    def stop(self) -> None:
        """Signal the controller to stop."""
        ...
    def is_running(self) -> bool:
        """Check if the controller thread is still running."""
        ...
    def join(self) -> str:
        """Wait for the controller thread to finish."""
        ...
    def latest_row(self) -> tuple[str, int, list[float]]:
        """Get the latest row: (system_time, timestamp, channel_values)."""
        ...
    def headers(self) -> list[str]:
        """Column headers including timestamp/time."""
        ...
    def read(self) -> Snapshot:
        """Read the latest row mapped to header names."""
        ...

class Controller:
    def __init__(self, op_name: str, op_dir: str, rate_hz: float) -> None:
        """Build a new controller.

        `rate_hz` will be rounded to the nearest nanosecond when converted
        to the sample period.

        This constructor does not run the controller or attach any peripherals.
        """
        ...
    def run(self) -> str:
        """Run the control program."""
        ...
    def run_nonblocking(self) -> RunHandle:
        """Run the control program on a separate thread and return a handle
        for coordination."""
        ...
    def scan(self, timeout_ms: int = 10) -> list[PeripheralLike]:
        """Scan the local network (and any other attached sockets)
        for available peripherals."""
        ...
    def add_peripheral(self, name: str, p: PeripheralLike) -> None: ...
    def add_calc(self, name: str, calc: CalcLike) -> None: ...
    def add_dispatcher(self, dispatcher: DispatcherLike) -> None:
        """Add a dispatcher via a JSON-serializable dispatcher instance."""
        ...
    def add_socket(self, socket: SocketLike) -> None:
        """Add a socket via a JSON-serializable socket instance."""
        ...
    def clear_calcs(self) -> None:
        """Remove all calcs."""
        ...
    def clear_peripherals(self) -> None:
        """Remove all peripherals."""
        ...
    def clear_dispatchers(self) -> None:
        """Remove all dispatchers."""
        ...
    def clear_sockets(self) -> None:
        """Remove all sockets."""
        ...
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
    @property
    def termination_criteria(self) -> list[context.Termination]: ...
    @termination_criteria.setter
    def termination_criteria(self, v: list[context.Termination]) -> None: ...
    @property
    def loss_of_contact_policy(self) -> context.LossOfContactPolicy: ...
    @loss_of_contact_policy.setter
    def loss_of_contact_policy(self, v: context.LossOfContactPolicy) -> None: ...

class _CalcBase:
    def to_json(self) -> str:
        """Serialize to typetagged JSON so Python can pass into trait handoff."""
        ...
    @classmethod
    def from_json(cls, s: str) -> Self:
        """Deserialize from typetagged JSON."""
        ...

class _DispatcherBase:
    def to_json(self) -> str:
        """Serialize to typetagged JSON so Python can pass into trait handoff."""
        ...
    @classmethod
    def from_json(cls, s: str) -> Self:
        """Deserialize from typetagged JSON."""
        ...

class _PeripheralBase:
    serial_number: int
    def to_json(self) -> str:
        """Serialize to typetagged JSON so Python can pass into trait handoff."""
        ...
    @classmethod
    def from_json(cls, s: str) -> Self:
        """Deserialize from typetagged JSON."""
        ...

class _SocketBase:
    def to_json(self) -> str:
        """Serialize to typetagged JSON so Python can pass into trait handoff."""
        ...
    @classmethod
    def from_json(cls, s: str) -> Self:
        """Deserialize from typetagged JSON."""
        ...

InterpMethod = Literal["linear", "left", "right", "nearest"]
Time = list[float]
Value = list[float]
Name = str
Sequence = dict[Name, tuple[Time, Value, InterpMethod]]
TimeoutTargetState = str

class _CalcModule(ModuleType):
    class Affine(_CalcBase):
        """A slope and offset, y = ax + b."""
        def __init__(
            self,
            input_name: str,
            slope: float,
            offset: float,
            save_outputs: bool,
        ) -> None: ...

    class Butter2(_CalcBase):
        """Single-input, single-output Butterworth low-pass
        filter implemented with `flaw::butter2`."""
        def __init__(
            self, input_name: str, cutoff_hz: float, save_outputs: bool
        ) -> None: ...

    class Constant(_CalcBase):
        """Simplest calc that does anything at all."""
        def __init__(self, y: float, save_outputs: bool) -> None: ...

    class InverseAffine(_CalcBase):
        """Derive input voltage from linear amplifier reading.

        First subtracts the output offset, then divides by the slope.
        """
        def __init__(
            self,
            input_name: str,
            slope: float,
            offset: float,
            save_outputs: bool,
        ) -> None: ...

    class Pid(_CalcBase):
        """A PID controller with simple saturation for anti-windup."""
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
        """Polynomial calibration: y = c0 + c1*x + c2*x^2 + ...

        with an attached note that should include traceability info
        like a sensor serial number.
        Coefficients ordered by increasing polynomial order.
        """
        def __init__(
            self,
            input_name: str,
            coefficients: list[float],
            note: str,
            save_outputs: bool,
        ) -> None: ...

    class RtdPt100(_CalcBase):
        """Derive input voltage from amplifier output."""
        def __init__(self, resistance_name: str, save_outputs: bool) -> None: ...

    class Sin(_CalcBase):
        """Sin wave between `low` and `high` with a period of `period_s`
        and phase offset of `offset_s`."""
        def __init__(
            self,
            period_s: float,
            offset_s: float,
            low: float,
            high: float,
            save_outputs: bool,
        ) -> None: ...

    class TcKtype(_CalcBase):
        """Calculate a K-type thermocouple's temperature reading in (K) from voltage,
        using the ITS-90 method for cold-junction correction.
        """
        def __init__(
            self,
            voltage_name: str,
            cold_junction_temperature_name: str,
            save_outputs: bool,
        ) -> None: ...

    class SequenceMachineInner(_CalcBase):
        """A lookup-table sequence machine that follows a set procedure during
        each sequence, and transitions between sequences based on set criteria.

        Unlike most calcs, the names of the inputs and outputs of this calc
        are not known at compile-time, and are assembled from inputs instead.
        """
        def __init__(self, entry: str) -> None: ...
        @staticmethod
        def load_folder(path: str) -> Self:
            """Read a configuration json and sequence CSV files from a folder.

            The folder must contain one json representing a [MachineCfg] and
            some number of CSV files each representing a [Sequence].
            """
            ...
        def save_folder(self, path: str) -> None:
            """Save a configuration json and sequence CSV files to a folder."""
            ...
        def get_entry(self) -> str: ...
        def set_entry(self, entry: str) -> None: ...
        def get_link_folder(self) -> str | None: ...
        def set_link_folder(self, link_folder: str | None) -> None: ...
        def get_timeout(self, sequence: str) -> TimeoutTargetState | None: ...
        def set_timeout(
            self, sequence: str, target: TimeoutTargetState | None
        ) -> None: ...
        def add_sequence(
            self,
            name: str,
            tables: Sequence,
            timeout: TimeoutTargetState | None,
        ) -> None: ...
        def add_constant_thresh_transition(
            self,
            source_target: tuple[str, str],
            channel: str,
            op: tuple[str, float],
            threshold: float,
        ) -> None:
            """Add a constant threshold transition for a sequence."""
            ...
        def add_channel_thresh_transition(
            self,
            source_target: tuple[str, str],
            channel: str,
            op: tuple[str, float],
            threshold_channel: str,
        ) -> None:
            """Add a channel threshold transition for a sequence."""
            ...
        def add_lookup_thresh_transition(
            self,
            source_target: tuple[str, str],
            channel: str,
            op: tuple[str, float],
            threshold_lookup: tuple[list[float], list[float], str],
        ) -> None:
            """Add a lookup threshold transition for a sequence."""
            ...

calc: _CalcModule

class MockupTransport:
    @staticmethod
    def thread_channel(name: str) -> Self: ...
    @staticmethod
    def unix_socket(name: str) -> Self: ...
    @staticmethod
    def udp() -> Self:
        """UDP transport bound to PERIPHERAL_RX_PORT (one mockup at a time)."""
        ...

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
        """Peripheral wrapper that emits mock outputs
        using a state machine that imitates hardware peripheral behavior."""
        def __init__(
            self,
            inner: PeripheralLike,
            configuring_timeout_ms: int = 250,
            end_epoch_ns: int | None = None,
        ) -> None: ...

    MockupTransport = MockupTransport

    class HootlDriver:
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
        """Implementation of Socket trait for stdlib unix socket."""
        def __init__(self, name: str) -> None: ...

    class UdpSocket(_SocketBase):
        """Implementation of Socket trait for stdlib UDP socket on IPV4."""
        def __init__(self) -> None: ...

    class ThreadChannelSocket(_SocketBase):
        """Socket implementation that communicates over a named user channel."""
        def __init__(self, name: str) -> None: ...

socket: _SocketModule

class _DispatcherModule(ModuleType):
    class CsvDispatcher(_DispatcherBase):
        """A plain-text CSV data target, which uses a pre-sized file
        to prevent sudden increases in write latency during file resizing.

        On reaching the end of the configured data size, it can be configured to either
        * Wrap (and start overwriting the existing data from the beginning),
        * Start a new file, or
        * Error, logging that the file is full and closing comms with the controller.

        depending on the appropriate response for the task at hand.

        Each line in this CSV format is fixed-width, meaning the line length will not
        change with each line. As a result, it is possible to read to a specific time
        or line in O(1) time by simple arithmetic rather than by reading the whole file.
        This guarantee will be broken when crossing from year 9999 to year 10000
        and so on, or if non-finite values are encountered in measurements, calcs,
        or metrics.

        Writes to disk on a separate thread to avoid blocking the control loop.
        """
        def __init__(
            self,
            chunk_size_megabytes: int,
            overflow_behavior: Overflow,
        ) -> None: ...

    class TimescaleDbDispatcher(_DispatcherBase):
        """Either reuse or create a new table in a TimescaleDB postgres
        database and write to that table.

        Depending on the buffer time window, this dispatcher will either use a simple
        INSERT query to send one row of values at a time, or use COPY INTO syntax
        to send buffered batches of rows all at once.

        Performs database transactions on on a separate thread to avoid blocking the
        control loop.

        Does not support TLS, and as a result, is recommended for communication with
        databases on the same internal network, not on the open web.
        """
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
        """Store collected data in in-memory columns, moving columns into
        a dataframe behind a shared reference at termination.

        To avoid deadlocks, the dataframe is not updated until after
        the run is complete. The dataframe is cleared at the start of each run.
        """
        def __init__(
            self,
            max_size_megabytes: int,
            overflow_behavior: Overflow,
        ) -> None: ...

    class LatestValueDispatcher(_DispatcherBase):
        """Dispatcher that always keeps the latest row available via a shared handle."""
        def __init__(self) -> None: ...

    class ChannelFilter(_DispatcherBase):
        """Wraps another dispatcher and forwards a subset of channels by name."""
        def __init__(
            self,
            inner: DispatcherLike,
            channels: list[str],
        ) -> None: ...

    class DecimationDispatcher(_DispatcherBase):
        """Wraps another dispatcher and forwards every Nth row."""
        def __init__(
            self,
            inner: DispatcherLike,
            nth: int,
        ) -> None: ...

    class LowPassDispatcher(_DispatcherBase):
        """Wraps another dispatcher and applies a 2nd order Butterworth low-pass
        filter per channel."""
        def __init__(
            self,
            inner: DispatcherLike,
            cutoff_hz: float,
        ) -> None: ...

dispatcher: _DispatcherModule

class _ContextModule(ModuleType):
    class Termination:
        """Criteria for exiting the control program.

        Timeout: A duration after which the control program should terminate.
        The controller will use the monotonic clock, not system realtime clock,
        to determine when this threshold occurs; as a result, if used for
        scheduling relative to a "realtime" date or time, it will accumulate
        some error as the monotonic clock drifts.

        Scheduled: Schedule termination at a specific "realtime" date or time.
        """
        @staticmethod
        def timeout_ms(ms: int) -> Self: ...
        @staticmethod
        def timeout_ns(ns: int) -> Self: ...
        @staticmethod
        def scheduled_epoch_ns(ns: int) -> Self: ...

    class LossOfContactPolicy:
        """Response to losing contact with a peripheral.

        Terminate: Terminate the control program.
        """

        Terminate: ClassVar[Self]

context: _ContextModule
