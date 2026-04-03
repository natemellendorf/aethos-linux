# Encounter Routing Audit and Shadow-Mode Migration Note

This note documents Rust transfer-planning behavior, the shadow-mode comparison path, and the post-shadow cutover where Encounter Scheduler v1 becomes the primary planner.

## Current status

- Primary transfer planner is now `EncounterSchedulerV1`.
- Legacy size-first ordering is isolated to a debug/test fallback path only (`AETHOS_ROUTING_LEGACY_FALLBACK=1`).
- Shadow diff telemetry remains available in debug/test mode to compare primary scheduler output against legacy ordering.

## Current planner audit (live path)

### Where candidates are gathered

- Entry point: `transfer_items_for_request_with_shadow_context` in `src/aethos_core/gossip_sync.rs`.
- Candidate lookup: `gossip_store_sqlite::transfer_candidates_for_request` in `src/aethos_core/gossip_store_sqlite.rs`.
- SQL lookup behavior:
  - loops in incoming `REQUEST.want` order,
  - per-id query with expiry gate (`expiry_unix_ms > now_ms + 30s`),
  - returns only IDs explicitly requested by peer.

### Where ordering decisions are made

- Ordering is in-memory in `transfer_items_for_request_with_shadow_context`:
  - sort by `envelope_b64.len()` ascending,
  - then by `item_id` ascending.
- Selection then takes a prefix subject to transfer limits.

### Whether implicit FIFO exists

- No strict FIFO queue order is used in live transfer planning.
- `recorded_at_unix_ms` is not used in transfer ranking.
- Existing pseudo-ordering is size-first, then item-id lexicographic.

### Whether direct-destination bias exists

- The live planner does not decode destination and does not apply direct/transit bias.
- Any direct/transit effect is incidental (for example via payload size), not explicit policy.

### Where nondeterminism can enter

- Core ranking/selection is deterministic for a fixed candidate set.
- Sources of drift are upstream of ranking:
  - candidate availability timing in sqlite (what exists at request time),
  - concurrent writes/pruning,
  - incoming `REQUEST.want` composition from peer state.
- For equal-size items, deterministic `item_id` tie-break is used.

### How current budgets are enforced

- Item cap: stop when selected count reaches `min(max_items, MAX_TRANSFER_ITEMS)`.
- Byte cap: stop on first candidate that would exceed `min(max_bytes, MAX_TRANSFER_BYTES)`.
- Current stop reasons are effectively:
  - `completed`,
  - `budget-items-exhausted`,
  - `budget-bytes-exhausted`.

## Shadow-mode comparison (this bead)

### Behavior

- Live output remains legacy planner output (no cutover in this bead).
- In debug/test builds, canonical `EncounterSchedulerV1` runs in parallel as a shadow planner.
- Shadow mode defaults to enabled in debug/test and can be disabled with:
  - `AETHOS_ROUTING_SHADOW_MODE=0|false|off`.

### Scope

- Live path wiring:
  - relay request handling now calls `transfer_items_for_request_with_shadow_context(..., Some(peer_hello.node_id))`.
  - legacy transfer objects returned to wire are unchanged.
- Release behavior:
  - shadow scheduler does not execute in release builds.

## Diff telemetry emitted in shadow mode

When a delta is found, `log_verbose` emits `transfer_scheduler_shadow_diff` containing:

- `old_top_n` vs `new_top_n` (top-5 ranking IDs),
- changed first selected item (+ old/new values),
- changed stop reason (+ old/new values),
- changed tier distribution (`tier0..tier5`) in selected prefix,
- changed transit/direct ratio in selected prefix.

The telemetry is generated from identical candidate snapshots used by the legacy planner.

## Mismatch characteristics observed by design

Expected mismatch vectors between legacy and canonical shadow planners:

- hard tier dominance can override size-first ordering,
- scarcity/safety/expiry/stagnation weighting can reorder same-tier items,
- legacy byte-stop behavior is first-overflow-stop; canonical path can differ when ranking changes,
- direct/transit proximity can influence canonical ranking where legacy has no explicit bias.

## Cutover recommendation sequence

1. Run shadow mode in developer and CI debug/test lanes; collect diff telemetry baselines.
2. Triage high-frequency deltas by lane (top-N changes, first-item flips, stop-reason shifts).
3. Validate that deltas are explainable and aligned with canonical fixture expectations.
4. Add guard metrics for transit-heavy and byte-constrained scenarios.
5. Introduce a runtime opt-in flag to switch live planner to canonical scheduler in non-release test environments.
6. After stable soak and review, make canonical planner default and keep legacy as fallback for one release window.

No wire schema, frame semantics, or protocol acceptance behavior is changed by this shadow-mode bead.
