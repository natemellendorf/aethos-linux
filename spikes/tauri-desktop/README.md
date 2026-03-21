# Tauri Desktop Rewrite Track

This directory is the active Tauri rewrite track for the desktop client.

The GTK implementation remains in `src/main.rs` during migration, but this path is now the target for cross-platform GUI delivery.

## Goals

- Ship standalone desktop bundles for Linux/macOS/Windows.
- Preserve existing chat/contact/settings workflows from GTK while improving UX.
- Keep protocol, relay, and identity logic in Rust.

## Current Scope

- React + Tailwind + shadcn/ui-style component system.
- Multi-view UI shell: Chats, Contacts, Share, Settings.
- Real identity, contacts, settings, and chat persistence wired to backend commands.
- Relay diagnostics + inbox sync commands with non-blocking backend execution.
- QR share generation + contact QR import flow.
- Outbound status transitions (sending/sent/failed), retry action, and metadata details.
- New-contact and unread badges in contact list.

## Run (Dev)

From this directory:

```bash
npm install
npm run tauri:dev
```

Frontend-only preview:

```bash
npm run dev
```

## Build (Desktop Bundle)

```bash
npm run tauri:build
```

## Tests

Run backend tests:

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

Optional relay target E2E tests (ignored by default):

```bash
AETHOS_RELAY_TEST_HTTP=http://your-relay:8082 cargo test --manifest-path src-tauri/Cargo.toml -- --ignored
```

Desktop two-instance WebDriver E2E (no human loop):

```bash
# Linux only for now (tauri-driver + WebKitWebDriver)
cargo install tauri-driver --locked
# Debian/Ubuntu example:
sudo apt install webkit2gtk-driver

cd spikes/tauri-desktop/e2e && npm install
cd .. && npm run e2e
```

What this validates:
- launches two isolated desktop app instances (separate `AETHOS_STATE_DIR`)
- exchanges contacts through UI automation
- sends message from instance A to instance B
- drives LAN gossip announce loop until convergence or timeout
- asserts each instance writes to its own local app log file
- includes log tails in test failure output for fast diagnosis

## macOS signing/notarization (CI)

Unsigned macOS artifacts can show "app is damaged" on first launch.

To produce a signed + notarized `.dmg` from GitHub Actions, add these repository secrets:

- `APPLE_CERTIFICATE` (base64-encoded `.p12` certificate)
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_SIGNING_IDENTITY` (for example: `Developer ID Application: Your Name (TEAMID)`)
- `APPLE_ID` (Apple account email)
- `APPLE_PASSWORD` (app-specific password)
- `APPLE_TEAM_ID`

Then run workflow `Release Binaries` with `notarize_macos=true`.

CLI example:

```bash
gh workflow run release-binaries.yml --ref spike/tauri-bundles -f notarize_macos=true
```

## Release readiness

Use `docs/tauri-cutover-checklist.md` before promoting this path to default desktop release.
