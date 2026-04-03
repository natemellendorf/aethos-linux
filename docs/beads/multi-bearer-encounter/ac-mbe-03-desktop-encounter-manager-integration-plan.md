# ac-mbe-03: Desktop Encounter-Manager Integration Plan

Status: complete (planning artifact)

## Problem Statement

Desktop client runtime currently coordinates discovery and transfer behavior across multiple modules. A clear encounter-manager integration plan is required so canonical orchestration becomes the control point without accidental policy forks.

## Deliverables

- Integration blueprint for encounter-manager ownership boundaries in desktop runtime.
- Event/state-flow diagrams for encounter start, control exchange, transfer phase, interruption, and resume.
- Adapter plan for existing modules (relay path, LAN path, scheduler path) to plug into canonical orchestration flow.
- Backward-compatible rollout stages (shadow and validation stages only; no cleanup in this bead).

## Acceptance Criteria

- Plan defines one authoritative runtime owner for encounter lifecycle transitions.
- Proposed integration points explicitly separate orchestration decisions from bearer-specific transport adapters.
- Failure handling paths include interruption and resume semantics aligned to canonical encounter contract.
- Dependencies and handoff criteria for discovery, transfer strategy, and telemetry beads are explicit.

## Non-Goals

- No runtime cutover.
- No transport implementation changes.
- No routing-policy changes.

## Desktop/Rust Risks and Edge Cases

- Existing async task boundaries may race encounter state updates across threads.
- UI-driven actions can trigger overlapping session intents if ownership boundaries remain unclear.
- Partial session cleanup on app suspend/restart can leave stale encounter state.

## Dependencies (In-Repo)

- `ac-mbe-01`
- `ac-mbe-02`

## Cross-Repo Dependencies (Authoritative aethos)

- `third_party/aethos/docs/protocol/encounter.md`
- `third_party/aethos/docs/adr/ADR-0002-runtime-architecture-gossip-v1.md`

## Integration Output

### Authoritative owner model

- Single owner for encounter lifecycle: encounter manager.
- UI and transport modules become event producers/consumers, not policy owners.

### Planned event/state flow

- `DiscoveryObserved` -> `BootstrapCandidateAccepted` -> `ControlExchangeStarted` -> `ControlExchangeReady` -> `TransferPlanned` -> `TransferExecuting` -> `TransferInterrupted|TransferCompleted` -> `EncounterClosed`

### Integration staging

1. Shadow wiring stage
  - Keep existing relay/LAN loops active.
  - Mirror events into encounter-manager interface and compare decisions in diagnostics.
2. Authority handoff stage
  - Encounter manager becomes source of transfer/control intents.
  - Transport loops execute emitted actions only.
3. Stabilization stage
  - Validate interruption/resume and no-progress convergence behavior under mixed-bearer scenarios.

### Module-level integration points

- `src/main.rs`
  - replace direct orchestration in `start_background_gossip_sync` and send-path encounter trigger with manager intents.
- `src/relay/client.rs`
  - relay round loop converted to adapter callbacks under manager-owned lifecycle.
- `src/aethos_core/gossip_sync.rs`
  - remains canonical data/validation layer and scheduler entrypoint.

### Handoff criteria to downstream beads

- Discovery/bootstrap integration contracts frozen (`ac-mbe-04`).
- Upgrade/downgrade transition contracts frozen (`ac-mbe-05`).
- Telemetry schema can fully explain manager decisions (`ac-mbe-06`).
