<!-- REVIEW-COMPLETE (task 3.2): Four decisions reviewed against verification findings (tasks 1.1–2.6, 3.1).

  Decision 1 (eight capabilities): SOUND. All eight seams have substantial contracts. One open
  question — decimation/low-pass ownership between data-dispatch and signal-pipeline — was flagged
  in the design's own Open Questions and remains unresolved after verification; task 3.4 should
  address it before the specs go evergreen.

  Decision 2 (trait-contract scope): SOUND. Verification confirmed all five trait specs map
  correctly to real public traits. One nuance: sequence_machine/ is unusual in that it is more
  a state-machine framework than a calc node; its open question (split vs fold into realtime-calc)
  is unresolved. Not a Decision 2 defect, but worth resolving in task 3.4.

  Decision 3 (defer hardware-frontend / Python SDK): SOUND and still appropriate. Tasks 5.1–5.3
  track the follow-ups. No verification finding suggests deferral was wrong; the Python SDK
  has no observable behavior beyond the Rust contracts, and hardware-frontend would need its own
  extensive firmware/hardware specification pass.

  Decision 6 (memory capture as workflow): DECISION IS CORRECT; EXECUTION HAD GAPS. Task 3.1
  confirmed the directive was followed for five subsystems (root CLAUDE.md, dispatcher, controller,
  peripheral, signal-pipeline rule) but socket and calc subsystems each have non-obvious flagged
  findings (task 1.2 flagged 5 socket gaps; tasks 1.3/2.3 flagged calc error-convention issues)
  with no corresponding CLAUDE.md. See inline comment at Decision 6 below.

  Additional finding — data-dispatch "emergency fallback" scenario is an overstatement: task 2.2
  found the CSV-fallback guarantee is NOT satisfied by the current implementation (see inline
  REVIEW in data-dispatch spec). This is a spec-accuracy issue (Decision 5 territory), not a
  Decision 1–3/6 issue. Flagged here for visibility since it is the most significant correctness
  finding from phase 2 verification.
-->

## Context

Deimos is a real-time DAQ and control system with a Rust core (`software/deimos/`) and a Python binding (`deimos-daq` on PyPI). The core is organized around four trait seams — `peripheral/`, `socket/`, `calc/`, `dispatcher/` — orchestrated by `controller/`. Today, `openspec/` contains only `config.yaml`; there are no evergreen capability specs. This change seeds the first **eight**: the five trait/orchestrator seams plus three cross-cutting behaviors (`time-sync`, `signal-pipeline`, `hootl-simulation`).

Concrete code to reference (verified against the current tree):

- `software/deimos/src/peripheral/mod.rs` — Peripheral trait; concrete impls include `deimos_daq_rev{5,6,7}.rs`, `analog_i_rev_{2,3,4}.rs`, and `hootl.rs` (simulated peripheral).
- `software/deimos/src/socket/mod.rs` — Socket trait; impls: `udp.rs`, `unix.rs`, `thread_channel.rs`, plus `worker.rs` and `orchestrator.rs` for lifecycle.
- `software/deimos/src/calc/mod.rs` — Calc trait with node types (`affine`, `butter`, `pid`, `polynomial`, `rtd_pt100`, `tc_ktype`, `sequence_machine/`, …) composed into a sparse graph by `orchestrator.rs`.
- `software/deimos/src/dispatcher/mod.rs` — Dispatcher trait; impls: `csv.rs`, `tsdb.rs`, `latest.rs`, with filtering/decimation helpers (`channel_filter.rs`, `decimation.rs`, `low_pass.rs`).
- `software/deimos/src/controller/mod.rs` — state machine (`controller_state.rs`, `peripheral_state.rs`), blocking + `nonblocking.rs` drivers, `timing.rs` for loop pacing.

Additional code surfaces covered by this change:

