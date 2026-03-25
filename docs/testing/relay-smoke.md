# Live relay smoke (non-GUI, opt-in)

This repository includes a non-GUI relay interoperability smoke CLI for macOS/Linux:

- bin: `relay-smoke`
- source: `src/bin/relay-smoke.rs`

It is intentionally **opt-in** and skipped unless `AETHOS_RUN_LIVE_RELAY_SMOKE=1`.

## What it checks

When enabled, it performs a bounded live flow:

1. Connect to relay (`AETHOS_RELAY_ENDPOINT` or default `wss://aethos-relay.network`)
2. Run current client relay handshake (`HELLO` exchange)
3. Create and store one local envelope addressed to `AETHOS_TARGET_WAYFARER_ID` with short TTL
4. Run bounded encounter rounds and mark success when at least one encounter succeeds and local publish/store checks pass (with stronger signal if traced item is requested/receipted)

## Required env

- `AETHOS_RUN_LIVE_RELAY_SMOKE=1`
- `AETHOS_TARGET_WAYFARER_ID=<64 lowercase hex>`

## Optional env

- `AETHOS_RELAY_ENDPOINT` (default `wss://aethos-relay.network`)
- `AETHOS_RELAY_AUTH_TOKEN` (if relay requires auth)
- `AETHOS_RELAY_SMOKE_RUN_ID` (default `run-<unix-ms>`)
- `AETHOS_RELAY_SMOKE_TTL_SECONDS` (default `120`, clamped `45..600`)
- `AETHOS_RELAY_SMOKE_MAX_ROUNDS` (default `3`, clamped `1..10`)
- `AETHOS_RELAY_SMOKE_ENCOUNTER_WINDOW_MS` (default `2000`, clamped `250..20000`)
- `AETHOS_RELAY_SMOKE_VERBOSE=1` (enables verbose relay logging)

## Run

```bash
# skip mode (default, exits success with status=skipped)
cargo run --bin relay-smoke

# live mode
AETHOS_RUN_LIVE_RELAY_SMOKE=1 \
AETHOS_TARGET_WAYFARER_ID=<target-wayfarer-id> \
cargo run --bin relay-smoke
```

## Artifacts

Outputs are written to deterministic path:

- `tests/artifacts/relay-smoke/<run-id>/`

Files:

- `run.log`
- `summary.json`
- `artifact-index.json`

The CLI always prints the artifact directory path on exit.
