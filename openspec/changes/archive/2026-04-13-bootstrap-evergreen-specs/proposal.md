<!-- REVIEW-COMPLETE (task 3.5): memory files scoped correctly, no duplication found, all files under 200 lines. socket/ and calc/ gaps from tasks 1.2/1.3/2.3 resolved: created software/deimos/src/socket/CLAUDE.md (5 behavioral asymmetries) and software/deimos/src/calc/CLAUDE.md (assert-vs-Result, NaN sentinel, post-init guard, sync eval). Existing files (root CLAUDE.md, dispatcher/CLAUDE.md, controller/CLAUDE.md, peripheral/CLAUDE.md, .claude/rules/signal-pipeline.md) are correctly scoped with no cross-file duplication. -->

<!-- REVIEW (task 3.1): Two issues flagged; motivation and memory-capture structure are sound.

  1. SCOPE — "No code changes" (What Changes bullet 4, and Impact section) is not quite accurate:
     tasks 2.1–2.6 introduced Rust example files (hootl_lifecycle.rs, hootl_csv_fallback.rs,
     hootl_butter_reset.rs, hootl_two_controllers.rs, etc.) to exercise runtime scenarios. These
     are test/verification harnesses, not production code, but they are code changes. Suggest
     narrowing the claim to "No production code changes" or noting that verification tasks may add
     example harnesses.

  2. MEMORY-CAPTURE DIRECTIVE — The directive is detailed and actionable, and was largely followed
     (five new memory files: root CLAUDE.md, dispatcher/CLAUDE.md, controller/CLAUDE.md,
     peripheral/CLAUDE.md, .claude/rules/signal-pipeline.md). Two gaps remain: the socket
     subsystem (5 gaps flagged in task 1.2, all left only as inline REVIEW comments in the spec)
     and the calc subsystem (error-convention issues from tasks 1.3 and 2.3) each have non-obvious
     findings but no corresponding subsystem CLAUDE.md. The directive names these subsystems as
     prime candidates (categories 2, 3, 6). No change needed to the directive text itself — the
     gaps are in execution, not specification. Maintainer should decide whether to add
     socket/CLAUDE.md and calc/CLAUDE.md before marking this change evergreen.
-->

## Why

Deimos has a working Rust core, Python bindings, and a published hardware product, but no evergreen specs documenting the behavioral contracts between its major components. Without a spec baseline, every future change has to re-derive what the system is *supposed* to do from code, which makes review slower and regressions easier. Seeding a starter set of evergreen capability specs now — before the next wave of features lands — gives reviewers, contributors, and future proposals a shared contract to cite and diff against.

## What Changes

- Add an initial set of evergreen capability specs under `openspec/specs/` covering the major seams of the current Rust core (`software/deimos/src/{peripheral,socket,calc,dispatcher,controller}`), plus three cross-cutting behaviors (time synchronization, the signal-processing pipeline, and HOOTL simulation) that are load-bearing enough to be worth pinning in the starter set.
- Each seed spec captures **current** behavior only — what the code actually does today — not aspirational behavior. Future proposals will delta these.
- Wire **codebase-knowledge capture** into the verification workflow: sub-agents performing code walks MUST persist any non-obvious findings about how to work with the repo into project-shared memory (`CLAUDE.md`, `.claude/rules/`, or nested subsystem `CLAUDE.md`) — not just their own auto-memory. See "Memory capture" below.
- No code changes. This is a documentation/contract bootstrap.

## Capabilities

### New Capabilities

- `peripheral-lifecycle`: Representation of a DAQ node — its channels, configuration, and the contract it presents to the controller.
- `transport-abstraction`: Socket trait contract — how bytes move between host and peripheral (UDP, TCP, UART, Unix, in-process), including timeout and reconnect semantics.
- `realtime-calc`: Calc trait contract — sparse expression graphs evaluated in lock-step with sampling, producing control outputs each tick.
- `data-dispatch`: Dispatcher trait contract — durable fanout to storage sinks, with the "never lose data" invariant and CSV fallback guarantee.
- `controller-orchestration`: Controller state machine — `Connecting → Binding → Configuring → OperatingRoundtrip` with self-healing on timeout, plus the {Performant, Efficient} × {Blocking, Non-blocking} run-mode matrix.
- `time-sync`: Three-level clock discipline (LOCAL per-peripheral closed-loop phase alignment, GLOBAL monotonic-clock + NTP, TOTAL composition) that delivers sub-µs alignment without specialized hardware.
- `signal-pipeline`: Six-stage signal chain (RF filter → active low-pass → ADC input filter → FIR fractional delay → Butterworth IIR → decimation), including the phase-margin trade-off and deliberate aliasing tolerance.
- `hootl-simulation`: Hardware-out-of-the-loop peripheral + transport semantics (`HootlPeripheral`, `HootlDriver`, `HootlTransport`) enabling full-lifecycle testing without physical hardware.

