<p align="center">
  <img src="https://raw.githubusercontent.com/natemellendorf/aethos/refs/heads/main/docs/img/banner.jpg" alt="Aethos banner" width="960">
</p>

# aethos-client

Cross-platform desktop client for Aethos (Linux, macOS, Windows).

## Install

### Linux/macOS (shell)

```bash
curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-client/main/scripts/install.sh | bash
```

### Windows (PowerShell)

```powershell
powershell -ExecutionPolicy Bypass -Command "$tmp = Join-Path $env:TEMP 'install-aethos.ps1'; iwr https://raw.githubusercontent.com/natemellendorf/aethos-client/main/scripts/install.ps1 -UseBasicParsing -OutFile $tmp; & $tmp"
```

This installer will:

- download the selected release artifact from GitHub for the local OS/arch
- install the binary to your local bin directory as `aethos` (with compatibility alias `aethos-linux`)
- on macOS, validate Homebrew + `gtk4` runtime availability (and fail with install guidance if missing)

By default, if `--ref` is not provided, installers resolve the latest official GitHub release tag and download a prebuilt binary artifact for the local OS/arch.

Optional flags/environment:

```bash
# Install from a specific tag
curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-client/main/scripts/install.sh | bash -s -- --ref v0.2.0
```

```bash
# Custom binary directory
curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-client/main/scripts/install.sh | bash -s -- --bin-dir "$HOME/bin"
```

```powershell
# Windows specific tag install
powershell -ExecutionPolicy Bypass -Command "$tmp = Join-Path $env:TEMP 'install-aethos.ps1'; iwr https://raw.githubusercontent.com/natemellendorf/aethos-client/main/scripts/install.ps1 -UseBasicParsing -OutFile $tmp; & $tmp -Ref v0.2.0"
```

## MVP 0 goals

- Build all desktop client work in this repository.
- Provide a native GUI scaffold for Linux/macOS/Windows.
- Generate a local Wayfarer ID.
- Communicate with the default Aethos relay endpoint:
  - `wss://aethos-relay.network`
- Connect over secure WebSocket by default (`wss://`).

## MVP 1 progress

- Introduced `aethos_core` module for protocol message scaffolding.
- Introduced `relay` module for endpoint normalization, `/ws` WS derivation, and relay transport probing.
- Hello frame now follows `CLIENT_RELAY_PROTOCOL_V1` with `type`, `wayfarer_id`, and `device_id`.
- Wayfarer ID now persists to native local user data storage (`$XDG_DATA_HOME` or `~/.local/share` on Linux).
- UI includes explicit delete action and warning about key backup/data loss implications.
- Applied first-pass cockpit/glass visual theme (blue/purple gradients + glass panel styling).

## Milestone 2 progress

- Added relay session manager primitives with reconnect backoff, failover scheduling, and relay health scoring.
- Added per-relay auth/session token plumbing for WS handshake authorization headers.
- Added request/response dispatcher primitives with correlation IDs and pending request tracking.
- Wired the GUI relay check path through session manager selection + dispatcher correlation logging so Milestone 2 primitives are exercised at runtime.


## Milestone 3 progress

- Promoted identity persistence to include a generated Ed25519 signing key stored alongside Wayfarer ID metadata with secure file permissions on Linux (0600).
- Added device profile persistence (device name + platform) as part of local identity lifecycle records.
- Added encrypted local relay session cache storage using ChaCha20-Poly1305 at rest, keyed from local identity material.

## Milestone 4 progress

- Added a tabbed GUI flow with dedicated Onboarding, Relay Dashboard, and Sessions views.
- Added onboarding progression UX that surfaces identity provisioning state and allows explicit transition into diagnostics.
- Added relay diagnostics timeline view to keep a running log of per-relay probe outcomes and dispatcher metadata.
- Added conversation/session list scaffold to start Milestone 4 session-view groundwork for later message-exchange integration.

## Protocol migration progress

