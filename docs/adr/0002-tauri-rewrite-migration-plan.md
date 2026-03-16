# ADR 0002: Tauri Rewrite Migration Plan

- Status: Accepted
- Date: 2026-03-15
- Deciders: Aethos client maintainers

## Context

ADR 0001 selected Tauri as the direction for cross-platform GUI distribution. Initial spike validation confirmed:

- Linux/macOS/Windows bundle builds in CI,
- successful signed + notarized macOS install,
- improved installer ergonomics compared to GTK runtime prerequisites.

We now need a clear plan to swap the existing GTK client over to the Tauri path without a risky single-cut rewrite.

## Decision

Adopt a phased migration where Tauri becomes the primary desktop implementation, with GTK kept temporarily for parity fallback.

## Plan

1. **Foundation phase** (in progress)
   - Build Tauri shell with tabs and layout parity for Onboarding, Chats, Contacts, Share, Settings.
   - Wire identity/settings/contacts/chat persistence to Rust backend commands.
   - Wire relay diagnostics to backend commands.

2. **Parity phase**
   - Port relay send/pull flows from GTK action handlers into backend commands.
   - Port chat delivery state transitions and polling orchestration.
   - Port QR import/export and sharing workflows.

3. **Default switch phase**
   - Make Tauri build/release path the default desktop pipeline.
   - Keep GTK path as fallback for one release cycle.

4. **Retirement phase**
   - Remove GTK UI shell and GTK runtime/dependency assumptions.
   - Keep shared protocol and relay contracts unchanged.

## Consequences

Positive:

- Reduced distribution friction on macOS/Windows.
- Improved UI iteration velocity with modern frontend tooling.
- Maintains Rust ownership of protocol/security-sensitive behavior.

Trade-offs:

- Temporary duplication while GTK and Tauri coexist.
- Additional migration testing needed for behavior parity.

## Exit criteria for full swap

- All user-facing chat and contact workflows available in Tauri.
- Relay sync workflows functionally equivalent to GTK behavior.
- CI publishes signed/notarized macOS artifacts and installable Linux/Windows artifacts.
- One release cycle completes without fallback regressions.
