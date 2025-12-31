from importlib.metadata import PackageNotFoundError, version

from .deimos import (
    calc,
    dispatcher,
    peripheral,
    socket,
    Controller,
    Overflow,
    Termination,
    LossOfContactPolicy,
    LoopMethod,
)

try:
    __version__ = version("deimos-daq")
except PackageNotFoundError:
    __version__ = "0.0.0"

__all__ = [
    "calc",
    "dispatcher",
    "peripheral",
    "socket",
    "Controller",
    "Overflow",
    "Termination",
    "LossOfContactPolicy",
    "LoopMethod",
    "__version__",
]
