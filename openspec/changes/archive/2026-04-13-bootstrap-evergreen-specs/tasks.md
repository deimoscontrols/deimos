> **Memory-capture directive (applies to every task below):** whenever a sub-agent performs a task in this change and discovers non-obvious knowledge about how to work with the Deimos codebase — build commands, repo-walking shortcuts, recurring patterns, gotchas, naming drift, error-convention mismatches, run/test harnesses — it MUST persist that finding to project-shared memory, not only its own auto-memory. Pick the right home using the decision guide from the proposal:
>
> - **Root `CLAUDE.md`** → build/test commands, top-level "read me first" pointers, hard team rules.
> - **`.claude/rules/<topic>.md` (no `paths:`)** → cross-cutting conventions that always apply (e.g., typetag-serde patterns, fail-fast conventions).
> - **`.claude/rules/<topic>.md` (with `paths:`)** → conventions scoped by glob (e.g., `paths: ["software/deimos/src/calc/**"]`).
> - **Nested `CLAUDE.md`** in the subsystem directory → subsystem-specific guidance (e.g., `software/deimos/src/dispatcher/CLAUDE.md` documenting the async-drop `terminate` pattern).
>
> Target ≤200 lines/file; one fact per bullet; cite a file path or file:line where the fact lives. Do NOT duplicate. When no repo-level memory exists yet, be the first to create it.

## 1. Verify seed specs against current code

- [x] 1.1 Walk `software/deimos/src/peripheral/mod.rs` end-to-end against `specs/peripheral-lifecycle/spec.md`; flag any SHALL statement the code doesn't actually honor and open an inline comment for maintainer review.
- [x] 1.2 Walk `software/deimos/src/socket/mod.rs` (plus `udp.rs`, `unix.rs`, `thread_channel.rs`, `orchestrator.rs`, `worker.rs`) against `specs/transport-abstraction/spec.md`; flag gaps.
- [x] 1.3 Walk `software/deimos/src/calc/mod.rs` and `orchestrator.rs` against `specs/realtime-calc/spec.md`; confirm the built-in calc list in the spec matches the `pub use` list in `mod.rs`.
- [x] 1.4 Walk `software/deimos/src/dispatcher/mod.rs` (plus `csv.rs`, `tsdb.rs`, `df.rs`, `latest.rs`, `channel_filter.rs`, `decimation.rs`, `low_pass.rs`) against `specs/data-dispatch/spec.md`.
- [x] 1.5 Walk `software/deimos/src/controller/mod.rs`, `controller_state.rs`, `peripheral_state.rs`, `timing.rs`, `nonblocking.rs` against `specs/controller-orchestration/spec.md`.
- [x] 1.6 Walk `software/deimos/src/controller/timing.rs`, `software/deimos/src/peripheral/mod.rs` (operating-roundtrip packet emission), and `software/deimos_shared/src/states/` against `specs/time-sync/spec.md`; cross-check LOCAL (period/phase deltas), GLOBAL (monotonic vs wall-clock), TOTAL (sub-µs without special hardware), and the arrival-timestamp dependency on the `transport-abstraction` contract.
- [x] 1.7 Walk `software/deimos/src/calc/butter.rs` and firmware pointers under `firmware/deimos_daq_rev7/` against `specs/signal-pipeline/spec.md`; verify the six-stage ordering claim, the INAMP-then-filter placement, the FIR default-order claim, and the "no run-start step" steady-state-init requirement. Flag if the firmware tree does not actually implement stages 4–6 where the spec says it does.
- [x] 1.8 Walk `software/deimos/src/peripheral/hootl.rs` and `software/deimos/src/socket/thread_channel.rs` against `specs/hootl-simulation/spec.md`; verify HOOTL is built unconditionally (no Cargo feature), confirm `HootlPeripheral`/`HootlDriver`/`HootlRunHandle`/`HootlTransport` exist with the roles the spec claims, and verify the byte-level-compat requirement against one concrete simulated model.

## 2. Verify specs against runtime behavior