- Added v1 client frame models for `send`, `pull`, and `ack` with input validation.
- Added relay frame parser support for `hello_ok`, `send_ok`, `message`, `messages`, `ack_ok`, and `error`.
- Added relay client helper APIs to execute v1 `send`/`pull`/`ack` exchanges after `hello`/`hello_ok` handshake.
- Added Sessions view controls to trigger v1 `send`, `pull`, and `ack` calls against the configured relay and log outcomes.
- Added a mock-relay integration-style test that exercises `send` -> `pull` -> `ack` over real local WebSocket sessions.
- Added a local EnvelopeV1 composer utility so plaintext body + recipient can be encoded into canonical `payload_b64` before send.
- Added EnvelopeV1 decode helpers so pulled `payload_b64` can be inspected as UTF-8 preview (or binary metadata fallback).
- Performed a quick Sessions UI cleanup pass by grouping compose and relay operations into clearer sections.
- Updated the Linux UI theme toward the iOS cosmic look (dark navy cards, cyan action buttons, violet accents).
- Sessions view now presents desktop chat UX with contacts, thread pane, and bubble-style incoming/outgoing messages grouped by contact wayfarer ID.
- App now opens directly to Chats for chat-first behavior.
- Wayfarer identity is auto-provisioned on first launch and reset now requires an explicit destructive confirmation prompt.
- Advanced send options were removed from the primary composer; sending now uses recipient + text body with automatic EnvelopeV1 payload composition.
- Relay end-user send flow is now chat-only (recipient + text), with relay protocol controls removed from the primary UI to reduce misconfiguration risk.
- Added chat interaction polish: auto-scroll to newest message, formatted message timestamps, and subtle visual pulse feedback on send/receive.
- Added responsive compact behavior: on narrower windows, contacts collapse into a top contact picker and return to sidebar mode on wider layouts.
- Added animated adaptive transitions between sidebar and compact contact-picker modes for smoother, mobile-like resizing behavior.
- Added local contact naming: users can assign friendly names to wayfarer IDs, and aliases persist locally on that Linux device.
- Added a dedicated `Contacts` tab for add/update/remove contact management; `Chats` is now message-only and uses selected managed contacts.
- Added local chat-history persistence so sent/received thread messages survive app restart.
- Added a `Share` tab with QR generation for Wayfarer ID and centered feather mark overlay.
- Added a `Save QR Image` action in Share so users can export their QR code for out-of-band contact exchange.
- Added QR contact import from image files in `Contacts` so Wayfarer IDs can be scanned instead of typed manually.
- Added quick one-click contact add from active chat thread header.
- Added Gossip Sync v1 frame models and validation for `inventory_summary`, `missing_request`, `transfer`, and `receipt`.
- Added local gossip inventory persistence for canonical `EnvelopeV1` payload metadata and deduped `item_id` storage.
- Added direct LAN gossip sync over UDP broadcast (`47655`) so Linux clients can exchange pending Aethos envelopes without a relay path.
- Wired relay send/pull flows into the gossip inventory store so relay and direct sync pathways converge into one local message inventory.

## Identity persistence

Wayfarer IDs are stored on disk so they survive app restarts:

- Preferred path: `$XDG_DATA_HOME/aethos-linux/identity.json`
- Fallback path: `~/.local/share/aethos-linux/identity.json`

These paths currently keep the `aethos-linux` directory name for backward compatibility.

Deleting the Wayfarer ID removes this local identity file. This is effectively like changing your email address; if users do not back up their keypair, they can lose access to data addressed to the old identity.

## Project layout

```text
src/
  aethos_core/
    mod.rs
    protocol.rs
  relay/
    mod.rs
    client.rs
  main.rs
```

## Prerequisites (source build)

If building from source, install Rust and platform GTK4 development/runtime dependencies.

Ubuntu/Debian example:

```bash
sudo apt update
sudo apt install -y \
  build-essential \
  pkg-config \
  libgtk-4-dev \
  libglib2.0-dev \
  curl

curl https://sh.rustup.rs -sSf | sh -s -- -y
source "$HOME/.cargo/env"
```

## Build, run, and test

From this repository root:

```bash
# Compile
cargo build

# Run the GUI app
cargo run --bin aethos

# Run installed binary
aethos

# Run tests
cargo test

# Formatting / lint checks
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

## Local dev lifecycle script

Use `scripts/dev.sh` to manage local development lifecycle (start/stop/status/logs):

```bash
# Start app (builds by default)
scripts/dev.sh start

# Show current process state
scripts/dev.sh status

# Stop all managed processes
scripts/dev.sh stop