- **Time synchronization** lives partially in `software/deimos/src/controller/timing.rs` (controller-side phase/period deltas sent in `emit_operating_roundtrip`) and in firmware (see `firmware/deimos_daq_rev7/` and siblings). The spec captures the *contract* — three composable levels yielding sub-µs total alignment — not the algorithm.
- **Signal pipeline** spans hardware (three analog stages, documented in `docs/guide.md`) and firmware (FIR fractional delay, Butterworth IIR, decimation; see `firmware/deimos_daq_rev7/`). The host-side `Butter2` calc (`software/deimos/src/calc/butter.rs`) is a user-facing echo of the firmware IIR stage. The spec is written at the contract level.
- **HOOTL simulation** is driven by `software/deimos/src/peripheral/hootl.rs` (`HootlPeripheral`, `HootlDriver`, `HootlRunHandle`, `HootlTransport`) plus the in-process `thread_channel.rs` socket implementation.

The guides under `docs/guide.md` and `docs/interpn-guide.md` provide narrative context (state machine stages, run/loop modes, "NEVER. LOSE. DATA." invariant, time sync levels).

## Goals / Non-Goals

**Goals:**
- Produce eight evergreen `spec.md` files that accurately describe the *current* behavioral contract of each seam — not aspirations.
- Each spec is self-contained: a new contributor can read one and understand what that seam guarantees to its callers.
- Specs cite concrete code paths under `software/deimos/src/` (and `firmware/` where the contract spans hardware) so reviewers can verify claims against the implementation.
- Persist non-obvious codebase-working knowledge uncovered during verification into project-shared memory (`CLAUDE.md`, `.claude/rules/`, nested `CLAUDE.md`), not just agent auto-memory. See the proposal's "Memory capture" section for the decision guide and categories.
- Keep the starter set small enough that maintainers can review it in one sitting (still achievable at eight, since three of the new specs are contract-level and short).

**Non-Goals:**
- No code changes. This is documentation-only.
- Not specifying hardware-frontend behavior (voltage, 4–20 mA, thermocouple, RTD signal conditioning). Frontend specs are a larger effort and deferred.
- Not specifying the Python SDK surface as a separate capability — treating the Python binding as a thin wrapper over the Rust contracts for this round; revisit in a follow-up if the binding grows its own observable behavior.
- Not prescribing new requirements or fixing existing bugs via specs. Evergreen specs describe what *is*, not what *should be next*.

## Decisions

**Decision 1: Seed eight capabilities — five trait/controller seams plus three cross-cutting behaviors (`time-sync`, `signal-pipeline`, `hootl-simulation`).**
- Alternative considered: one mega-spec "deimos-core". Rejected — too coarse to diff against in future proposals.
- Alternative considered: ten+ specs covering every submodule. Rejected — too thin; each would restate code without adding contract value.
- Alternative considered: five-only (trait seams), defer the three cross-cutting ones. Rejected on reconsideration — `time-sync`, `signal-pipeline`, and `hootl-simulation` each describe a load-bearing contract that reviewers will want to pin *now*: sub-µs sync is the headline marketing claim, the signal pipeline is where the most subtle correctness lives, and HOOTL is the thing tests depend on. Deferring them left too many dangling references from the other specs.

<!-- REVIEW (task 3.2): Decision 1 is sound. Verification (tasks 1.1–1.8, 2.1–2.6) confirmed all
     eight seams have real behavioral contracts worth pinning. One open question remains from the
     design itself: decimation.rs and low_pass.rs sit on the boundary between data-dispatch and
     signal-pipeline. The data-dispatch spec claims them as dispatcher-side transforms; signal-pipeline
     treats stages 1–6 as hardware/firmware only; the data-dispatch spec's Reference note for
     the composable-dispatcher requirement explicitly says dispatcher-side decimation "MAY be
     composed on top of stage-6 decimation but is not a substitute for it." The boundary is
     plausible but the duplicate naming (stage-6 decimation vs DecimationDispatcher) will confuse
     future proposal authors. Task 3.4 should resolve this before evergreen. Not a blocker on
     Decision 1 itself. -->

