# aethos-linux

Native Linux GUI client scaffold for Aethos.

## MVP 0 goals

- Build all Linux work in this repository.
- Provide a native GUI scaffold for Ubuntu/Debian.
- Generate a local Wayfair ID.
- Communicate with Aethos Relays listening at:
  - `http://192.168.1.200:8082`
  - `http://192.168.1.200:9082`
- Connect over WebSocket to those endpoints using `/ws` (`ws://.../ws` derived from `http://...`).

## MVP 1 progress

- Introduced `aethos_core` module for protocol message scaffolding.
- Introduced `relay` module for endpoint normalization, `/ws` WS derivation, and relay transport probing.
- Hello envelope now includes a Wayfair ID and serializes as JSON before send.
- Wayfair ID now persists to native Linux user data storage (`$XDG_DATA_HOME` or `~/.local/share`).
- UI includes explicit delete action and warning about key backup/data loss implications.
- Applied first-pass cockpit/glass visual theme (blue/purple gradients + glass panel styling).

## Milestone 2 progress

- Added relay session manager primitives with reconnect backoff, failover scheduling, and relay health scoring.
- Added per-relay auth/session token plumbing for WS handshake authorization headers.
- Added request/response dispatcher primitives with correlation IDs and pending request tracking.
- Wired the GUI relay check path through session manager selection + dispatcher correlation logging so Milestone 2 primitives are exercised at runtime.


## Milestone 3 progress

- Promoted identity persistence to include a generated Ed25519 signing key stored alongside Wayfair ID metadata with secure file permissions on Linux (0600).
- Added device profile persistence (device name + platform) as part of local identity lifecycle records.
- Added encrypted local relay session cache storage using ChaCha20-Poly1305 at rest, keyed from local identity material.

## Milestone 4 progress

- Added a tabbed GUI flow with dedicated Onboarding, Relay Dashboard, and Sessions views.
- Added onboarding progression UX that surfaces identity provisioning state and allows explicit transition into diagnostics.
- Added relay diagnostics timeline view to keep a running log of per-relay probe outcomes and dispatcher metadata.
- Added conversation/session list scaffold to start Milestone 4 session-view groundwork for later message-exchange integration.

## Milestone 5 kickoff

- Added charter-aligned Milestone 5 review and implementation sequence in `docs/milestone-5-kickoff-review.md`.
- Captured direct-first transport orchestration recommendations before beginning local radio adapter implementation.

## Identity persistence

Wayfair IDs are stored on disk so they survive app restarts:

- Preferred path: `$XDG_DATA_HOME/aethos-linux/identity.json`
- Fallback path: `~/.local/share/aethos-linux/identity.json`

Deleting the Wayfair ID removes this local identity file. This is effectively like changing your email address; if users do not back up their keypair, they can lose access to data addressed to the old identity.

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

## Design reference

Art direction notes are tracked in:

- `docs/design/art-style-reference.md`

## Next

Protocol-compliant message framing, relay contract implementation, identity/key material,
and peer transport logic are tracked in `docs/project-charter.md`.