- [x] 2.1 Run an end-to-end HOOTL session and confirm lifecycle stages `Connecting → Binding → Configuring → OperatingRoundtrip` are observed in order (covers `peripheral-lifecycle`, `controller-orchestration`, `hootl-simulation`). If this is the first time anyone runs HOOTL from scratch in this repo, capture the exact invocation + env vars in root `CLAUDE.md` so the next agent doesn't re-derive it.
- [x] 2.2 Run a HOOTL session with both `CsvDispatcher` and `TimescaleDbDispatcher`, deliberately break the DB connection mid-run, and confirm CSV continues recording every row (covers `data-dispatch` "emergency fallback" scenario).
- [x] 2.3 Run a session containing `Butter2`, verify its state resets across a restart (covers `realtime-calc` "reset between runs" scenario).
- [x] 2.4 Run two controllers on one transport with disjoint peripheral sets and confirm each ignores the other's peripherals (covers `controller-orchestration` subset-operation scenario).
- [x] 2.5 Instrument a HOOTL session to extract per-peripheral phase alignment over time; confirm convergence to sub-µs within a few seconds (covers `time-sync` TOTAL requirement, as much as HOOTL can exercise it). If HOOTL cannot meaningfully exercise the claim without real hardware, record that honestly as an inline `<!-- REVIEW: -->` in `specs/time-sync/spec.md`.
- [x] 2.6 Run a HOOTL `DeimosDaqRev7` session and capture one operating-cycle packet bytes; diff against the bytes the real `DeimosDaqRev7` impl would emit for the same inputs (covers `hootl-simulation` byte-level-compat requirement).

## 3. Maintainer review

- [x] 3.1 Review `proposal.md` for accuracy of motivation, scope, and the memory-capture directive.
- [x] 3.2 Review `design.md` decisions — especially Decision 1 (eight-capability scope), Decision 2 (trait-contract scope), Decision 3 (deferring hardware-frontend / Python SDK), and Decision 6 (memory capture as workflow).
- [x] 3.3 Review each of the eight `specs/*/spec.md` files; approve or request changes per-spec so they can land independently if needed.
- [x] 3.4 Resolve the remaining Open Questions in `design.md` (decimation/low-pass ownership between `data-dispatch` and `signal-pipeline`, sequence-machine split, firmware capability tree).
- [x] 3.5 Review any new `CLAUDE.md` / `.claude/rules/` / nested `CLAUDE.md` files introduced by verification tasks; confirm they are scoped correctly and not duplicating guidance.

## 4. Land and archive

- [x] 4.1 Run `pnpm openspec validate bootstrap-evergreen-specs --strict` and resolve any structural errors.
- [x] 4.2 Run `pnpm openspec archive bootstrap-evergreen-specs` to promote the eight specs into `openspec/specs/`. Cleared after the spec-text rewrites resolved the four `request-changes` verdicts (commit `07358bc`); maintainer ran apply+archive together.
- [x] 4.3 Confirm `openspec/specs/{peripheral-lifecycle,transport-abstraction,realtime-calc,data-dispatch,controller-orchestration,time-sync,signal-pipeline,hootl-simulation}/spec.md` all exist and are non-empty after apply.
- [x] 4.4 Archive the change per the OpenSpec workflow (`openspec archive` combines apply + archive).

## 5. Follow-on proposals (tracking only)

- [ ] 5.1 Open a proposal seed for a **hardware-frontend** capability set (voltage, 4–20 mA, thermocouple K-type, RTD/PT100 signal-conditioning contracts). **TRACKING — future change.**
- [ ] 5.2 Decide whether the **Python SDK** needs its own capability spec, or stays covered by the Rust contracts (resolves a deferred question from this change — revisit once the binding has observable behavior of its own). **TRACKING — future change.**
- [ ] 5.3 Decide whether **firmware** deserves its own capability tree (paralleling `openspec/specs/` on the host side) rather than being folded into host-side specs that cross the boundary. **TRACKING — future change.** Task 3.4 proposed resolution: defer with positive recommendation once a second firmware target lands.
