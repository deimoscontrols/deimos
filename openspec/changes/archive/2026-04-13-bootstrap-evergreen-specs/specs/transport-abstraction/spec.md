<!-- REVIEW-COMPLETE (task 3.3): approved — spec text now describes actual per-transport behavior (thread_channel truncation, broadcast prefix, Unix/UDP broadcast and open asymmetries, method-based orchestrator surface). Candidate alignment changes are tracked as follow-on proposals, not gates on this bootstrap. -->

## ADDED Requirements

### Requirement: Sockets provide packetized message passing

A `Socket` implementation SHALL move discrete byte packets between the controller and peripherals. For the `UdpSocket` and `UnixSocket` transports, each `send` or `recv` corresponds to exactly one logical packet (OS datagram boundaries are preserved). For the in-process `ThreadChannelSocket`, each `send` enqueues one crossbeam-channel message; `recv` copies up to `buf.len()` bytes of that message and silently discards any remainder if `buf` is smaller than the payload.

Reference: `trait Socket` in `software/deimos/src/socket/mod.rs`; impls: `udp.rs`, `unix.rs`, `thread_channel.rs:109-117`.

#### Scenario: UDP transport delivers a whole packet

- **WHEN** the controller calls `send(peripheral_id, msg)` over a UDP socket
- **THEN** the peer SHALL receive exactly one packet containing `msg` — not a concatenation, not a split

#### Scenario: Thread-channel transport for in-process testing

- **WHEN** a HOOTL peripheral is paired with the controller via `thread_channel.rs` and the caller provides a `buf` at least as large as the largest message it expects
- **THEN** `send`/`recv` exercise the same lifecycle code paths as UDP; callers MUST size `buf` to the maximum expected payload, since under-sized buffers are truncated silently rather than surfaced as an error

<!-- FOLLOW-ON: `ThreadChannelSocket::recv` could return `Err` (matching `orchestrator.rs:recv` which returns Err when payload.len() > buf.len()) instead of silently truncating. That change proposal would align thread_channel with UDP/Unix "OS-surfaces-original-datagram-boundary" semantics. -->

<!-- FOLLOW-ON: `ThreadChannelSocket::broadcast` (thread_channel.rs:123-126) sends with `PeripheralId::default()` as the prefix. HOOTL peripherals accept this today, but a stricter peripheral that validates the prefix could reject broadcasts. A future change proposal could introduce a broadcast sentinel prefix understood by all `Socket` impls. -->

### Requirement: Sockets support unicast, broadcast, and late address binding

A `Socket` MUST expose `send(PeripheralId, &[u8])` for unicast to a known peripheral, `broadcast(&[u8])` to reach reachable peripherals (used during `Disconnected`/`Binding`), and `update_map(PeripheralId, SocketAddrToken)` to bind a peripheral id to a transport-specific address token observed via `recv`. Broadcast success semantics are transport-specific: `UdpSocket::broadcast` returns `Err` when it fails to send on any target, while `UnixSocket::broadcast` returns `Ok(())` when the peripheral socket directory does not exist or is empty (sending zero packets). Callers MUST NOT treat a successful `broadcast` return as confirmation of delivery.

Reference: `Socket::send`, `Socket::broadcast`, `Socket::update_map` in `software/deimos/src/socket/mod.rs`; `UdpSocket::broadcast` in `udp.rs`; `UnixSocket::broadcast` in `unix.rs:222-261`.

#### Scenario: Controller discovers a peripheral it has never talked to

- **WHEN** the controller broadcasts a connect packet and a previously unknown peripheral replies
- **THEN** the socket SHALL surface the reply's address via a `SocketAddrToken`, and the controller MUST call `update_map` to associate that token with the peripheral's id before subsequent unicast sends

### Requirement: Received packets carry arrival timestamps

Every `recv` result SHALL include the arrival `Instant`, sufficiently accurate to be used by the time-synchronization subsystem. Packet metadata MUST also report the size in bytes and an opaque `SocketAddrToken` identifying the sender's transport address.

Reference: `SocketPacketMeta { pid, token, time, size }` in `software/deimos/src/socket/mod.rs`.

#### Scenario: Closed-loop timing adjusts to packet arrival

- **WHEN** a peripheral's operating-roundtrip reply arrives
- **THEN** `recv` MUST return the `Instant` at which the packet was received, accurate enough that the controller's local time-sync loop can phase-align the peripheral's sample clock

### Requirement: Sockets manage their own lifecycle

Every `Socket` MUST expose `is_open`, `open(ctx)`, and `close`. `open` SHALL perform any required one-time setup; `close` SHALL clear state and release locks. The controller state machine SHALL use these to implement reconnect on timeout by calling `close` followed by `open`. `open` idempotency is transport-specific: `UdpSocket::open` internally closes and reopens if already open (idempotent against double-open), while `UnixSocket::open` returns `Err("Controller unix socket already open")` when already open. The controller's reconnect path always pairs `close` with `open`, so both transports work in the reconnect flow; callers that invoke `open` directly without a preceding `close` MUST account for the divergence.

Reference: `Socket::is_open`, `Socket::open`, `Socket::close` in `software/deimos/src/socket/mod.rs`; `UdpSocket::open` in `udp.rs:184-209`; `UnixSocket::open` in `unix.rs:107-129`.

#### Scenario: Controller recovers from a transport drop

- **WHEN** the controller detects a timeout on a socket mid-run
- **THEN** it SHALL call `close` followed by `open` and return to `Disconnected`, without requiring the caller to construct a new `Socket`

<!-- FOLLOW-ON: A future change proposal could align `UdpSocket::open` and `UnixSocket::open` idempotency semantics (either both idempotent or both returning `Err` on double-open), or codify the divergence in the trait contract. Not a gate on this bootstrap. -->

### Requirement: Sockets are composable via orchestration and workers

Deimos SHALL provide a `SocketOrchestrator` layer so a single controller can drive multiple sockets concurrently. `SocketOrchestrator` exposes a method-based public API — `send`, `broadcast`, `update_map`, `recv` — that fans out to the underlying `Socket` impls and fans in `recv` results from all sockets. In Efficient mode, orchestration is backed by per-socket `SocketWorker` threads that communicate via `SocketWorkerCommand` / `SocketWorkerEvent` internally; in Performant mode the orchestrator drives sockets directly. The command/event enum is an internal detail, not part of the caller-facing contract.

Reference: `software/deimos/src/socket/orchestrator.rs`, `worker.rs`; exports `SocketOrchestrator`, `SocketWorker`, `SocketWorkerCommand`, `SocketWorkerEvent`, `SocketWorkerHandle`.

#### Scenario: One controller, two transports

- **WHEN** a controller is configured with a UDP socket for a remote DAQ and a thread-channel socket for a HOOTL peripheral
- **THEN** the orchestrator SHALL drive both concurrently and deliver `SocketRecvMeta` from each without requiring the caller to poll them individually
