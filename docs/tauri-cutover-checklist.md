# Tauri Cutover Checklist

Use this checklist before promoting the Tauri desktop app as the default client path.

## Functional Parity

- [ ] Chat send flow works with relay enabled and disabled.
- [ ] Inbox sync pulls new messages and dedupes by `msg_id` / manifest.
- [ ] Delivery receipts reconcile outbound message state to delivered.
- [ ] Contact add/remove/import-QR flows work.
- [ ] Share tab can generate and download QR image.
- [ ] Settings persist correctly across restarts.
- [ ] Relay and gossip status chips update as expected.

## Packaging and Install

- [ ] Linux artifacts (`.AppImage`, `.deb`) build and install on clean machine.
- [ ] macOS artifact is signed + notarized and installs without quarantine bypass.
- [ ] Windows installer launches on clean machine without missing DLL errors.

## CI and Release Pipeline

- [ ] `Tauri Spike Bundles` workflow succeeds on Linux/macOS/Windows.
- [ ] macOS notarization path succeeds with repository secrets.
- [ ] Artifact names and install docs are up to date.

## Observability and Stability

- [ ] Startup and runtime failures surface in UI status text.
- [ ] Logs are discoverable from settings diagnostics context.
- [ ] No UI lockups when saving settings or running diagnostics.

## Cutover Actions

- [ ] Update release docs to point to Tauri install artifacts.
- [ ] Set Tauri workflow as default desktop release path.
- [ ] Keep GTK path as fallback for one release cycle.
- [ ] Open follow-up issue to remove GTK path after fallback window.