### Modified Capabilities

<!-- none: there are no existing specs yet -->

## Memory capture (what sub-agents should look out for)

When a sub-agent walks the Deimos code to verify a spec — or encounters friction, surprise, or a useful pattern while executing any task in this change — the agent MUST persist that finding to one of the following locations:

- **Root `CLAUDE.md`** — core "read me first" knowledge: build commands (`cargo build`, `cargo test`, `maturin develop`), how to run a HOOTL session, top-level architecture pointer (one-line: "four traits + controller, see …"), hard rules the whole team follows.
- **`.claude/rules/<topic>.md` without `paths:`** — cross-cutting conventions that always apply but deserve their own file (e.g., `typetag-serde-patterns.md`, `fail-fast-vs-result-err.md`).
- **`.claude/rules/<topic>.md` with `paths:`** — conventions scoped by file pattern (e.g., `paths: ["software/deimos/src/calc/**"]` for calc-impl conventions).
- **Nested `CLAUDE.md` in a subsystem directory** — subsystem-specific guidance (e.g., `software/deimos/src/dispatcher/CLAUDE.md` documenting the async-drop `terminate` pattern; `software/deimos/src/controller/CLAUDE.md` documenting the error-path invariants).

Specific categories to watch for during this change:

1. **Canonical trait vs concrete impl** — where the object-safe trait lives vs where each impl lives, and the `typetag::serde(tag = "type")` + `Box<dyn Trait>` + JSON-round-trip `Clone` pattern used throughout.
2. **Error conventions** — where the code uses `Result<_, String>` vs `assert!` / `unwrap()` / `panic!` as a fail-fast contract (flagged during task 1.3 for calcs and 1.5 for controller).
3. **Async-drop patterns** — dispatchers `terminate` by dropping worker handles, not by joining (flagged during task 1.4). Any similar "returns `Ok(())` while shutdown finishes in the background" pattern deserves a rule.
4. **Run commands / harnesses** — how to actually run a HOOTL session, where examples live, what env vars (`RUST_LOG`, etc.) turn on useful logs. If not documented anywhere, document it in root `CLAUDE.md` the first time a task needs it.
5. **Naming drift** — where code names and spec names diverge (e.g., `Disconnected` in code ↔ `Connecting` in spec, flagged during task 1.5). Decide per finding whether the fix goes in the spec, in a nested `CLAUDE.md`, or both.
6. **Gotchas per transport / per peripheral revision** — any behavioral asymmetry worth a line in a nested `CLAUDE.md` (e.g., `UdpSocket::open` is idempotent; `UnixSocket::open` is not — flagged during task 1.2).

**Format:** target under 200 lines per file; one fact per bullet; cite a file path or file:line where the fact lives. Do NOT duplicate the same guidance across mechanisms.

**When not to capture:** anything a reader can derive in seconds by reading the file the spec already cites. Memory is for non-obvious knowledge, not a restatement of headers.

## Impact

- **New directory:** `openspec/specs/` populated with **eight** capability directories, each containing a `spec.md`.
- **New project memory surface:** the first-ever `CLAUDE.md`, `.claude/rules/`, or nested subsystem `CLAUDE.md` files may be introduced as byproducts of verification tasks. The deimos repo currently has only `.claude/settings.json` and `.claude/agent-memory-local`, so any new memory file is additive.
- **No code changes.** Specs are derived from the current `software/deimos/src/` tree and the guides under `docs/`.
- **Out of scope for this change (candidates for follow-ups):** hardware-frontend specs (voltage, 4–20 mA, thermocouple, RTD) and the Python SDK surface. These are deliberately deferred so the starter set stays reviewable in one pass.
- **Reviewers:** Deimos maintainers need to confirm each seed spec matches observed behavior before it becomes evergreen.
