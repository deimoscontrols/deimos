from typing import ClassVar, Protocol, Self

from . import calc as calc
from . import dispatcher as dispatcher
from . import peripheral as peripheral
from . import socket as socket

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
            wait_for_ready: Block until the controller has calculated and published its
                first latest-value snapshot.
        """
        ...
    def scan(self, timeout_ms: int = 10) -> list[PeripheralLike]:
        """Scan the local network (and any other attached sockets)
        for available peripherals."""
        ...
    def available_inputs(self) -> list[str]:
        """List peripheral inputs that can be written manually."""
        ...
    def graphviz_dot(self) -> str:
        """Render the current calc expression graph as Graphviz DOT text."""
        ...
    def add_peripheral(self, name: str, p: PeripheralLike) -> None:
        """Register a peripheral with the control program"""
        ...
    def attach_hootl_driver(
        self,
        peripheral_name: str,
        transport: peripheral.HootlTransport,
        end_epoch_ns: int | None = None,
    ) -> peripheral.HootlRunHandle:
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
