# Spec Follow-on Backlog

Known caveats and deferred work identified during spec bootstrap (2026-04-13). Each entry corresponds to a `<!-- FOLLOW-ON: -->` comment in the live specs — the inline comments remain the authoritative footnote at the point of the requirement they qualify; this file aggregates them for planning.

When one of these ripens into active work, promote it to a proper `openspec/changes/<name>/` proposal (with `proposal.md`, `tasks.md`, and spec deltas) and strike it from this list.

---

## transport-abstraction

### T1. `ThreadChannelSocket::recv` surfaces truncation as `Err`
- **Source:** `openspec/specs/transport-abstraction/spec.md` — "Sockets expose a unified datagram-style I/O surface"
- **Today:** `ThreadChannelSocket::recv` silently truncates when payload > buf; UDP/Unix surface the OS's datagram-boundary error.
- **Direction:** Return `Err` on truncation to match `orchestrator.rs:recv` semantics. Align with "OS-surfaces-original-datagram-boundary" contract.
- **Refs:** `software/deimos/src/socket/thread_channel.rs:109-117`

### T2. Broadcast sentinel prefix
- **Source:** `openspec/specs/transport-abstraction/spec.md` — "Sockets expose a unified datagram-style I/O surface"
- **Today:** `ThreadChannelSocket::broadcast` sends with `PeripheralId::default()` as the prefix. HOOTL peripherals accept this; a stricter peripheral that validates the prefix could reject broadcasts.
- **Direction:** Introduce a broadcast sentinel prefix understood by all `Socket` impls.
- **Refs:** `software/deimos/src/socket/thread_channel.rs:123-126`

### T3. Unify `open()` idempotency across UDP/Unix
- **Source:** `openspec/specs/transport-abstraction/spec.md` — "lifecycle is open → close → (reopen)"
- **Today:** `UdpSocket::open` and `UnixSocket::open` differ on double-open behavior.
- **Direction:** Pick one (both idempotent, or both `Err` on double-open) and codify in the trait contract.

---

## data-dispatch

### D1. Synchronous flush-on-terminate for TSDB and Latest
- **Source:** `openspec/specs/data-dispatch/spec.md` — "Dispatchers flush before terminate returns"
- **Today:** CSV flushes synchronously on terminate. TimescaleDB and LatestValue only close their mpsc sender; their workers flush on drop (async w.r.t. terminate).
- **Direction:** Join on the worker thread during terminate so flush is complete before the call returns. Uniform "flush before terminate" guarantee across all dispatchers.

### D2. Per-dispatcher isolation (soft failure)
- **Source:** `openspec/specs/data-dispatch/spec.md` — "CSV provides a filesystem-backed data path independent of the network"
- **Today:** Any dispatcher error aborts the entire run. TSDB init failure panics via `.unwrap()`; `consume` errors terminate the session. `examples/hootl_csv_fallback.rs` reproduces both modes.
- **Direction:** Per-dispatcher isolation policy — a TSDB failure should not take down a parallel CSV recording. Known verification gap flagged in `CLAUDE.md` (task 2.2) and in `software/deimos/src/dispatcher/CLAUDE.md`.
- **Refs:** `software/deimos/src/controller/mod.rs:839`, `mod.rs:1543-1558`
- **Related:** C1 (shares the error-path refactor surface)

---

## controller-orchestration

### C1. Terminate on all error-exit paths
- **Source:** `openspec/specs/controller-orchestration/spec.md` — "Clean shutdown on every exit path"
- **Today:** Two error-exit paths in `Controller::run()` skip `self.terminate()`:
  1. Dispatcher error (`mod.rs ~1551-1558`): returns `Err` after `socket_orchestrator.close()`, no terminate.
  2. Calc eval error (`mod.rs ~1525-1527`): also skips terminate.
  Other error-exits (loss-of-contact, reconnect timeout, termination signal, socket-worker error) do call terminate.
- **Direction:** Route both paths through the same terminate-then-exit flow used by the others.
- **Related:** D2 (dispatcher error path overlaps)

---

## hootl-simulation

### H1. Cycle-level observation and injection hooks
- **Source:** `openspec/specs/hootl-simulation/spec.md` — "HOOTL enables full-lifecycle in-process tests"
- **Today:** `HootlRunHandle` exposes no cycle counter, per-cycle callback, or response-injection surface. `FUTURE` comment at `software/deimos/src/peripheral/hootl.rs:148` acknowledges this.
- **Direction:** Observation/injection surface so tests can assert on specific cycles or inject crafted responses.
- **Related:** H3 (shared API surface on the run handle)

### H2. Peripheral-delegated response bytes (value equivalence)
- **Source:** `openspec/specs/hootl-simulation/spec.md` — "HOOTL values are synthetic, not hardware-equivalent"
- **Today:** HOOTL runner writes synthetic placeholders (`counter + idx * 0.01`). `FUTURE` comment at `software/deimos/src/peripheral/hootl.rs:584`: "use peripheral object to write output".
- **Direction:** Delegate response-bytes construction to the inner peripheral so HOOTL is value-equivalent as well as framing-equivalent.

### H3. Cycle-count-based stop API
- **Source:** `openspec/specs/hootl-simulation/spec.md` — "HOOTL sessions are bounded by wall-clock time"
- **Today:** Only wall-clock deadline is supported.
- **Direction:** Add `max_cycles: Option<u64>` to `HootlConfig` for deterministic "run exactly N operating cycles" tests.
- **Related:** H1 (land together as the HOOTL test-ergonomics surface)

---

## realtime-calc

### R1. Per-eval wall-clock budget for calcs
- **Source:** `openspec/specs/realtime-calc/spec.md` — "Calcs evaluate synchronously once per cycle; MUST NOT block"
- **Today:** "No block" is convention and reviewer judgment only. A hanging calc stalls the entire control loop.
- **Direction:** Per-eval wall-clock budget that aborts a hanging calc and fails the cycle. Turns the convention into an enforced contract.

### R2. Post-init `update_input_map` returns `Err`
- **Source:** `openspec/specs/realtime-calc/spec.md` — "Inputs are routed by name; resolved once at init"
- **Today:** Post-init re-routing mutates the stored name, does NOT update the resolved tape index, and returns `Ok`. Silent misuse — the calc keeps reading from the originally resolved index.
- **Direction:** Return `Err` (or documented no-op with warning log) when called after `init`.

### R3. Calc init preconditions as `Result::Err` instead of panic
- **Source:** `openspec/specs/realtime-calc/spec.md` — "Stateful calcs reset between runs; init preconditions fail-fast"
- **Today:** `Butter2::init()` and `Pid::init()` use `assert!()` on `dt_ns > 0` — bad precondition panics the controller process.
- **Direction:** Convert to `Result::Err` to align with the Result-based error contract used elsewhere. Current panic behavior is intentional fail-fast until something else can be done with the error.

---

## Cross-cutting observations

- **D2 + C1** touch the same surface: dispatcher error handling in `Controller::run`. A single change could address both — add terminate to the dispatcher error path while reshaping that path to support per-dispatcher isolation.
- **H1 + H3** both extend the HOOTL run-handle API. Landing them together avoids a second API-surface churn.
- **R3** (panic → Result) is a broader idiom question — if adopted, sweep other `assert!` preconditions in calcs at the same time.
