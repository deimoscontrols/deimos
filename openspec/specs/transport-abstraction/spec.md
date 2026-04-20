# transport-abstraction Specification

## Purpose

Defines the socket trait: packetized send/receive (UDP, Unix datagram, in-process thread-channel), unicast plus broadcast plus late address binding, per-packet arrival timestamps for time-sync, lifecycle management with reconnect, and orchestration across multiple sockets. Documents the intentional per-transport asymmetries (thread-channel truncation, broadcast-success divergence, open idempotency divergence) as behavioral contracts, not bugs.

## Requirements
### Requirement: Sockets provide packetized message passing

A socket implementation SHALL move discrete byte packets between the controller and peripherals. For the UDP and Unix-datagram transports, each send or receive corresponds to exactly one logical packet — OS datagram boundaries are preserved. For the in-process thread-channel transport used by HOOTL, each send enqueues one message; receive copies up to the caller's buffer length and silently discards any remainder if the buffer is smaller than the payload. Callers using the thread-channel transport MUST size receive buffers to the maximum expected payload, since under-sized buffers are truncated without error.

The thread-channel transport's broadcast uses a default peripheral-id prefix rather than a true broadcast sentinel. HOOTL peripherals accept this today; peripherals that validate the prefix would reject it.

Reference: `trait Socket` in `software/deimos/src/socket/mod.rs`; impls: `udp.rs`, `unix.rs`, `thread_channel.rs:109-117`.

<!-- FOLLOW-ON: `ThreadChannelSocket::recv` could return `Err` (matching `orchestrator.rs:recv` which returns Err when payload.len() > buf.len()) instead of silently truncating. That change proposal would align thread_channel with UDP/Unix "OS-surfaces-original-datagram-boundary" semantics. -->

<!-- FOLLOW-ON: `ThreadChannelSocket::broadcast` (thread_channel.rs:123-126) sends with `PeripheralId::default()` as the prefix. HOOTL peripherals accept this today, but a stricter peripheral that validates the prefix could reject broadcasts. A future change proposal could introduce a broadcast sentinel prefix understood by all `Socket` impls. -->

#### Scenario: UDP send delivers one whole packet, thread-channel receive may truncate

- **WHEN** the controller sends a message over the UDP transport sized to fit within the MTU
- **THEN** the peer MUST receive exactly one packet containing that message — not a concatenation, not a split
- **AND** the same message sent over the thread-channel transport to a receiver with an undersized buffer MUST be silently truncated to the buffer length, without an error being surfaced

### Requirement: Sockets support unicast, broadcast, and late address binding

A socket MUST provide unicast to a known peripheral id, broadcast to all reachable peripherals (used during Disconnected/Binding lifecycle phases), and a late-binding operation that associates a peripheral id with a transport-specific address token first observed on a received packet. Broadcast success semantics are transport-specific: the UDP transport returns an error when it fails to send on any target, while the Unix-datagram transport returns success when the peripheral socket directory does not exist or is empty (sending zero packets). Callers MUST NOT treat a successful broadcast return as confirmation of delivery.

Reference: `Socket::send`, `Socket::broadcast`, `Socket::update_map` in `software/deimos/src/socket/mod.rs`; `UdpSocket::broadcast` in `udp.rs`; `UnixSocket::broadcast` in `unix.rs:222-261`.

#### Scenario: Controller discovers a peripheral it has never talked to

- **WHEN** the controller broadcasts a connect packet and a previously unknown peripheral replies
- **THEN** the socket SHALL surface the reply's transport address as an opaque token, and the controller MUST bind that token to the peripheral's id before any subsequent unicast send to that peripheral

### Requirement: Received packets carry arrival timestamps

Every receive result SHALL include an arrival timestamp accurate enough to drive the time-synchronization subsystem. Packet metadata MUST also report the payload size in bytes and an opaque address token identifying the sender's transport address.

Reference: `SocketPacketMeta { pid, token, time, size }` in `software/deimos/src/socket/mod.rs`.

#### Scenario: Closed-loop timing adjusts to packet arrival

- **WHEN** a peripheral's operating-roundtrip reply arrives
- **THEN** the receive path MUST return the arrival timestamp with sufficient accuracy for the controller's local time-sync loop to phase-align the peripheral's sample clock

### Requirement: Sockets manage their own lifecycle

Every socket MUST expose open, close, and an is-open query. Open SHALL perform any required one-time setup; close SHALL clear state and release locks. The controller state machine SHALL reconnect on timeout by calling close followed by open, without requiring the caller to construct a new socket.

Open-while-already-open idempotency is transport-specific: the UDP transport internally closes and reopens (idempotent against double-open), while the Unix-datagram transport returns an error. The controller's reconnect path always pairs close with open, so both transports work in that flow; callers that invoke open directly without a preceding close MUST account for the divergence.

Reference: `Socket::is_open`, `Socket::open`, `Socket::close` in `software/deimos/src/socket/mod.rs`; `UdpSocket::open` in `udp.rs:184-209`; `UnixSocket::open` in `unix.rs:107-129`.

#### Scenario: Controller recovers from a transport drop

- **WHEN** the controller detects a timeout on a socket mid-run
- **THEN** it SHALL close and reopen the existing socket and return to the Disconnected lifecycle state, reusing the socket instance

<!-- FOLLOW-ON: A future change proposal could align `UdpSocket::open` and `UnixSocket::open` idempotency semantics (either both idempotent or both returning `Err` on double-open), or codify the divergence in the trait contract. Not a gate on this bootstrap. -->

### Requirement: Sockets are composable via orchestration and workers

Deimos SHALL provide an orchestration layer so a single controller can drive multiple sockets concurrently. The orchestrator exposes the same send / broadcast / bind / receive contract as a single socket and fans operations out to, and receives back from, the underlying sockets. In Efficient operating mode, orchestration is backed by per-socket worker threads communicating over an internal command/event protocol; in Performant mode the orchestrator drives sockets directly. The worker protocol is an internal detail, not part of the caller-facing contract.

Reference: `software/deimos/src/socket/orchestrator.rs`, `worker.rs`; exports `SocketOrchestrator`, `SocketWorker`, `SocketWorkerCommand`, `SocketWorkerEvent`, `SocketWorkerHandle`.

#### Scenario: One controller, two transports

- **WHEN** a controller is configured with a UDP socket for a remote DAQ and a thread-channel socket for a HOOTL peripheral
- **THEN** the orchestrator SHALL drive both concurrently and deliver received packets from each without requiring the caller to poll them individually

