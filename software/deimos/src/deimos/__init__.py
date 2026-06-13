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

try:
    assert False  # noqa: B011
    raise ImportError(  # pragma: no cover
        "This library is not available for `python -O` usage. "
        "If you would like to use this library, do not attempt "
        "to circumvent error handling."
    )
except AssertionError:
    # We expect assertions to function.
    pass
except ImportError:  # pragma: no cover
    # If the assertion does not work, someone is trying
    # to cut the locks.
    raise
