# ac-mbe-06: Desktop Telemetry, Diagnostics, and Explainability Plan

Status: complete (planning artifact)

## Problem Statement

Multi-bearer orchestration is difficult to validate and debug without canonical observability. Desktop currently lacks a unified explainability contract for why discovery was accepted, why a bearer was chosen, why transitions happened, and whether scheduler intent matched runtime execution.

## Deliverables

- Telemetry taxonomy for discovery events, bearer selection, upgrade/downgrade transitions, interruption/resume, and scheduler plan-vs-executed outcomes.
- Structured diagnostics schema proposal for logs/artifacts used by desktop tests and GUI diagnostics views.
- Correlation strategy across encounter ID, session ID, bearer role, and transfer operations.
- Privacy/safety review notes for telemetry fields emitted on desktop.

## Acceptance Criteria

- Each required explainability domain has named events and required fields.
- Schema supports determining both intent (planned) and actual behavior (executed).
- Telemetry contract aligns with canonical encounter lifecycle terminology.
- Output is usable by later mixed-bearer test and cutover beads as objective evidence.

## Non-Goals

- No telemetry implementation.
- No dashboard UI implementation.
- No changes to protocol payloads.

## Desktop/Rust Risks and Edge Cases

- Excessive logging during bursty discovery could impact desktop runtime stability.
- Missing correlation IDs across async boundaries can make diagnostics non-actionable.
- Log redaction needs to avoid leaking sensitive network/context metadata.

## Dependencies (In-Repo)

- `ac-mbe-05`

## Cross-Repo Dependencies (Authoritative aethos)

- `third_party/aethos/docs/protocol/encounter.md`
- `third_party/aethos/docs/protocol/gossip.md`
- Future `aethos` authoritative observability guidance for multi-bearer orchestration (if introduced)

## Telemetry/Diagnostics Output

### Required explainability domains

- discovery events
- bearer selection
- upgrade/downgrade transitions
- interruption/resume
- scheduler planned work vs executed transfer path

### Canonical event taxonomy (desktop)

- `encounter_discovery_observed`
- `encounter_discovery_accepted`
- `encounter_discovery_rejected`
- `encounter_control_exchange_started`
- `encounter_bearer_selected`
- `encounter_bearer_upgrade_attempted`
- `encounter_bearer_upgrade_applied`
- `encounter_bearer_downgrade_applied`
- `encounter_transfer_interrupted`
- `encounter_transfer_resumed`
- `encounter_scheduler_plan_emitted`
- `encounter_scheduler_plan_executed`
- `encounter_scheduler_plan_mismatch`
- `encounter_closed`

### Required correlation keys

- `encounter_id`
- `session_id`
- `peer_node_id`
- `bearer_role` (discovery/control/transfer)
- `bearer_adapter`
- `plan_id` (scheduler plan)
- `message_ids`/`item_ids` (when applicable)

### Existing logging integration baseline

- Reuse structured logging pipeline in `src/aethos_core/logging.rs` (`AETHOS_STRUCTURED_LOGS`).
- Keep machine-readable artifacts compatible with existing E2E triage workflow.

### Privacy/safety constraints

- Do not emit raw payload bytes or sensitive auth material.
- Hash/redact endpoint and peer-network metadata where not needed for diagnosis.
- Allow diagnostics verbosity controls to prevent excessive event volume.
