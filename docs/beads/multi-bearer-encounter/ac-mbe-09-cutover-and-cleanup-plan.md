# ac-mbe-09: Cutover and Cleanup Plan for Superseded Desktop Encounter Logic

Status: complete (planning artifact)

## Problem Statement

Legacy encounter logic cannot be removed safely until canonical multi-bearer flow is proven by diagnostics and mixed-bearer integration evidence. A dedicated cutover/cleanup plan is required to avoid regressions and contract drift.

## Deliverables

- Staged cutover plan from legacy local encounter paths to canonical encounter-manager flow.
- Cleanup inventory of superseded modules, flags, and compatibility shims with removal gates.
- Rollback strategy and safety checks for each cutover stage.
- Final evidence checklist linking telemetry and integration-test artifacts required before removal.

## Acceptance Criteria

- Cleanup candidates are listed with explicit evidence gates (not just intent).
- Plan requires diagnostics and mixed-bearer integration outcomes before removal steps.
- Cutover stages include safe fallback and rollback criteria.
- No cleanup step depends on redefining shared Aethos contract behavior in this repo.

## Non-Goals

- No immediate code deletion.
- No protocol/spec edits.
- No additional feature work unrelated to encounter orchestration adoption.

## Desktop/Rust Risks and Edge Cases

- Hidden legacy code paths may only appear under impaired network or resume-from-suspend flows.
- Premature cleanup can remove fallback paths still needed for canonical convergence debugging.
- Multi-module cleanup can break diagnostics continuity if event contracts are not preserved.

## Dependencies (In-Repo)

- `ac-mbe-08`

## Cross-Repo Dependencies (Authoritative aethos)

- `third_party/aethos/docs/protocol/encounter.md`
- `third_party/aethos/Fixtures/Protocol/gossip-v1/README.md`
- Upstream `aethos` canonicalization completion for multi-bearer orchestration and fixtures

## Cutover/Cleanup Output

### Staged cutover plan

1. Shadow stage
  - encounter manager receives mirrored events while legacy orchestration still drives transport.
2. Controlled authority stage
  - encounter manager drives new encounters in test/staging modes; legacy kept as guarded fallback.
3. Default authority stage
  - encounter manager becomes default in desktop runtime.
4. Legacy removal stage
  - remove superseded encounter logic only after evidence gates pass.

### Cleanup candidates (planned)

- Direct orchestration calls from UI send path in `src/main.rs` that bypass encounter manager.
- Transport-specific orchestration branching in LAN background loop.
- Legacy migration flags/shims related to non-canonical encounter ownership, after one release soak window.

### Required evidence gates

- `ac-mbe-06` telemetry schema implemented and producing explainable traces.
- `ac-mbe-08` mixed-bearer integration scenarios passing with deterministic artifacts.
- Upstream canonical fixture/spec dependencies resolved for transition semantics.

### Rollback rules

- Rollback permitted only by toggling encounter-manager authority gate, not by ad hoc code edits.
- Rollback must preserve diagnostics parity to avoid blind regressions.

### Final safety checks before removal

- No unresolved plan-vs-executed mismatches in blocking scenarios.
- No unresolved interruption/resume data-loss regressions.
- No local behavior that conflicts with authoritative `aethos` contract language.
