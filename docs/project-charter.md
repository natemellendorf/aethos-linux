# Aethos Linux Client Charter (MVP 0 -> MVP 1)

## Purpose

Create a native Linux GUI client that implements the Aethos protocol natively (without Swift),
starting with Ubuntu/Debian and extending to broader targets later.

## Principles

1. **Native implementation**: Linux-specific library and app logic should not depend on Swift runtime.
2. **Protocol fidelity**: Keep behavior aligned with Aethos protocol and relay contract.
3. **Transport abstraction**: Decouple relay transport from local radio transport.
4. **Incremental delivery**: Land small, testable milestones.

## MVP 0 Scope (this repository state)

- Rust GTK GUI scaffold for Linux desktop.
- Wayfair ID generation in-client (UUID v4 placeholder).
- Relay connectivity checks over WebSocket, derived from configured HTTP relay addresses.
- Runtime-editable relay endpoint configuration (IP/host and port).
- UI status updates for each relay connection attempt.
- Documentation for next milestones.

## MVP 0 Exit Criteria

- [x] App launches on Linux desktop with GTK.
- [x] User can generate a Wayfair ID from the GUI.
- [x] App attempts relay WebSocket connections to both configured endpoints.
- [x] App surfaces connection status in the GUI for each relay.
- [x] Project roadmap is documented.

## Roadmap

### Milestone 1a: MVP1 module split (in progress)

- [x] Create `aethos_core::protocol` for hello envelope schema + serialization.
- [x] Create `relay::client` for endpoint normalization and relay probe transport logic.
- [x] Wire GUI to use module APIs instead of inline transport/protocol code.
- [x] Add initial cockpit/glass visual direction pass for Linux desktop UI.


### Milestone 1: Core protocol crate

- Create `aethos-core` for message models and serialization.
- Add protocol versioning and capability negotiation primitives.
- Implement deterministic encoding/decoding tests for protocol payloads.

### Milestone 2: Relay client

- [x] Build resilient relay session manager:
  - [x] Reconnect with backoff.
  - [x] Relay failover and health scoring.
  - [x] Auth/session token support when contract requires it.
- [x] Implement request/response dispatcher and correlation IDs.

### Milestone 3: Local identity and storage

- Promote Wayfair ID to durable identity lifecycle:
  - [x] Key generation + secure storage.
  - [~] Device profile and peer metadata (device profile landed; peer metadata pending).
- [~] Add encrypted local store for session and peer cache (session cache landed; peer cache pending).

### Milestone 4: GUI experience

- [~] Add onboarding flow and identity provisioning UX (initial onboarding steps + identity rotation UX landed).
- [~] Add relay status dashboard and diagnostics (tabbed dashboard + diagnostics timeline landed).
- [~] Add conversation/session views for Aethos message exchange (session list scaffold landed; message exchange pending).

### Milestone 5: Local radio transports

- [~] Milestone 5 kickoff review and implementation sequence documented (`docs/milestone-5-kickoff-review.md`).
- [ ] Abstract transport interfaces for Wi-Fi/Bluetooth/Cellular pathways.
- [ ] Integrate Linux-compatible transport plugins.
- [ ] Route peer communication direct-first, relay fallback second.

### Milestone 6: Packaging and distribution

- Debian packaging (`.deb`) and reproducible build pipeline.
- System integration (desktop files, app icon, auto-start options).
- Security hardening review and release checklist.

## Risks and mitigations

- **Spec drift risk**: Track upstream protocol changes with version gates.
- **Transport complexity**: Keep clear adapter boundaries and conformance tests.
- **Desktop dependency variance**: Document Ubuntu/Debian package prerequisites.
