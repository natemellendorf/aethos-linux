# Multi-Bearer Encounter Execution Queue

Purpose: convert the bead plan set into an ordered execution queue and capture completion state for this effort.

## Queue Rules

- Respect dependency order from `README.md`.
- Complete each bead's defined tasks before opening dependent beads.
- Keep cross-repo authority references explicit for every bead.

## Ordered Queue

1. `ac-mbe-01` (root)
2. `ac-mbe-02` (depends on `ac-mbe-01`)
3. `ac-mbe-03` (depends on `ac-mbe-01`, `ac-mbe-02`)
4. `ac-mbe-04` (depends on `ac-mbe-03`)
5. `ac-mbe-05` (depends on `ac-mbe-03`, `ac-mbe-04`)
6. `ac-mbe-06` (depends on `ac-mbe-05`)
7. `ac-mbe-07` (depends on `ac-mbe-02`)
8. `ac-mbe-08` (depends on `ac-mbe-04`, `ac-mbe-05`, `ac-mbe-06`, `ac-mbe-07`)
9. `ac-mbe-09` (depends on `ac-mbe-08`)

## Task Pack and Completion Status

### `ac-mbe-01`

- Tasks:
  - inventory current desktop encounter/discovery/session code paths
  - classify decision points (keep/adapt/remove)
  - produce contract gap matrix
  - define dependency-safe sequencing for downstream beads
- Status: complete
- Artifact: `ac-mbe-01-audit-current-desktop-encounter-paths.md`

### `ac-mbe-02`

- Tasks:
  - map canonical encounter/bearer concepts to Rust boundaries
  - define owner vs adapter interfaces
  - define contract conformance checklist
  - identify Rust-local abstractions to convert into adapters
- Status: complete
- Artifact: `ac-mbe-02-rust-adoption-of-canonical-bearer-orchestration-models.md`

### `ac-mbe-03`

- Tasks:
  - define encounter-manager ownership model
  - define transport integration boundaries
  - define staged integration rollout
  - define handoff criteria for discovery/transition/telemetry beads
- Status: complete
- Artifact: `ac-mbe-03-desktop-encounter-manager-integration-plan.md`

### `ac-mbe-04`

- Tasks:
  - define discovery/bootstrap role model per bearer
  - define BLE bootstrap-first posture and non-BLE fallback behavior
  - define normalized discovery event contract
  - define deterministic handoff criteria to control exchange
- Status: complete
- Artifact: `ac-mbe-04-discovery-bootstrap-bearer-adoption-plan.md`

### `ac-mbe-05`

- Tasks:
  - define explicit upgrade/downgrade transition states
  - define trigger matrix and anti-thrashing safety constraints
  - define interruption/resume transfer continuity expectations
  - define scheduler interaction rule without policy reinterpretation
- Status: complete
- Artifact: `ac-mbe-05-transfer-bearer-upgrade-downgrade-plan.md`

### `ac-mbe-06`

- Tasks:
  - define telemetry taxonomy across required explainability domains
  - define required event fields and correlation keys
  - align with existing structured log pipeline
  - define privacy/redaction constraints
- Status: complete
- Artifact: `ac-mbe-06-desktop-telemetry-diagnostics-explainability-plan.md`

### `ac-mbe-07`

- Tasks:
  - map authoritative fixture sources and families
  - define fixture ingestion/versioning approach for desktop
  - document missing upstream mixed-bearer fixture dependencies
  - define local adapter rules for fixture setup vs expected outcomes
- Status: complete
- Artifact: `ac-mbe-07-mixed-bearer-fixture-consumption-plan.md`

### `ac-mbe-08`

- Tasks:
  - define mixed-bearer integration scenario matrix
  - define scheduler plan-vs-executed verification approach
  - define evidence artifacts and deterministic CI guidance
  - define blocking gates for cleanup cutover
- Status: complete
- Artifact: `ac-mbe-08-mixed-bearer-integration-test-plan.md`

### `ac-mbe-09`

- Tasks:
  - define staged cutover plan
  - define cleanup candidate inventory and gates
  - define rollback model
  - define final pre-removal safety checks
- Status: complete
- Artifact: `ac-mbe-09-cutover-and-cleanup-plan.md`

## Final State

- All beads in this effort are complete as planning/adoption artifacts.
- No runtime code implementation is included in this execution queue pass.
