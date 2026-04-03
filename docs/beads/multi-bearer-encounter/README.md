# Desktop Rust Multi-Bearer Encounter Bead Graph

Purpose: define the execution graph for Rust/desktop adoption of the next Aethos multi-bearer encounter phase.

Execution queue: `docs/beads/multi-bearer-encounter/execution-queue.md`

Scope of this graph:
- Planning beads only (no implementation in this pass).
- Desktop client adopts the authoritative Aethos contract; it does not redefine routing policy.
- Discovery, control exchange, and bulk transfer may use different bearers.
- BLE is treated as discovery/bootstrap-first where applicable, not assumed as default bulk bearer.

## Bead Index

1. `ac-mbe-01` - Audit current desktop encounter/discovery/session paths
2. `ac-mbe-02` - Rust adoption plan for canonical bearer/orchestration interfaces and models
3. `ac-mbe-03` - Desktop encounter-manager integration plan
4. `ac-mbe-04` - Discovery/bootstrap bearer adoption plan
5. `ac-mbe-05` - Transfer bearer upgrade/downgrade strategy plan
6. `ac-mbe-06` - Desktop telemetry/diagnostics/explainability contract
7. `ac-mbe-07` - Mixed-bearer fixture-consumption mapping plan
8. `ac-mbe-08` - Mixed-bearer integration-test and scheduler verification plan
9. `ac-mbe-09` - Cutover and cleanup plan for superseded encounter logic

## Dependency Graph

`ac-mbe-01` -> `ac-mbe-02` -> `ac-mbe-03`

`ac-mbe-03` -> `ac-mbe-04` -> `ac-mbe-05` -> `ac-mbe-06`

`ac-mbe-02` -> `ac-mbe-07` -> `ac-mbe-08`

`ac-mbe-04` -> `ac-mbe-08`

`ac-mbe-05` -> `ac-mbe-08`

`ac-mbe-06` -> `ac-mbe-08` -> `ac-mbe-09`

## Cross-Repo Authority Gates (aethos)

These beads are intentionally adoption-focused and depend on authoritative `aethos` outputs:
- Encounter/orchestration contract: `third_party/aethos/docs/protocol/encounter.md`
- Gossip transport and bearer frame invariants: `third_party/aethos/docs/protocol/gossip.md` and `third_party/aethos/docs/protocol/frames.md`
- Source-of-truth contract governance: `third_party/aethos/docs/adr/ADR-0001-protocol-contract-source-of-truth.md`
- Runtime architecture baseline: `third_party/aethos/docs/adr/ADR-0002-runtime-architecture-gossip-v1.md`
- Canonical interoperability fixtures: `third_party/aethos/Fixtures/Protocol/gossip-v1/README.md`

If upstream `aethos` introduces new multi-bearer ADR/spec/fixture beads, this graph should link to those IDs before execution begins.

## Execution Status

- `ac-mbe-01` complete (audit artifact documented)
- `ac-mbe-02` complete (canonical adoption interface plan documented)
- `ac-mbe-03` complete (encounter-manager integration plan documented)
- `ac-mbe-04` complete (discovery/bootstrap bearer plan documented)
- `ac-mbe-05` complete (upgrade/downgrade strategy plan documented)
- `ac-mbe-06` complete (telemetry/diagnostics/explainability plan documented)
- `ac-mbe-07` complete (fixture-consumption plan documented)
- `ac-mbe-08` complete (integration-test and scheduler verification plan documented)
- `ac-mbe-09` complete (cutover/cleanup plan documented)

Completion note: these are planning/adoption beads only. They intentionally do not implement runtime code.
