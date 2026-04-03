# ac-mbe-08: Mixed-Bearer Integration-Test and Scheduler Verification Plan

Status: complete (planning artifact)

## Problem Statement

Before any cutover, desktop needs integration-test plans that verify canonical mixed-bearer behavior, including interruption/resume and scheduler intent-vs-execution evidence. Without this, cleanup would be speculative and high risk.

## Deliverables

- Integration-test matrix for mixed bearer combinations across discovery, control exchange, and bulk transfer phases.
- Scenario definitions for upgrade/downgrade transitions, interruption/resume, and degraded-network fallback.
- Verification approach for scheduler planned work vs executed transport behavior using diagnostics from telemetry bead.
- CI/stability guidance for deterministic Linux-first execution and artifact collection.

## Acceptance Criteria

- Plan includes pass criteria tied to canonical fixture expectations and encounter contract semantics.
- At least one scenario family covers each required explainability domain: discovery, bearer selection, transitions, interruption/resume, and plan-vs-executed.
- Test plan identifies which scenarios are blocking gates for cutover and cleanup.
- Dependencies on missing upstream fixtures/spec clarifications are explicit.

## Non-Goals

- No implementation of integration tests.
- No production behavior changes.
- No cleanup/removal work.

## Desktop/Rust Risks and Edge Cases

- Mixed-bearer tests can be flaky without controlled timing and deterministic bearer simulation hooks.
- Artifact volume can become too large for routine CI if telemetry is not scoped.
- Cross-platform differences may require Linux-first gating with later platform-specific expansion.

## Dependencies (In-Repo)

- `ac-mbe-04`
- `ac-mbe-05`
- `ac-mbe-06`
- `ac-mbe-07`

## Cross-Repo Dependencies (Authoritative aethos)

- `third_party/aethos/Fixtures/Protocol/gossip-v1/README.md`
- `third_party/aethos/docs/protocol/encounter.md`
- Upstream `aethos` mixed-bearer fixture expansions and orchestration clarifications (as published)

## Integration-Test Output

### Scenario matrix (planned)

- `MB-01 bootstrap-lan-transfer-relay`
  - discovery via LAN, control via relay, transfer via relay
- `MB-02 bootstrap-ble-or-lan-transfer-lan`
  - discovery/bootstrap-first bearer differs from bulk transfer bearer
- `MB-03 upgrade-on-quality-improvement`
  - starts on weaker transfer path, upgrades when stronger bearer becomes ready
- `MB-04 downgrade-on-impairment`
  - transfer bearer degrades, downgrade preserves progress
- `MB-05 interruption-resume`
  - forced interruption and deterministic resume behavior
- `MB-06 scheduler-plan-vs-execution`
  - validates selected transfer items and stop reasons against executed transport path

### Evidence sources

- Existing Linux-first harness: `docs/testing/gui-network-e2e.md`
- Artifacts required per run:
  - `artifact-index.json`
  - `failure-summary.json` (when failing)
  - `triage-summary.json`
  - structured runtime log extracts for encounter event taxonomy

### Blocking gate definition before cleanup bead

- No critical mismatch between scheduler plan and executed transfer in clean scenarios.
- Upgrade/downgrade and interruption/resume scenarios are deterministic across reruns.
- Canonical fixture-aligned parser/validation checks pass for all used frame types.

### CI guidance

- Linux-first gating is mandatory.
- Mixed-bearer scenarios run in reproducible named configs with explicit seeds/settings.
- Flaky scenarios require deterministic replay mode before being allowed as blockers.
