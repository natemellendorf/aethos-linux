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

## Run

```bash
cargo run
```

## Local relay testing

The default `192.168.1.200` endpoints are only placeholders. In this environment, point the GUI
relay fields at locally reachable relay instances (for example localhost) and run a relay server
locally per `aethos-relay` setup instructions.

## Next

Protocol-compliant message framing, relay contract implementation, identity/key material,
and peer transport logic are planned in `docs/project-charter.md`.
