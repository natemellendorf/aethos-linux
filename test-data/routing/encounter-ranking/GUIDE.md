# Encounter Ranking Golden Fixture Guide

These fixtures are authoritative conformance vectors for the scheduler contract in `docs/protocol/encounter-scheduler-v1.md`.

## 1) Discover and load

1. Read `manifest.json` to discover required lanes.
2. For each fixture, load the JSON payload as-is.
3. Treat each fixture's `nowUnixMs` as authoritative test-time input (do not replace with wall clock).

## 2) Validate fixture shape

Each fixture MUST validate against `schema.json` (`encounter-ranking.v1`).

Minimum check:

- parse/format check with `python3 -m json.tool`

Recommended check:

- JSON Schema validation against `Fixtures/Routing/encounter-ranking/schema.json`.

## 3) Execute ranking deterministically

For each fixture:

1. Use fixture `encounterClass`, `budgetProfile`, `nowUnixMs`, and `cargoItems` as scheduler input.
2. Apply hard tier-first ordering exactly as defined in `docs/protocol/encounter-scheduler-v1.md` section **3. Hard tier-first ordering (global)** (`tier` ascending is globally dominant).
3. Compute intra-tier scores using the canonical fixed-point path (`scoreNumerator` integer comparisons).
4. Apply selection constraints and stop-reason precedence exactly as defined in `docs/protocol/encounter-scheduler-v1.md` section **10. Selection prefix and stop reason** and **10.1 Canonical constraint evaluation order**.
5. Produce:
   - full `rankingOrder`
   - selected `selectedPrefix` under budget constraints
   - per-item score breakdowns
   - terminal `stopReason`
   - `tieBreakReason` where relevant

## 4) Compare expected outputs

Conformance requires exact match on:

- item IDs and order in `rankingOrder`
- item IDs and order in `selectedPrefix`
- per-item `scoreBreakdowns.components` values (6-digit millionth precision)
- per-item `scoreNumerator` (authoritative)
- per-item diagnostic `score`
- `stopReason`
- `tieBreakReason`

## 5) Repeated-run stability check

For `repeated-run-stability-mixed-ties.json`, run the same input multiple times in a fresh process and assert deterministic equality with one of these methods:

- **Preferred (cross-language): structural equality**
  - Compare object fields by key/value equality.
  - Arrays `rankingOrder`, `selectedPrefix`, and `scoreBreakdowns` MUST match exactly in element order.
  - For each `scoreBreakdowns` entry, compare `itemID`, each `components` field value, `scoreNumerator`, and `score` exactly.
- **Optional: canonical serialization**
  - Serialize only deterministic output fields using a project-defined canonical JSON routine and require byte-for-byte identity across repeated runs.

Either method is acceptable for fixture conformance so long as ordered arrays are treated as order-sensitive.

## 6) Acceptance mapping locked by this suite

| Acceptance criterion | Fixture ID(s) |
| --- | --- |
| Blink: receipts/checkpoints/tiny endangered before bulk cargo | `blink-encounter-priority`, `receipt-checkpoint-protection` |
| Short: drains high-value messages before larger attachments | `short-drain-messages-before-attachments` |
| Durable: allocates cargo budget without starving control traffic | `durable-allocates-cargo-without-starving-control` |
| Transit-vs-direct: low-replication transit can outrank direct-only metadata | `transit-vs-direct-tier-precedence` |
| Media-starvation-guard: large media deferred unless durable budget permits | `media-starvation-guard-short-budget` |
| Same-tier exact tie: deterministic tie-break chain exercised | `same-tier-exact-tie-itemid-determinism`, `repeated-run-stability-mixed-ties` |
| Repeated-run stability: deterministic repeated outputs | `repeated-run-stability-mixed-ties` |
| Tier-ordering invariant proof (score cannot bypass tier) | `receipt-checkpoint-protection`, `transit-vs-direct-tier-precedence` |
| Weighted-field coverage proof (all weighted fields exercised across suite) | `blink-encounter-priority`, `short-drain-messages-before-attachments`, `durable-allocates-cargo-without-starving-control`, `receipt-checkpoint-protection`, `repeated-run-stability-mixed-ties` |