# Follow app logs
scripts/dev.sh logs --service app
```

Optional relay process management (if you want script-managed relays too):

```bash
AETHOS_RELAY_PRIMARY_CMD="cargo run --manifest-path ../aethos-relay/Cargo.toml" \
AETHOS_RELAY_SECONDARY_CMD="cargo run --manifest-path ../aethos-relay/Cargo.toml -- --port 9082" \
scripts/dev.sh start
```

## Git hooks + release flow

Install repository-managed hooks:

```bash
scripts/setup-git-hooks.sh
```

Hook behavior:

- `pre-commit`: runs full lint gate (`cargo fmt --all -- --check` and `cargo clippy --all-targets --all-features -- -D warnings`)
- `pre-merge-commit`: when merge includes local `main` tip, runs `cargo test`
- `pre-push`: when pushing `origin/main`, creates a GitHub prerelease only when explicitly requested

Prerelease + release scripts:

```bash
# Create prerelease manually (also used by pre-push hook)
scripts/release/create-prerelease.sh

# Create prerelease and explicitly queue prerelease binary artifacts
AETHOS_BUILD_PRERELEASE_BINARIES=1 scripts/release/create-prerelease.sh

# Request prerelease during push (opt-in)
AETHOS_CREATE_PRERELEASE=1 git push origin main
# or
git push -o prerelease origin main

# Dry-run next official version inference
scripts/release/create-release.sh --dry-run

# Cut official release (updates Cargo.toml version, tests, commit, tag, GitHub release)
scripts/release/create-release.sh

# Promote an existing prerelease to official release tag
# (example: v1.2.3-pre.2026... -> v1.2.3)
scripts/release/create-release.sh --from-prerelease <tag>
```

Binary artifact workflow (`.github/workflows/release-binaries.yml`):

- Official release (`release.published` with `prerelease=false`): builds and uploads Linux/macOS/Windows artifacts automatically.
- Prerelease: skipped by default.
- Explicit prerelease binary build: run workflow manually with `workflow_dispatch` and `build_prerelease=true`, or set `AETHOS_BUILD_PRERELEASE_BINARIES=1` when running `create-prerelease.sh`.
- Output assets are attached to the release as:
  - `aethos-<tag>-x86_64-unknown-linux-gnu.tar.gz`
  - `aethos-<tag>-<macos-target>.tar.gz`
  - `aethos-<tag>-x86_64-pc-windows-gnu.zip`
  - `aethos-<tag>-sha256sums.txt`

The app footer displays the build identifier as `v<version> (build <git-sha>)` for troubleshooting.

Versioning strategy:

- Conventional-commit inspired bumping from commits since last `v*` tag:
  - `major`: commit subject with `!` (e.g. `feat!:`) or `BREAKING CHANGE:` in body
  - `minor`: `feat:` commits
  - `patch`: all other changes

## Local relay testing

The default endpoint is `wss://aethos-relay.network`.
In local/dev environments, point the GUI relay field to a reachable listener, for example:

- `http://127.0.0.1:8082`

Relay terminals and startup details should follow the `aethos-relay` README setup.

## Local gossip sync (direct peer path)

- Gossip sync runs in the background and uses UDP broadcast on port `47655`.
- The client emits single-page `inventory_summary` frames (MVP0 profile), responds to `missing_request`, and imports `transfer` frames into local chats.
- Use LAN/private network segments only for this mode. Inventory metadata is visible to peers that can receive local gossip traffic.

## GUI + network E2E harness (agent-operable)

We now maintain a Linux-first GUI+network E2E harness that is designed for autonomous debugging workflows:

- Real desktop app automation via `tauri-driver` + WebDriver
- Multi-instance orchestration
- Named fault scenarios (relay and peer path)
- Deterministic run metadata and artifact bundles for machine triage

Primary docs:

- `docs/testing/gui-network-e2e.md`

Quick start:

```bash
# Run default clean scenario in compose harness
docker compose -f docker-compose.e2e.yml up --abort-on-container-exit --exit-code-from e2e-runner

# Run explicit scenario/mode
AETHOS_E2E_SCENARIO=slow-relay AETHOS_E2E_MODE=relay \
docker compose -f docker-compose.e2e.yml up --abort-on-container-exit --exit-code-from e2e-runner
```

Artifacts are written under:

- `tests/e2e-harness/artifacts/<run-id>/`

Pre-merge expectation:

- See `scripts/hooks/pre-merge-commit` and `scripts/e2e/pre-merge-gate.sh`.
- Merge validation now includes both a baseline pass-mode scenario and an impaired/failure-mode scenario with artifact generation.

## Design reference

Art direction notes are tracked in:

- `docs/design/art-style-reference.md`

## Next

Protocol-compliant message framing, relay contract implementation, identity/key material,
and peer transport logic are tracked in `docs/project-charter.md`.