**Decision 2: Scope each spec to the public trait contract, not the concrete impls.**
- Rationale: evergreen specs should survive a new peripheral revision or a new transport. The trait is the stable surface; `deimos_daq_rev7.rs` is not. Concrete impls appear as *examples*, not requirements.
- Alternative considered: one spec per concrete impl (e.g., `deimos-daq-rev7`, `udp-socket`). Rejected — couples specs to hardware revisions.

<!-- REVIEW (task 3.2): Decision 2 is sound and held up well through verification. All five trait
     specs (peripheral, socket, calc, dispatcher, controller) map cleanly to real public traits.
     The only nuance: `signal-pipeline` and `time-sync` are not backed by a single trait — they
     are cross-cutting contracts composed across multiple modules. They fit Decision 2's intent
     (stable observable contract, not tied to any one impl) even though they don't have a
     single trait file to anchor them. One open question (unrelated to this decision) is whether
     `sequence_machine/` is stable enough as part of `realtime-calc` or needs its own spec;
     task 3.4 should resolve. -->

**Decision 3: Defer hardware-frontend specs and a separate Python SDK capability to follow-up proposals.**
- Rationale: hardware-frontend specs (voltage / 4–20 mA / thermocouple / RTD signal conditioning) each need their own requirement set and span hardware + firmware + host calcs; they are a larger effort best handled once the starter contracts have settled. The Python SDK is currently a faithful binding over the Rust contracts with no behavior of its own — adding a separate capability before it diverges would mostly restate the Rust specs.
- The three cross-cutting behaviors previously deferred (`time-sync`, `signal-pipeline`, `hootl-simulation`) are included in this change per Decision 1 above.

<!-- REVIEW (task 3.2): Decision 3 is still the right call. No finding from verification (tasks
     1.1–2.6) suggests hardware-frontend or Python SDK specs are needed now. The follow-up
     tracking (tasks 5.1–5.3 in tasks.md) covers all three deferred questions: hardware-frontend
     capability set, Python SDK, and whether firmware deserves its own parallel capability tree.
     The firmware question (task 5.3) gained new weight from verification: task 1.7 found that
     the signal-pipeline spec claims stages 4–6 are implemented in firmware, but the spec
     cannot be tested against firmware behavior via the Deimos host side alone — a future
     firmware capability tree (task 5.3) would allow those claims to be validated independently.
     No change to this decision needed; task 5.3 priority may be worth revisiting once the host
     specs are evergreen. -->

**Decision 4: Each seed spec uses requirement statements anchored to observable behavior, not implementation details.**
- Prefer: "The peripheral SHALL return a channel list with stable indices across a run." Avoid: "`Peripheral::channels()` returns a `Vec<Channel>`." The first survives a refactor; the second doesn't.
- OpenSpec delta proposals later will modify these statements — so they need to be statements, not code descriptions.

**Decision 5: Where the current code has a known constraint or wart, spec it honestly.**
- Example: the OVP rail ≤ 2.9 V limit is hardware, not software, so it doesn't appear here — but the "never lose data" / CSV-fallback guarantee *is* a software contract and will be spec'd verbatim.
- Rationale: an evergreen spec that overstates guarantees is worse than no spec. Future proposals can tighten the contract.

## Risks / Trade-offs

- **[Risk] Specs drift from code the moment they're written.** → Mitigation: each spec cites the file path(s) it describes so reviewers can diff against source; future proposals must update specs alongside code changes (enforced by the OpenSpec workflow).
- **[Risk] The seed set over-commits Deimos to contracts that are actually implementation accidents.** → Mitigation: Decision 5 — only spec behavior the maintainers intend to keep. Ambiguous behavior goes in Open Questions, not requirements.
- **[Risk] Five specs is still a lot for a first-ever evergreen review.** → Mitigation: each spec is short (one seam, a handful of SHALL statements) and independently reviewable. Maintainers can land them one at a time if needed, since no code depends on them.
- **[Trade-off] Deferring `time-sync` and `signal-pipeline` means the most scientifically interesting parts of Deimos remain unspec'd for now.** → Accepted: those are the parts where getting the contract wrong causes the most downstream pain, so they deserve their own focused proposal.

