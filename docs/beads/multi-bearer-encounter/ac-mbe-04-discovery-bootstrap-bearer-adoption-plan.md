# ac-mbe-04: Discovery/Bootstrap Bearer Adoption Plan

Status: complete (planning artifact)

## Problem Statement

Multi-bearer encounter architecture requires desktop discovery/bootstrap behavior that aligns with the shared model. Without explicit planning, desktop can incorrectly treat a single bearer (especially BLE) as the primary transfer path and diverge from canonical orchestration intent.

## Deliverables

- Discovery/bootstrap bearer plan defining bearer responsibilities, capability advertisement assumptions, and handoff points into encounter manager.
- BLE-first-bootstrap guidance for desktop where BLE is available, including fallback behavior when BLE is absent/limited.
- Discovery event normalization plan so upstream orchestration receives canonical encounter signals independent of local bearer implementation.
- Preconditions for triggering control exchange after discovery.

## Acceptance Criteria

- Plan clearly states BLE is discovery/bootstrap-first, not default bulk transfer.
- Discovery path is transport-neutral at the orchestration boundary.
- Handoff criteria from discovery to control exchange are deterministic and measurable.
- Edge behaviors (duplicate discovery events, stale advertisements, transient availability) are covered.

## Non-Goals

- No BLE stack implementation work.
- No transfer bearer policy implementation.
- No fixture execution.

## Desktop/Rust Risks and Edge Cases

- Linux desktop environments can have inconsistent BLE adapter permissions/availability.
- LAN-only environments must remain first-class without synthetic BLE assumptions.
- Rapid flapping discovery events can create oscillating encounter starts without debounce/backpressure plan.

## Dependencies (In-Repo)

- `ac-mbe-03`

## Cross-Repo Dependencies (Authoritative aethos)

- `third_party/aethos/docs/protocol/encounter.md`
- `third_party/aethos/docs/protocol/gossip.md`
- Future `aethos` multi-bearer discovery ADR/spec beads (when published)

## Discovery/Bootstrap Output

### Role model

- Discovery/bootstrap bearer is responsible for peer presence and bootstrap metadata exchange.
- Control and bulk transfer bearers are selected after bootstrap; they are not implicitly the same bearer.
- BLE posture: discovery/bootstrap-first when available; never assumed primary bulk transport.

### Normalized discovery event contract

- Required fields: `discovery_event_id`, `peer_node_id` (if known), `transport_hint`, `capabilities_snapshot`, `observed_at_unix_ms`, `signal_quality_hint`, `origin_adapter`.
- Output event is transport-neutral and consumed by encounter manager.

### Deterministic handoff criteria to control exchange

- peer identity passes canonical hello validation (or equivalent upstream bootstrap rule).
- bootstrap capabilities indicate at least one feasible control-exchange bearer.
- discovery debounce window has not flagged flapping instability.
- manager dedupe confirms no active equivalent encounter for same peer.

### Linux desktop edge policy (adoption-safe)

- If BLE unavailable/blocked, LAN and relay discovery paths remain valid bootstrap sources.
- Discovery adapter must emit explicit `capability_absent` flags instead of silent fallback.
- Burst discovery events use debounce + backpressure before encounter start intent.

### Cross-repo dependency note

- Any BLE metadata schema, capability tokens, or bootstrap contract details must come from authoritative `aethos` ADR/spec beads when published; this repo only maps those contracts into desktop adapters.
