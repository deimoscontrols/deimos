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
    Wrap: ClassVar[Self]
    """Overwrite oldest data first."""

    NewFile: ClassVar[Self]
    """Create a new shard."""

    Error: ClassVar[Self]
    """Emit an error and shut down."""

    @staticmethod
    def wrap() -> Wrap:
        """Wrap back to the beginning of the file."""
        ...
    @staticmethod
    def new_file() -> NewFile:
        """Create a new file on overflow."""
        ...
    @staticmethod
    def error() -> Error:
        """Error on overflow."""
        ...

class LoopMethod:
    Performant: ClassVar[Self]
    """
    Use 100% of a CPU to protect timing.
    This increases maximum usable control frequency.
    """

    Efficient: ClassVar[Self]
    """
    Use operating system scheduling to reduce CPU usage
    at the expense of degraded cycle performance.
    Typically viable up to about 50Hz control rate.
    """

    @staticmethod
    def performant() -> Performant:
        """Use 100% of a CPU to protect timing."""
        ...
    @staticmethod
    def efficient() -> Efficient:
        """Use operating system scheduling for lower CPU usage."""
        ...

class Termination:
    Timeout: ClassVar[Self]
    """End the control program after some duration from the start of the first cycle."""

    Scheduled: ClassVar[Self]
    """End the control program at a specific UTC system time."""

    @staticmethod
    def timeout_s(s: float) -> Timeout:
        """End after `s` seconds from the start of the first cycle."""
        ...
    @staticmethod
    def scheduled_epoch_ns(ns: int) -> Scheduled:
        """End at a specified absolute system time in UTC nanoseconds."""
        ...

class LossOfContactPolicy:
    Terminate: ClassVar[Self]
    """Terminate the control program."""

    Reconnect: ClassVar[Self]
    """Attempt to reconnect to the peripheral"""

    @staticmethod
    def terminate() -> Terminate:
        """Construct a policy that terminates the control program."""
        ...
    @staticmethod
    def reconnect_s(timeout_s: float) -> Reconnect:
        """Construct a reconnect policy with a timeout in seconds."""
        ...
    @staticmethod
    def reconnect_indefinite() -> Reconnect:
        """Construct a reconnect policy with no timeout."""
        ...

class Snapshot:
    @property
    def system_time(self) -> str: ...
    """End-of-cycle UTC system time in RFC3339 format with nanoseconds."""
    @property
    def timestamp(self) -> int: ...
    """End-of-cycle time in monotonic nanoseconds since start of control program."""
    @property
    def values(self) -> dict[str, float]: ...
    """Latest values by channel name."""

class RunHandle:
    def stop(self) -> None:
        """Signal the controller to stop."""
        ...
    def is_running(self) -> bool:
        """Check if the controller thread is still running."""
        ...
    def is_ready(self) -> bool:
        """Check if the controller has completed its first cycle."""
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
    def available_inputs(self) -> list[str]:
        """List peripheral inputs that can be written manually."""
        ...
    def write(self, values: dict[str, float]) -> None:
        """Write values to peripheral inputs not driven by calcs."""
        ...

