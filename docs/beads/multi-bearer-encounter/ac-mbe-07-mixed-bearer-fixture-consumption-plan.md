# ac-mbe-07: Mixed-Bearer Fixture Consumption Plan

Status: complete (planning artifact)

## Problem Statement

Desktop adoption must be validated against authoritative fixture expectations, but fixture consumption paths are not yet mapped for mixed-bearer encounter behavior. Without a fixture-consumption plan, desktop risks inventing local test assumptions and drifting from the shared contract.

## Deliverables

- Fixture ingestion plan mapping authoritative `aethos` fixture families to desktop test harness inputs.
- Canonical fixture-version pinning and update strategy for this repo.
- Gap list for missing upstream mixed-bearer fixture cases, with explicit cross-repo requests.
- Adapter plan for translating fixture scenarios into desktop runtime setup without altering canonical expectations.

## Acceptance Criteria

- Plan names the authoritative fixture source locations and how desktop will consume them.
- Expected outcomes for mixed-bearer scenarios are inherited from upstream fixtures, not redefined locally.
- Missing fixture coverage is documented as upstream dependency rather than patched by local behavior.
- Fixture-consumption plan can directly feed integration-test planning bead.

## Non-Goals

- No new fixture format definition in this repo.
- No fixture execution implementation.
- No protocol contract changes.

## Desktop/Rust Risks and Edge Cases

- Fixture lifecycle churn upstream can break deterministic local test baselines.
- Desktop harness may need normalization for timing-sensitive mixed-bearer scenarios.
- Local environment constraints (BLE unavailable) may require fixture capability tagging.

## Dependencies (In-Repo)

- `ac-mbe-02`

## Cross-Repo Dependencies (Authoritative aethos)

- `third_party/aethos/Fixtures/Protocol/gossip-v1/README.md`
- `third_party/aethos/docs/adr/ADR-0001-protocol-contract-source-of-truth.md`
- Upstream `aethos` multi-bearer fixture-definition beads (required for complete coverage)

## Fixture Consumption Output

### Authoritative fixture source mapping

- Core source: `third_party/aethos/Fixtures/Protocol/gossip-v1/`
- Current fixture families consumed as baseline:
  - hello/version/validation vectors
  - summary/request/transfer/receipt vectors
  - relay-ingest and malformed/oversize transfer vectors

### Desktop fixture ingestion strategy

- Phase 1: use canonical `.json` + `.cbor` vectors as parser/validator conformance inputs.
- Phase 2: compose mixed-bearer scenario bundles by combining authoritative vectors with encounter-phase scripts (no schema changes).
- Phase 3: lock fixture version references in test docs and CI metadata to avoid accidental drift.

### Upstream dependency gaps (explicit)

- Missing dedicated multi-bearer encounter transition fixtures (upgrade/downgrade/interruption-resume) are upstream dependencies.
- This repo will not invent substitute normative expectations; it will track provisional harness-only scenarios as non-authoritative until upstream fixtures land.

### Local adapter rule

- Desktop adapters may transform fixture setup mechanics (ports, endpoints, transport handles), but must not alter expected canonical outcomes.
