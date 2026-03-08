# aethos-linux

Native Linux GUI client scaffold for Aethos.

## MVP 0 goals

- Build all Linux work in this repository.
- Provide a native GUI scaffold for Ubuntu/Debian.
- Generate a local Wayfarer ID.
- Communicate with Aethos Relays listening at:
  - `http://192.168.1.200:8082`
  - `http://192.168.1.200:9082`
- Connect over WebSocket to those endpoints using `/ws` (`ws://.../ws` derived from `http://...`).

## MVP 1 progress

- Introduced `aethos_core` module for protocol message scaffolding.
- Introduced `relay` module for endpoint normalization, `/ws` WS derivation, and relay transport probing.
- Hello frame now follows `CLIENT_RELAY_PROTOCOL_V1` with `type`, `wayfarer_id`, and `device_id`.
- Wayfarer ID now persists to native Linux user data storage (`$XDG_DATA_HOME` or `~/.local/share`).
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
- Added Gossip Sync v1 frame models and validation for `inventory_summary`, `missing_request`, `transfer`, and `receipt`.
- Added local gossip inventory persistence for canonical `EnvelopeV1` payload metadata and deduped `item_id` storage.
- Added direct LAN gossip sync over UDP broadcast (`47655`) so Linux clients can exchange pending Aethos envelopes without a relay path.
- Wired relay send/pull flows into the gossip inventory store so relay and direct sync pathways converge into one local message inventory.

## Identity persistence

Wayfarer IDs are stored on disk so they survive app restarts:

- Preferred path: `$XDG_DATA_HOME/aethos-linux/identity.json`
- Fallback path: `~/.local/share/aethos-linux/identity.json`

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

## Prerequisites (Ubuntu/Debian)

Install Rust and GTK4 development packages:

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

## One-line install (curl | bash)

For a quick workstation install from GitHub:

```bash
curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-linux/main/scripts/install.sh | bash
```

This installer will:

- install required Ubuntu/Debian packages (when `apt-get` is available)
- install Rust via rustup if `cargo` is missing
- download the selected source ref from GitHub
- build `aethos-linux` in release mode
- install the binary to `~/.local/bin/aethos-linux`

Optional flags/environment:

```bash
# Install from a tag
curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-linux/main/scripts/install.sh | bash -s -- --ref v0.1.0

# Skip dependency bootstrap if already provisioned
curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-linux/main/scripts/install.sh | bash -s -- --skip-deps

# Custom binary directory
curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-linux/main/scripts/install.sh | bash -s -- --bin-dir "$HOME/bin"
```

## Build, run, and test

From this repository root:

```bash
# Compile
cargo build

# Run the GUI app
cargo run

# Run tests
cargo test

# Formatting / lint checks
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

## Local relay testing

The default `192.168.1.200` endpoints are placeholders. In local/dev environments,
point the GUI relay fields to reachable listeners, for example:

- `http://127.0.0.1:8082`
- `http://127.0.0.1:9082`

Relay terminals and startup details should follow the `aethos-relay` README setup.

## Local gossip sync (direct peer path)

- Gossip sync runs in the background and uses UDP broadcast on port `47655`.
- The client emits single-page `inventory_summary` frames (MVP0 profile), responds to `missing_request`, and imports `transfer` frames into local chats.
- Use LAN/private network segments only for this mode. Inventory metadata is visible to peers that can receive local gossip traffic.

## Design reference

Art direction notes are tracked in:

- `docs/design/art-style-reference.md`

## Next

Protocol-compliant message framing, relay contract implementation, identity/key material,
and peer transport logic are tracked in `docs/project-charter.md`.