class Controller:
    """
    The control program that communicates with hardware peripherals,
    runs calculations, and dispatches data.
    """

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
    def run_nonblocking(
        self,
        latest_value_cutoff_freq: float | None = None,
        wait_for_ready: bool = True,
    ) -> RunHandle:
        """Run the control program on a separate thread and return a handle
        for coordination.

        Args:
            latest_value_cutoff_freq: Optional second-order Butterworth low-pass filter
                                      cutoff frequency to apply to latest-value data.
                                      If the selected frequency is outside the viable
                                      range for the filter, the cutoff frequency will
                                      be clamped to the viable bounds and a warning
                                      will be emitted.
            wait_for_ready: Block until the controller has completed its first cycle.
        """
        ...
    def scan(self, timeout_ms: int = 10) -> list[PeripheralLike]:
        """Scan the local network (and any other attached sockets)
        for available peripherals."""
        ...
    def available_inputs(self) -> list[str]:
        """List peripheral inputs that can be written manually."""
        ...
    def add_peripheral(self, name: str, p: PeripheralLike) -> None:
        """Register a peripheral with the control program"""
        ...
    def attach_hootl_driver(
        self,
        peripheral_name: str,
        transport: HootlTransport,
        end_epoch_ns: int | None = None,
    ) -> HootlRunHandle:
        """Wrap an existing peripheral with a hootl wrapper and start its driver."""
        ...
    def add_calc(self, name: str, calc: CalcLike) -> None:
        """Add a calc to the expression graph that runs on every cycle"""
        ...
    def add_dataframe_dispatcher(
        self,
        name: str,
        max_size_megabytes: int,
        overflow_behavior: Overflow,
    ) -> dispatcher.DataFrameHandle:
        """Add an in-memory dataframe dispatcher and return its shared handle."""
        ...
    def add_dispatcher(self, name: str, dispatcher: DispatcherLike) -> None:
        """Add a dispatcher via a JSON-serializable dispatcher instance."""
        ...
    def dispatcher_names(self) -> list[str]: ...
    def remove_dispatcher(self, name: str) -> bool: ...
    def add_socket(self, name: str, socket: SocketLike) -> None:
        """Add a socket via a JSON-serializable socket instance."""
        ...
    def remove_socket(self, name: str) -> bool:
        """Remove a socket by name."""
        ...
    def set_peripheral_input_source(self, input_field: str, source_field: str) -> None:
        """Connect an entry in the calc graph to a
        command to be sent to the peripheral."""
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
    def op_name(self) -> str:
        """
        The name of the operation.
        Used to set database table names, set log and data file names, etc.
        """
        ...
    @op_name.setter
    def op_name(self, v: str) -> None: ...
    @property
    def op_dir(self) -> str:
        """
        The directory where this operation's logs and other data will be placed,
        and where calcs with linked configuration (like a SequenceMachine) can
        find their linked files or folders by relative path.
        """
        ...
    @op_dir.setter
    def op_dir(self, v: str) -> None: ...
    @property
    def dt_ns(self) -> int:
        """[ns] control program cycle period."""
        ...
    @dt_ns.setter
    def dt_ns(self, v: int) -> None: ...
    @property
    def rate_hz(self) -> float:
        """[Hz] control program cycle rate."""
        ...
    @rate_hz.setter
    def rate_hz(self, v: float) -> None: ...
    @property
    def peripheral_loss_of_contact_limit(self) -> int:
        """Number of missed packets from the controller that indicates disconnection."""
        ...
    @peripheral_loss_of_contact_limit.setter
    def peripheral_loss_of_contact_limit(self, v: int) -> None: ...
    @property
    def controller_loss_of_contact_limit(self) -> int:
        """Number of missed packets from a peripheral that indicates disconnection."""
        ...
    @controller_loss_of_contact_limit.setter
    def controller_loss_of_contact_limit(self, v: int) -> None: ...
    @property
    def termination_criteria(self) -> Termination | None:
        """Criteria for exiting the control program."""
        ...
    @termination_criteria.setter
    def termination_criteria(self, v: Termination | None) -> None: ...
    @property
    def loss_of_contact_policy(self) -> LossOfContactPolicy:
        """
        The response of the control program when a peripheral disconnects during run.
        """
        ...

    @loss_of_contact_policy.setter
    def loss_of_contact_policy(self, v: LossOfContactPolicy) -> None: ...
    @property
    def loop_method(self) -> LoopMethod:
        """
        The loop waiting method for the controller.

        Busywaiting is performant, but inefficient;
        relying on the operating system for scheduling is efficient, but not performant.
        """
        ...
    @loop_method.setter
    def loop_method(self, v: LoopMethod) -> None: ...
    @property
    def enable_manual_inputs(self) -> bool:
        """Whether manual input overrides should be applied during the control loop."""
        ...
    @enable_manual_inputs.setter
    def enable_manual_inputs(self, v: bool) -> None: ...

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
        def get_entry(self) -> str:
            """The sequence that is the entrypoint for the machine."""
            ...

        def set_entry(self, entry: str) -> None:
            """Set the sequence that is the entrypoint for the machine."""
            ...

        def get_link_folder(self) -> str | None:
            """
            The folder, by relative path from `op_dir`,
            that this machine will automatically reload
            configuration from during init, if any.
            """
            ...

        def set_link_folder(self, link_folder: str | None) -> None:
            """
            Set the folder, by relative path from `op_dir`,
            that this machine will automatically reload
            configuration from during init, if any.
            """
            ...

        def get_timeout(self, sequence: str) -> TimeoutTargetState | None:
            """
            The state that this sequence will transition to when it times out.
            If None, this sequence will loop.
            """
            ...

        def set_timeout(self, sequence: str, target: TimeoutTargetState | None) -> None:
            """
            Set the state that this sequence will transition to when it times out.
            If None, this sequence will loop.
            """
            ...

        def add_sequence(
            self,
            name: str,
            tables: Sequence,
            timeout: TimeoutTargetState | None,
        ) -> None:
            """Add a sequence state to the machine."""
            ...

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
        """UDP transport bound to PERIPHERAL_RX_PORT (one hootl driver at a time)."""
        ...

