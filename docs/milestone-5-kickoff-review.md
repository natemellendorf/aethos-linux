# Milestone 5 Kickoff Review: Local Radio Transports

## Goal alignment

Milestone 5 in the charter defines three deliverables:

1. Abstract transport interfaces for Wi-Fi/Bluetooth/Cellular pathways.
2. Integrate Linux-compatible transport plugins.
3. Route peer communication direct-first, relay fallback second.

This review assesses the current codebase against those deliverables and proposes an implementation sequence that keeps protocol compatibility while preserving the current GUI + relay diagnostics workflow.

## Current state summary

### What is already in place

- Relay transport logic is already modularized in `relay::client` with endpoint normalization, relay selection, health scoring, reconnect backoff, and request/response correlation support.
- Identity lifecycle and secure local storage are implemented in `aethos_core::identity_store`, including durable identity material and encrypted relay session cache.
- GUI milestones through Milestone 4 are present in `main.rs`, including onboarding, relay diagnostics, and sessions scaffold.

### What is missing for Milestone 5

- No local transport abstraction layer exists yet (there is no `Transport` trait or equivalent interface for non-relay paths).
- No Linux local transport adapters (Wi-Fi LAN discovery/direct socket, Bluetooth, cellular-aware path selection) are implemented.
- Session view is scaffold-only; there is no peer send path that can choose direct transport before relay fallback.

## Proposed architecture for Milestone 5

### 1) Introduce transport abstraction in `aethos_core`

Add a new module (proposed: `src/aethos_core/transport.rs`) with:

- `TransportKind` enum (`Relay`, `WifiLan`, `Bluetooth`, `Cellular`).
- `PeerAddress` and `TransportEndpoint` data models.
- `OutboundMessage` / `InboundMessage` wrappers referencing protocol envelope payloads.
- `trait PeerTransport` with core operations:
  - `kind()`
  - `is_available()`
  - `send()`
  - `receive_poll()` (or callback registration)
  - optional `score_path()` for route preference heuristics.

This keeps Milestone 5 work protocol-first and transport-agnostic.

### 2) Add transport orchestrator

Add a coordinator module (proposed: `src/transport/orchestrator.rs`) that:

- Maintains active transport adapters.
- Performs direct-first route selection using transport availability + score.
- Falls back to relay adapter when direct transports fail or are unavailable.
- Emits diagnostics events for GUI timeline integration.

Initial route policy:

- Prefer `WifiLan` when peer endpoint is known + reachable.
- Then `Bluetooth` (short-range fallback).
- Then `Cellular` if supported and policy allows.
- Finally fallback to relay path.

### 3) Define Linux-first plugin boundary

Use an adapter pattern with feature-gated modules:

- `src/transport/plugins/wifi_lan.rs`
- `src/transport/plugins/bluetooth.rs`
- `src/transport/plugins/cellular.rs`
- `src/transport/plugins/relay_fallback.rs`

Start with a minimal, testable `wifi_lan` implementation (loopback/dev-LAN capable), while `bluetooth` and `cellular` can initially return structured `NotAvailable` status until concrete Linux integration is added.

### 4) Wire GUI sessions scaffold to transport send path

Upgrade Milestone 4 session scaffold so a session action can:

- Build an outbound protocol envelope.
- Ask orchestrator for route.
- Report route decision + outcome to diagnostics timeline.

This creates an end-to-end vertical slice for Milestone 5 without requiring full chat UX completion.

## Incremental delivery plan

1. **M5.1 (interfaces):** Add transport trait/models + unit tests for route scoring.
2. **M5.2 (orchestrator):** Add direct-first + relay fallback selection with deterministic tests.
3. **M5.3 (wifi plugin):** Add Linux LAN direct transport proof-of-path (local/dev).
4. **M5.4 (GUI integration):** Hook session action into orchestrator + diagnostics output.
5. **M5.5 (plugin expansion):** Add Bluetooth/cellular adapters behind capability checks.

## Risks and mitigations

- **Linux stack variance (Bluetooth/cellular tooling):** isolate platform specifics behind plugin interfaces and feature flags.
- **Route flapping and unstable direct paths:** keep relay fallback always available and persist short-lived route health signals.
- **Protocol drift while adding transports:** keep all transport adapters envelope-based so protocol encoding remains centralized.

## Recommendation

Begin with M5.1 + M5.2 in the next implementation PR so Milestone 5 has a stable abstraction and route engine before platform-specific adapters.
