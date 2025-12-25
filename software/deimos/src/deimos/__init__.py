from .deimos import (
    calc,
    dispatcher,
    peripheral,
    socket,
    context,
    Controller,
    Overflow,
)

__all__ = [
    "calc",
    "dispatcher",
    "peripheral",
    "socket",
    "context",
    "Controller",
    "Overflow",
    "Termination",
    "LossOfContactPolicy",
]

class SequenceMachine(calc.SequenceMachineInner):
    def __init__(self, entry: str):
        super().__init__(entry)
