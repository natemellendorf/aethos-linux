# ac-mbe-01: Audit Current Desktop Encounter/Discovery/Session Paths

Status: complete (planning artifact)

## Problem Statement

The desktop client has evolving encounter, discovery, and gossip session code paths that predate the next multi-bearer phase. Without a concrete audit, later adoption risks preserving hidden Rust-local assumptions that drift from the authoritative Aethos orchestration model.

## Deliverables

- Current-state architecture map covering discovery triggers, encounter/session lifecycle, bearer touchpoints, and scheduler usage in desktop Rust runtime.
- Explicit inventory of policy-bearing decisions currently embedded in client code, including where they duplicate or diverge from `aethos` contracts.
- Gap matrix: current behavior vs authoritative contract expectations (by lifecycle stage: discovery, control exchange, bulk transfer, interruption/resume).
- Recommended ordering of integration points to enable canonical adoption with minimal churn.

## Acceptance Criteria

- Audit artifact names exact modules/functions where encounter/discovery/session behavior is implemented today.
- Every identified decision point is classified as: keep (already canonical), adapt (needs alignment), or remove (local-only behavior).
- Audit explicitly distinguishes discovery/bootstrap bearer behavior from bulk-transfer bearer behavior.
- Findings are traceable to authoritative references in `third_party/aethos/docs/protocol/*.md`.

## Non-Goals

- No runtime behavior changes.
- No protocol or routing-policy design changes.
- No fixture or test harness implementation.

## Desktop/Rust Risks and Edge Cases

- Existing scheduler internals may blend selection policy with transport capability assumptions.
- Desktop networking stack differences (LAN sockets vs relay websocket path) may obscure where encounter state is actually authoritative.
- Legacy fallback/debug paths may appear dormant but still influence production behavior.

## Dependencies (In-Repo)

- None (root bead).

## Cross-Repo Dependencies (Authoritative aethos)

- `third_party/aethos/docs/protocol/encounter.md`
- `third_party/aethos/docs/protocol/gossip.md`
- `third_party/aethos/docs/protocol/frames.md`

## Audit Output

### Current runtime map (desktop Rust)

- Discovery/bootstrap bearer (LAN datagram): `start_background_gossip_sync` in `src/main.rs:3087`
  - UDP bind/listen on `GOSSIP_LAN_PORT`
  - periodic HELLO broadcast every ~3s
  - HELLO -> SUMMARY -> REQUEST -> TRANSFER -> RECEIPT loop over unicast reply
- Relay encounter bearer (stream/websocket): `run_relay_encounter_gossipv1` in `src/relay/client.rs:453`
  - explicit HELLO handshake (`complete_hello_handshake`)
  - encounter round sends SUMMARY + RELAY_INGEST, then processes SUMMARY/REQUEST/TRANSFER/RECEIPT
  - short encounter window (`Duration::from_secs(3)` default)
- Shared frame/model/validation logic: `src/aethos_core/gossip_sync.rs`
  - frame build/parse/validate (`build_hello_frame`, `build_summary_frame`, `build_request_frame`, `parse_frame`, `validate_frame`)
  - transfer import/export and storage boundary (`transfer_items_for_request_with_shadow_context`, `import_transfer_items`)
- Storage + candidate lookup: `src/aethos_core/gossip_store_sqlite.rs`
  - request lookup by requested item ids and expiry (`transfer_candidates_for_request`)
  - summary preview ranking input (`summary_preview_candidates`)

### Decision points found (keep/adapt/remove)

- Keep (already canonical-shaped)
  - strict frame validation and item-id hash invariants in `gossip_sync`
  - datagram transport semantics (one frame payload per datagram exchange path)
  - stream encounter handshake gating in relay path
- Adapt (needs canonical orchestration ownership)
  - encounter lifecycle ownership split between `main.rs` (LAN) and `relay/client.rs` (relay)
  - relay encounter emits control/data flow directly from transport loop, not from a shared encounter manager
  - request/summary candidate composition includes local heuristics that should be owned by canonical orchestration contract, not UI/runtime glue
  - scheduler invocation exists in transfer selection but encounter lifecycle still controls when/how scheduling applies
- Remove (superseded once canonical flow proven)
  - local dual orchestration branching where UI thread spawns relay encounter directly per send action
  - transport-specific orchestration logic embedded in `main.rs` background gossip loop

### Gap matrix against authoritative contract

- Discovery stage
  - Current: LAN HELLO broadcast drives peer awareness; no BLE/bootstrap abstraction yet
  - Gap: no explicit discovery event normalization layer across bearer types
- Control exchange stage
  - Current: relay and LAN control exchanges exist but are managed in different modules
  - Gap: no single encounter-manager authority for state transitions
- Bulk transfer stage
  - Current: shared transfer selection exists, scheduler-enabled, with debug legacy fallback
  - Gap: transfer bearer transition policy is implicit and transport-loop-local
- Interruption/resume stage
  - Current: short relay rounds and loop retries; limited explicit resume-state model
  - Gap: no canonical upgrade/downgrade + resume state machine surfaced at orchestration boundary

### Desktop/Rust risk inventory (concrete)

- Concurrent threads and timers can initiate overlapping encounters (`thread::spawn` from UI send plus periodic background sync).
- Session ownership is transport-local (relay session lease vs LAN loop map), increasing race risk during cutover.
- Existing debug/fallback flags (`AETHOS_ROUTING_LEGACY_FALLBACK`, shadow-mode logging) can mask canonical behavior during migration.

### Recommended dependency-safe sequence

1. Freeze canonical interface/model mapping (`ac-mbe-02`).
2. Define encounter-manager integration boundary (`ac-mbe-03`).
3. Land discovery/bootstrap and transfer transition plans (`ac-mbe-04`, `ac-mbe-05`).
4. Require telemetry and fixture/test plans before any cleanup (`ac-mbe-06` to `ac-mbe-09`).
