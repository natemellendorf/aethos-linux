# Aethos Linux Client Charter (MVP 0 -> MVP 1)

## Purpose

Create a native Linux GUI client that implements the Aethos protocol natively (without Swift),
starting with Ubuntu/Debian and extending to broader desktop/mobile targets later.

## Principles

1. **Native implementation**: Linux-specific library and app logic should not depend on Swift runtime.
2. **Protocol fidelity**: Keep behavior aligned with Aethos protocol and relay contract.
3. **Transport abstraction**: Decouple relay transport from local radio transport.
4. **Incremental delivery**: Land small, testable milestones.

## MVP 0 Scope (this repository state)

- Rust GTK GUI scaffold for Linux desktop.
- Wayfair ID generation in-client.
- Relay connectivity checks over WebSocket to:
  - `ws://192.168.1.200:8082`
  - `ws://192.168.1.200:9082`
- Documentation for next milestones.

## Roadmap

### Milestone 1: Core protocol crate

- Create `aethos-core` module for message models and serialization.
- Add protocol versioning and capability negotiation primitives.
- Implement deterministic encoding/decoding tests for protocol payloads.

### Milestone 2: Relay client

- Build resilient relay session manager:
  - Reconnect with backoff.
  - Relay failover and health scoring.
  - Auth/session token support when contract requires it.
- Implement request/response dispatcher and correlation IDs.

### Milestone 3: Local identity and storage

- Promote Wayfair ID generation to durable identity lifecycle:
  - Key generation + secure storage.
  - Device profile and peer metadata.
- Add encrypted local store for session and peer cache.

### Milestone 4: GUI experience

- Add onboarding flow and identity provisioning UX.
- Add relay status dashboard and diagnostics.
- Add conversation/session views for Aethos message exchange.

### Milestone 5: Local radio transports

- Abstract transport interfaces for Wi-Fi/Bluetooth/Cellular pathways.
- Integrate Linux-compatible transport plugins.
- Route peer communication direct-first, relay fallback second.

### Milestone 6: Packaging and distribution

- Debian packaging (`.deb`) and reproducible build pipeline.
- System integration (desktop files, app icon, auto-start options).
- Security hardening review and release checklist.

## Engineering checklist

- [ ] Finalize protocol data model from upstream specs.
- [ ] Finalize relay contract compatibility matrix.
- [ ] Add unit + integration tests for protocol and relay client.
- [ ] Add CI checks: `fmt`, `clippy`, tests, build.
- [ ] Add observability: structured logs + optional tracing.

## Risks and mitigations

- **Spec drift risk**: Track upstream protocol changes with version gates.
- **Transport complexity**: Keep clear adapter boundaries and conformance tests.
- **Desktop dependency variance**: Document Ubuntu/Debian package prerequisites.
