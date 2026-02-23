# aethos-linux

Native Linux GUI client scaffold for Aethos.

## MVP 0 goals

- Build all Linux work in this repository.
- Provide a native GUI scaffold for Ubuntu/Debian.
- Generate a local Wayfair ID.
- Communicate with Aethos Relays listening at:
  - `http://192.168.1.200:8082`
  - `http://192.168.1.200:9082`
- Connect over WebSocket to those endpoints (`ws://...` derived from `http://...`).

## Current implementation

- GTK4 desktop UI (Rust).
- Button to generate a Wayfair ID (UUID v4 placeholder for MVP0).
- Editable relay endpoint fields so IP/host values are configurable at runtime.
- Relay status panel showing per-endpoint connection state.
- Relay probe sends a minimal hello envelope after WebSocket connection.

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

The default `192.168.1.200` endpoints are only placeholders. In this environment, point the GUI
relay fields at locally reachable relay instances (for example localhost).

Typical local loop:

```bash
# Terminal 1: run relay (see aethos-relay README for exact setup)
# e.g. a local relay listening on 8082/9082

# Terminal 2: run Linux client
cargo run
```

Then in the GUI, set relay endpoints to your local relay listeners, for example:

- `http://127.0.0.1:8082`
- `http://127.0.0.1:9082`

## Next

Protocol-compliant message framing, relay contract implementation, identity/key material,
and peer transport logic are planned in `docs/project-charter.md`.
