# ac-mbe-05: Transfer Bearer Upgrade/Downgrade Strategy Plan

Status: complete (planning artifact)

## Problem Statement

Desktop needs a canonical strategy for moving from bootstrap/control bearers to stronger transfer bearers, and for downgrading under impairment. Without an explicit plan, upgrade/downgrade behavior becomes ad hoc and hard to verify against shared fixtures and scheduler expectations.

## Deliverables

- Upgrade/downgrade decision framework aligned with canonical encounter contract boundaries.
- Trigger matrix for upgrade candidates, downgrade triggers, and recovery behavior after interruption.
- Interaction plan between encounter manager and transfer scheduler so planned work vs executed bearer path remains observable.
- Safety constraints that prevent unsafe bearer thrashing and policy reinterpretation.

## Acceptance Criteria

- Upgrade and downgrade transitions are represented as explicit state transitions, not implicit side effects.
- Plan supports discovery, control exchange, and bulk transfer on different bearers.
- Downgrade path preserves resumability guarantees expected by canonical orchestration.
- Strategy is framed as adoption of shared contract, not a new desktop routing policy.

## Non-Goals

- No runtime algorithm implementation.
- No performance tuning.
- No scheduler policy redesign.

## Desktop/Rust Risks and Edge Cases

- Desktop bearer quality signals may be noisy or unavailable on some hosts.
- Concurrent transfer tasks can continue on stale bearer assumptions during transition.
- Partial failure modes (control bearer alive, transfer bearer degraded) need deterministic precedence handling.

## Dependencies (In-Repo)

- `ac-mbe-03`
- `ac-mbe-04`

## Cross-Repo Dependencies (Authoritative aethos)

- `third_party/aethos/docs/protocol/encounter.md`
- `third_party/aethos/docs/protocol/gossip.md`
- Canonical fixture updates in `third_party/aethos/Fixtures/Protocol/gossip-v1/` for upgrade/downgrade scenarios

## Upgrade/Downgrade Output

### Transition states

- `TransferPathBootstrap`
- `TransferPathCandidateUpgrade`
- `TransferPathUpgraded`
- `TransferPathDegraded`
- `TransferPathDowngraded`
- `TransferPathRecovering`

### Upgrade trigger classes (contract-aligned)

- control exchange confirms stronger bearer capability and readiness.
- quality/throughput/latency indicators exceed minimum transfer thresholds.
- encounter budget and scheduler plan indicate benefit from upgrade.

### Downgrade trigger classes

- repeated frame send/read failures above threshold.
- sustained no-progress streak while encounter still active.
- control bearer alive but transfer bearer unhealthy.

### Safety constraints

- minimum dwell time before repeated transitions (anti-thrash).
- bounded retries per encounter phase.
- explicit resume token/state handoff when downgrading during active transfer.

### Scheduler interaction rule

- Scheduler remains planner of item order under canonical scoring.
- Encounter manager owns bearer transition timing and records plan-vs-executed mismatch diagnostics.

### Explicit non-reinterpretation guard

- This plan does not introduce new routing scores, tie-breakers, or policy semantics.
- Any transition semantics that alter canonical behavior are upstream `aethos` concerns and must not be invented locally.
