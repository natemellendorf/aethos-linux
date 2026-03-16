# ADR 0001: Cross-platform GUI Direction - Tauri Spike

- Status: Accepted
- Date: 2026-03-15
- Deciders: Aethos client maintainers

## Context

The current desktop client is implemented with Rust + GTK (`gtk4` crate) and builds on Linux, macOS, and Windows. In practice, releases are not fully self-contained:

- macOS requires Homebrew-managed GTK runtime dependencies before launch.
- Windows release artifacts currently ship only `aethos.exe`, which can fail on clean systems due to missing runtime DLLs.

This increases installation friction and support burden, and conflicts with the desired distribution experience: a standalone desktop application that works across Linux/macOS/Windows with minimal downstream setup.

## Decision

We will evaluate a migration path toward a Tauri-based desktop shell while preserving protocol and core logic in Rust.

Specifically:

1. Keep existing GTK client unchanged during evaluation.
2. Create an isolated spike implementation under `spikes/tauri-desktop`.
3. Use the spike to validate:
   - packaging/distribution ergonomics across Linux/macOS/Windows,
   - UI/UX fidelity and implementation velocity,
   - integration model with Rust core APIs.

No immediate replacement of the GTK client is committed by this ADR. A migration decision follows only after spike evaluation.

## Rationale

Tauri provides:

- Strong cross-platform packaging support with smaller bundles than many alternatives.
- High visual fidelity potential through modern web UI tooling.
- Continued Rust-first business logic and protocol implementation.
- A practical path to reduce runtime dependency surprises for end users.

## Consequences

Positive:

- Clear path to improve install and first-run experience.
- Easier access to modern UI/animation patterns for better UX.
- Maintains Rust ownership of security-critical logic.

Trade-offs:

- Team must adopt web frontend tooling patterns.
- Short-term duplication while GTK and spike coexist.
- Additional CI packaging work for Tauri release outputs.

## Evaluation Exit Criteria

The spike is successful if it demonstrates all of the following:

1. Clean builds and runnable app bundles on Linux/macOS/Windows.
2. A representative UI flow (onboarding, contacts, chat shell, settings) with good responsiveness.
3. Basic Rust command invocation and data exchange between UI and backend.
4. A documented packaging path that avoids manual runtime-install guidance for end users.

## Follow-up

- Implement and iterate the isolated spike at `spikes/tauri-desktop`.
- Compare spike outcomes vs. GTK runtime-bundling hardening effort.
- Make go/no-go migration decision in a follow-up ADR.
