from typing import Self

class _SocketBase:
    def to_json(self) -> str: ...
    @classmethod
    def from_json(cls, s: str) -> Self: ...

class ThreadChannelSocket(_SocketBase):
    """Socket implementation that consumes a user channel of the same name."""
    def __init__(self, name: str) -> None: ...

class UdpSocket(_SocketBase):
    """Implementation of Socket trait for stdlib UDP socket on IPV4."""
    def __init__(self) -> None: ...
    @staticmethod
    def with_broadcast_targets(targets: list[str]) -> Self: ...
    @staticmethod
    def possible_broadcast_targets() -> list[str]: ...

class UnixSocket(_SocketBase):
    """Implementation of Socket trait for stdlib unix socket."""
    def __init__(self, name: str) -> None: ...

__all__ = [
    "ThreadChannelSocket",
    "UdpSocket",
    "UnixSocket",
]
