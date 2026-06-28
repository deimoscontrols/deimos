# Python API

## Core

::: deimos
    options:
      show_root_heading: true
      show_root_full_path: false
      heading_level: 2
      show_source: false
      members:
        - Controller
        - RunHandle
        - Snapshot
        - LoopMethod
        - LossOfContactPolicy
        - Overflow
        - Termination

## Calcs

::: deimos.calc
    options:
      show_root_heading: true
      show_root_full_path: false
      heading_level: 2
      show_source: false
      members:
        - Affine
        - Butter2
        - Constant
        - InverseAffine
        - Pid
        - Polynomial
        - RtdPt100
        - SequenceMachine
        - Sin
        - TcKtype

## Peripherals

::: deimos.peripheral
    options:
      show_root_heading: true
      show_root_full_path: false
      heading_level: 2
      show_source: false
      members:
        - AnalogIRev2
        - AnalogIRev3
        - AnalogIRev4
        - DeimosDaqRev5
        - DeimosDaqRev6
        - DeimosDaqRev7
        - HootlDriver
        - HootlPeripheral
        - HootlRunHandle
        - HootlTransport

## Sockets

::: deimos.socket
    options:
      show_root_heading: true
      show_root_full_path: false
      heading_level: 2
      show_source: false
      members:
        - ThreadChannelSocket
        - UdpSocket
        - UnixSocket

## Dispatchers

::: deimos.dispatcher
    options:
      show_root_heading: true
      show_root_full_path: false
      heading_level: 2
      show_source: false
      members:
        - ChannelFilter
        - CsvDispatcher
        - DataFrameDispatcher
        - DataFrameHandle
        - DecimationDispatcher
        - LatestValueDispatcher
        - LowPassDispatcher
        - TimescaleDbDispatcher