## Decision 6: Project-memory capture is part of the workflow, not a nice-to-have.

- Sub-agents executing verification tasks MUST persist non-obvious codebase-working findings to project-shared memory locations — `CLAUDE.md`, `.claude/rules/`, or nested subsystem `CLAUDE.md` — not only to their own auto-memory.
- Rationale: auto-memory is per-Claude and per-project-on-one-machine; team-shared memory survives across contributors, machines, and toolchains. The deimos repo currently has *no* `CLAUDE.md` / `.claude/rules/` — so bootstrapping that surface is as valuable as the specs themselves.
- The proposal's "Memory capture" section lists the decision guide (root vs rules vs nested) and the categories to watch for. Tasks.md operationalizes this: specific capture instructions appear as directives in each verification task.
- Alternative considered: leave memory capture to each agent's auto-memory. Rejected — it leaks the knowledge away from the repo; review-time findings evaporate once the review run ends.

<!-- REVIEW (task 3.2): Decision 6 is correct; execution had gaps (also flagged by task 3.1).

     What was captured: root CLAUDE.md, dispatcher/CLAUDE.md, controller/CLAUDE.md,
     peripheral/CLAUDE.md, .claude/rules/signal-pipeline.md — five files, all appropriate.

     Gaps remaining (both flagged in task 3.1 review of proposal.md):
     1. socket/ — task 1.2 flagged five behavioral asymmetries (UdpSocket open idempotency,
        UnixSocket open non-idempotency, SendFrom returning Err without closing the socket,
        cross-transport reconnect contract differences, and the "should be as accurate as
        possible" arrival-timestamp doc comment). None of these were persisted to a
        socket/CLAUDE.md or a .claude/rules/ file — they are still only inline REVIEW
        comments in the spec.
     2. calc/ — tasks 1.3 and 2.3 flagged error-convention issues (Butter2 uses f64::NAN
        sentinel returns rather than Result; state reset on restart is asserted in tests but
        not documented as a contract). No calc/CLAUDE.md was created.

     Recommendation: add software/deimos/src/socket/CLAUDE.md and
     software/deimos/src/calc/CLAUDE.md before marking this change evergreen (task 3.5 is
     the right home for that review). The decision itself is sound — the gaps are in
     execution, not in the directive. -->

## Risks / Trade-offs (additions for the three new specs)

- **[Risk] `signal-pipeline` spans hardware, firmware, and host; easy to over-specify.** → Mitigation: this spec captures the *observable contract* (stage ordering, deliberate phase-margin trade-off, decimation at the edge), not per-coefficient numerics. Firmware-coefficient correctness is out of scope for evergreen specs.
- **[Risk] `time-sync` claims "sub-µs without special hardware" — a headline claim that must not overstate.** → Mitigation: the spec says "converges to sub-µs between modules within a few seconds" matching the deck's language, and cites `controller/timing.rs` plus firmware state; we do not spec a worst-case bound.
- **[Risk] `hootl-simulation` may grow its own semantics that don't match real hardware.** → Mitigation: the spec pins HOOTL's *purpose* (full-lifecycle testing) and its *transport equivalence guarantee*, not its internal randomization or fixture shape.

## Open Questions

- `dispatcher/decimation.rs` and `low_pass.rs` blur into the signal pipeline. For now the `data-dispatch` spec treats them as dispatcher-side transforms and `signal-pipeline` treats them as the hardware/firmware chain only; revisit if reviewers find the split confusing.

<!-- PROPOSED RESOLUTION (task 3.4): Keep the current split and clarify the ownership boundary
     in both specs. The evidence from tasks 1.4 and 1.7 establishes that the two decimation
     surfaces serve distinct concerns:

     - **Stage-6 decimation** (firmware, `signal-pipeline` spec) is the *mandatory* anti-alias
       decimation: it happens before samples reach the host, controls the IIR cutoff ratio
       (configuring.rs lines 89–91), and operates at the ADC/firmware layer. Every channel
       passes through it unconditionally.
     - **`DecimationDispatcher` / `LowPassDispatcher`** (`data-dispatch` spec) are *optional*
       host-side adapters applied after rows have already been delivered to a dispatcher. They
       allow a specific dispatcher (e.g. TSDB) to receive a further-reduced rate without
       affecting other dispatchers or the pipeline itself.

     The `signal-pipeline` spec already acknowledges this: its Reference note for stage 6
     explicitly states "host-side `decimation.rs` … MAY be composed on top of stage-6
     decimation but is not a substitute for it." The boundary is load-bearing, not accidental.

     Recommended maintainer action: add a one-sentence "scope note" to the top of the
     `data-dispatch` composable-dispatcher requirement clarifying that `DecimationDispatcher`
     and `LowPassDispatcher` operate on already-decimated rows and cannot compensate for
     missing firmware-side anti-aliasing. No spec requirement text needs to change. -->

- Are `sequence_machine/` semantics part of `realtime-calc`, or do they warrant their own capability? Seeded inside `realtime-calc` for now; split later if it gets unwieldy.

<!-- PROPOSED RESOLUTION (task 3.4): Keep `SequenceMachine` folded inside `realtime-calc`.
     Evidence from task 1.3 and the codebase: `sequence_machine/` (mod.rs ~976 lines, plus
     lookup.rs, sequence.rs, transition.rs — ~1 500 lines total) is a large module, but it
     participates in the same Calc trait contract as every other calc type. The `realtime-calc`
     spec already names `SequenceMachine` in the built-in library requirement and calls out
     "sequence machine position" explicitly in the Calcs-reset-between-runs requirement. These
     references demonstrate that the existing spec language adequately covers the observable
     contract without needing a separate capability.

     The question is one of spec *weight*, not contract *scope*. Splitting would only be
     warranted if `SequenceMachine` imposed requirements that could not be expressed as
     corollaries of the `Calc` trait — e.g. if it had its own lifecycle (connect, configure,
     terminate) independent of the controller, or if it exposed a separate public interface.
     It does not: `SequenceMachine` implements `Calc`, uses `eval(tape)`, and resets on
     `terminate()` like every other node.

     Recommended maintainer action: no change needed. If `sequence_machine/` grows its own
     external-facing config API or multi-run persistence semantics in a future proposal, that
     proposal should revisit the split question at that time. -->

- Does firmware get its own capability tree later (paralleling `openspec/specs/` on the host side) — or should firmware contracts live inside the host-side spec for each behavior that crosses the boundary (current choice)?

<!-- PROPOSED RESOLUTION (task 3.4): Do not create a firmware capability tree in this change.
     Defer to task 5.3 with a concrete recommendation: a firmware-parallel tree is worth doing,
     but only after the host specs are evergreen.

     Evidence from task 1.7: the `signal-pipeline` spec claims stages 4–6 are implemented in
     firmware and cites `firmware/deimos_daq_rev7/src/board/subsystems/sampling.rs`; the
     verification confirmed those claims are accurate for rev7. However, the spec cannot be
     machine-tested against firmware behavior via the Deimos host side alone — firmware stages
     are observable only through their output at the USB boundary, not via host-side Rust code.
     This is the core tension: host-side specs that cross the firmware boundary have claims that
     are load-bearing but not automatically verifiable.

     A firmware capability tree (one `firmware/openspec/specs/` directory, mirroring the host
     layout) would let firmware proposals formally express the contract each firmware revision
     exposes and let verification tasks target firmware code directly rather than cross-
     referencing it from a host-side spec. The `firmware/` directory already has three parallel
     revision directories (deimos_daq_rev5, deimos_daq_rev6, deimos_daq_rev7) plus two
     analog_i revisions — enough surface area to justify the scaffolding.

     Recommended maintainer action: accept the current choice (firmware contracts inside host-
     side specs) for this round. In a follow-up proposal (task 5.3), create
     `firmware/openspec/` with at least a `signal-pipeline` firmware spec that validates the
     stage 4–6 claims from this change. The decision can be revisited as part of that proposal
     using the concrete gap this task identified. -->

