from typing import Protocol, Self

from .deimos import Overflow

class DispatcherLike(Protocol):
    def to_json(self) -> str: ...

class _DispatcherBase:
    def to_json(self) -> str: ...
    @classmethod
    def from_json(cls, s: str) -> Self: ...

class CsvDispatcher(_DispatcherBase):
    """A fixed-width CSV data target backed by a pre-sized file."""
    def __init__(
        self,
        chunk_size_megabytes: int,
        overflow_behavior: Overflow,
    ) -> None: ...

class TimescaleDbDispatcher(_DispatcherBase):
    """Write control-loop rows to a TimescaleDB/PostgreSQL table."""
    def __init__(
        self,
        dbname: str,
        host: str,
        user: str,
        token_name: str,
        buffer_time_ns: int,
        retention_time_hours: int,
    ) -> None: ...

class DataFrameHandle:
    """Shared handle for reading collected dataframe data."""
    def columns(self) -> dict[str, list[float]]: ...
    def time(self) -> list[str]: ...
    def timestamp(self) -> list[int]: ...

class DataFrameDispatcher(_DispatcherBase):
    """Store collected data in in-memory columns."""
    def __init__(
        self,
        max_size_megabytes: int,
        overflow_behavior: Overflow,
    ) -> None: ...
    def handle(self) -> DataFrameHandle: ...

class LatestValueDispatcher(_DispatcherBase):
    """Dispatcher that always keeps the latest row available."""
    def __init__(self) -> None: ...

class ChannelFilter(_DispatcherBase):
    """Wrap another dispatcher and forward a subset of channels by name."""
    def __init__(
        self,
        inner: DispatcherLike,
        channels: list[str],
    ) -> None: ...

class DecimationDispatcher(_DispatcherBase):
    """Wrap another dispatcher and forward every Nth row."""
    def __init__(
        self,
        inner: DispatcherLike,
        nth: int,
    ) -> None: ...

class LowPassDispatcher(_DispatcherBase):
    """Wrap another dispatcher and apply a low-pass filter per channel."""
    def __init__(
        self,
        inner: DispatcherLike,
        cutoff_hz: float,
    ) -> None: ...

__all__ = [
    "ChannelFilter",
    "CsvDispatcher",
    "DataFrameDispatcher",
    "DataFrameHandle",
    "DecimationDispatcher",
    "LatestValueDispatcher",
    "LowPassDispatcher",
    "TimescaleDbDispatcher",
]
