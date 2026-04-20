# Socket subsystem — non-obvious patterns

## open() idempotency differs by transport

- `UdpSocket::open()` (`udp.rs:184-209`) silently closes and reopens if already open
  (`if self.socket.is_some() { self.close(); }`). Calling open() twice is safe.
- `UnixSocket::open()` (`unix.rs:107-129`) returns `Err("Controller unix socket already open")`
  if called when already open. The caller must call `close()` first.
- Consequence: code that calls `open()` without a preceding `close()` behaves differently on UDP
  vs Unix. The reconnect scenario (controller calls `close()` then `open()`) works correctly on
  both; the divergence only matters for direct double-open callers.

## broadcast() error semantics differ by transport

- `UnixSocket::broadcast()` (`unix.rs:222-261`) returns `Ok(())` when the peripheral socket
  directory does not exist or is empty, sending zero packets.
- `UdpSocket::broadcast()` returns `Err(...)` when it fails to send on any target.
- Treating a successful broadcast as delivery confirmation is not safe across transports.
  Unix silent-success is intentional for the phase where peripherals may not yet have registered.

## ThreadChannelSocket::recv() silently truncates on small buffer

- `thread_channel.rs:recv()` (lines 109-117) copies `payload.len().min(buf.len())` bytes and
  returns that truncated size. UDP and Unix datagram sockets discard overflow but surface the
  original boundary; `ThreadChannelSocket` makes the truncation invisible.
- By contrast, `orchestrator.rs:recv()` returns `Err` when `payload.len() > buf.len()`.
- For in-process (HOOTL) use, ensure the controller allocates a buffer at least as large as
  the largest expected packet. No runtime error will indicate a mismatch.

## ThreadChannelSocket::broadcast() prepends default PeripheralId

- `thread_channel.rs:123-126`: broadcast is implemented as
  `self.send(PeripheralId::default(), msg)`, prepending a zeroed PeripheralId.
- HOOTL peripherals currently accept this (they strip the prefix on recv). Stricter peripheral
  implementations that validate the PeripheralId before processing may reject or misroute it.

## arrival-timestamp accuracy is best-effort on in-process transports

- The spec requires `recv` to return an `Instant` "sufficiently accurate" for the time-sync
  subsystem. For UDP and Unix, the timestamp is taken at kernel recv boundary.
- For `ThreadChannelSocket`, the timestamp is `Instant::now()` sampled in the receive method
  body (`thread_channel.rs`), after channel receive. In-process scheduling jitter means
  HOOTL timestamps are not as accurate as real-hardware timestamps.
- The `time-sync` spec's TOTAL sub-µs claim is explicitly hardware-only; HOOTL cannot
  meaningfully exercise it. See `<!-- REVIEW: -->` in `specs/time-sync/spec.md`.
