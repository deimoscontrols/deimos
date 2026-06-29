from typing import Literal, Self

class _CalcBase:
    def to_json(self) -> str: ...
    @classmethod
    def from_json(cls, s: str) -> Self: ...

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
    """Single-input, single-output Butterworth low-pass filter."""
    def __init__(
        self,
        input_name: str,
        cutoff_hz: float,
        save_outputs: bool,
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

    with an attached note that should include traceability info like a sensor
    serial number. Coefficients ordered by increasing polynomial order.
    """
    def __init__(
        self,
        input_name: str,
        coefficients: list[float],
        note: str,
        save_outputs: bool,
    ) -> None: ...

class RtdPt100(_CalcBase):
    """Derive temperature from a Pt100 RTD resistance reading."""
    def __init__(self, resistance_name: str, save_outputs: bool) -> None: ...

class Sin(_CalcBase):
    """Sin wave between `low` and `high` with a period and phase offset."""
    def __init__(
        self,
        period_s: float,
        offset_s: float,
        low: float,
        high: float,
        save_outputs: bool,
    ) -> None: ...

class TcKtype(_CalcBase):
    """Calculate a K-type thermocouple temperature from voltage."""
    def __init__(
        self,
        voltage_name: str,
        cold_junction_temperature_name: str,
        save_outputs: bool,
    ) -> None: ...

InterpMethod = Literal["linear", "left", "right", "nearest"]
Time = list[float]
Value = list[float]
Name = str
Sequence = dict[Name, tuple[Time, Value, InterpMethod]]
TimeoutTargetState = str

class SequenceMachine(_CalcBase):
    """A lookup-table sequence machine with transition criteria."""
    def __init__(self, entry: str) -> None: ...
    @staticmethod
    def load_folder(path: str) -> Self:
        """Read configuration JSON and sequence CSV files from a folder."""
        ...
    def save_folder(self, path: str) -> None:
        """Save configuration JSON and sequence CSV files to a folder."""
        ...
    def graphviz_dot(self) -> str:
        """Render this sequence machine's state graph as Graphviz DOT text."""
        ...
    def get_entry(self) -> str:
        """The sequence that is the entrypoint for the machine."""
        ...
    def set_entry(self, entry: str) -> None:
        """Set the sequence that is the entrypoint for the machine."""
        ...
    def get_link_folder(self) -> str | None:
        """The folder reloaded during init, relative to `op_dir`, if any."""
        ...
    def set_link_folder(self, link_folder: str | None) -> None:
        """Set the folder reloaded during init, relative to `op_dir`, if any."""
        ...
    def get_timeout(self, sequence: str) -> TimeoutTargetState | None:
        """The state this sequence transitions to when it times out."""
        ...
    def set_timeout(self, sequence: str, target: TimeoutTargetState | None) -> None:
        """Set the state this sequence transitions to when it times out."""
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

__all__ = [
    "Affine",
    "Butter2",
    "Constant",
    "InverseAffine",
    "Pid",
    "Polynomial",
    "RtdPt100",
    "SequenceMachine",
    "Sin",
    "TcKtype",
]
