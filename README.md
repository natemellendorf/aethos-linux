# aethos-linux

Native Linux GUI client scaffold for Aethos.

## MVP 0 goals

- Build all Linux work in this repository.
- Provide a native GUI scaffold for Ubuntu/Debian.
- Generate a local Wayfair ID.
- Connect over WebSocket to relay endpoints:
  - `ws://192.168.1.200:8082`
  - `ws://192.168.1.200:9082`

## Run

```bash
cargo run
```

## Notes

This is intentionally a minimal MVP scaffold. Protocol-compliant framing, session handling,
relay contract integration, and peer-to-peer transport orchestration are tracked in
`docs/project-charter.md`.
