# ac-mbe-02: Rust Adoption of Canonical Bearer/Orchestration Interfaces and Models

Status: complete (planning artifact)

## Problem Statement

Desktop Rust needs a canonical interface/model surface for multi-bearer encounter orchestration. Without this, integration work will continue to encode Rust-specific behavior that conflicts with shared Aethos contracts and fixture expectations.

## Deliverables

- Rust-facing model/interface adoption plan that maps canonical Aethos concepts into desktop modules without redefining semantics.
- Type/interface boundary map showing where canonical encounter, bearer role, and orchestration state are represented.
- Contract conformance checklist for parser/encoder boundaries and orchestration state transitions.
- Migration notes that identify local abstractions to keep as adapters only (not semantic owners).

## Acceptance Criteria

- Plan references authoritative contracts for encounter and frame behavior and maps each canonical concept to a Rust location.
- No new Rust-local routing policy is introduced in the proposed interfaces.
- Discovery, control exchange, and bulk transfer roles are represented as separable bearer responsibilities.
- The plan defines how later beads can verify conformance against canonical fixtures.

## Non-Goals

- No implementation of new Rust types or APIs.
- No fixture execution.
- No cutover/removal of legacy code.

## Desktop/Rust Risks and Edge Cases

- Strong typing can accidentally freeze provisional assumptions before upstream canonicalization is complete.
- Existing module boundaries may force temporary adapter layers to avoid broad refactors.
- Capability metadata for bearer selection may be sparse in current desktop runtime.

## Dependencies (In-Repo)

- `ac-mbe-01`

## Cross-Repo Dependencies (Authoritative aethos)

- `third_party/aethos/docs/adr/ADR-0001-protocol-contract-source-of-truth.md`
- `third_party/aethos/docs/protocol/encounter.md`
- `third_party/aethos/docs/protocol/gossip.md`

## Adoption Output

### Canonical model adoption map (Rust-side)

- Encounter identity/state
  - Adopt shared encounter lifecycle terms as first-class Rust model states.
  - Proposed owner module target: `aethos_core::encounter_manager` (new in later implementation beads).
- Bearer role separation
  - Discovery/bootstrap bearer role
  - Control exchange bearer role
  - Bulk transfer bearer role
  - Each role represented as capability metadata, not hardcoded transport assumptions.
- Frame and transfer contract
  - Continue using `aethos_core::gossip_sync` as canonical frame parser/validator boundary.
  - Keep transport adapters thin: they pass canonical frame/model payloads into encounter manager.

### Interface boundary plan

- Encounter manager boundary (owner)
  - Accept normalized discovery events and session intents.
  - Emit deterministic actions: establish control exchange, issue request/summary, start/stop transfer, emit diagnostics.
- Bearer adapter boundary (non-owner)
  - Relay websocket adapter, LAN datagram adapter, future BLE adapter.
  - Adapters expose capability + health signals and frame send/receive mechanics only.
- Scheduler boundary
  - Scheduler remains transfer planner; encounter manager decides when planning is invoked.
  - No scheduler policy forks in adapter/UI modules.

### Contract conformance checklist for future execution beads

- MUST keep frame validation/encoding at canonical boundary (`gossip_sync`).
- MUST keep bearer framing semantics aligned with `docs/protocol/frames.md`.
- MUST avoid local reinterpretation of encounter transition semantics.
- MUST preserve transport-neutral behavior at orchestration layer.
- MUST verify behavior against authoritative fixtures before cleanup.

### Rust-local abstractions to convert into adapters

- `start_background_gossip_sync` flow in `src/main.rs` -> discovery/control datagram adapter.
- Relay encounter loop in `src/relay/client.rs` -> stream bearer adapter.
- UI-triggered send/poll orchestration in `src/main.rs` -> encounter intent producer only.