class HootlRunHandle:
    def stop(self) -> None: ...
    def is_running(self) -> bool: ...
    def join(self) -> None: ...
    def __enter__(self) -> Self: ...
    def __exit__(self, exc_type: object, exc: object, tb: object) -> bool: ...

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

    class HootlPeripheral(_PeripheralBase):
        """Peripheral wrapper that emits mock outputs using driver-owned state.

        Note: attach this via Controller.attach_hootl_driver to keep the shared
        driver state intact; JSON roundtrips reset the link.
        """
        def __init__(self, inner: PeripheralLike) -> None: ...

    HootlTransport = HootlTransport

    HootlRunHandle = HootlRunHandle

    class HootlDriver:
        def __init__(
            self,
            inner: PeripheralLike,
            transport: HootlTransport,
            end_epoch_ns: int | None = None,
        ) -> None: ...
        """A way to operate a hootl driver from outside the control program."""

        def run_with(self, controller: Controller) -> HootlRunHandle: ...
        """Start the driver attached to this controller."""

peripheral: _PeripheralModule

class _SocketModule(ModuleType):
    class UnixSocket(_SocketBase):
        """Implementation of Socket trait for stdlib unix socket."""
        def __init__(self, name: str) -> None: ...

    class UdpSocket(_SocketBase):
        """Implementation of Socket trait for stdlib UDP socket on IPV4."""
        def __init__(self) -> None: ...

    class ThreadChannelSocket(_SocketBase):
        """Socket implementation that consumes a user channel of the same name.
        Only one peripheral can be connected per thread socket."""
        def __init__(self, name: str) -> None: ...

socket: _SocketModule

class _DispatcherModule(ModuleType):
    class CsvDispatcher(_DispatcherBase):
        """A plain-text CSV data target, which uses a pre-sized file
        to prevent sudden increases in write latency during file resizing.

        On reaching the end of the configured data size, it can be configured to
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
        """
        WARNING: This dispatcher requires a direct link in the backend, and can't be
        initialized properly from Python. Instead, it can be initialized by
        `Controller.add_dataframe_dispatcher`.

        Store collected data in in-memory columns, moving columns into
        a dataframe behind a shared reference at termination.

        To avoid deadlocks, the dataframe is not updated until after
        the run is complete. The dataframe is cleared at the start of each run.
        """
        def __init__(
            self,
            max_size_megabytes: int,
            overflow_behavior: Overflow,
        ) -> None: ...
        def handle(self) -> dispatcher.DataFrameHandle: ...

    class DataFrameHandle:
        """Shared handle for reading collected dataframe data."""
        def columns(self) -> dict[str, list[float]]: ...
        def time(self) -> list[str]: ...
        def timestamp(self) -> list[int]: ...

    class LatestValueDispatcher(_DispatcherBase):
        """
        WARNING: This dispatcher requires a direct link in the backend, and can't be
        initialized properly from Python.

        Dispatcher that always keeps the latest row available via a shared handle.
        """
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
