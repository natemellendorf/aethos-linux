# GUI + Network E2E Harness

This harness extends the Tauri desktop E2E flow into an agent-operable multi-node testbed.

## Architecture

- `docker-compose.e2e.yml`
  - `relay-1` (mock relay service for deterministic local runs)
  - `toxiproxy` (relay path fault injection)
  - `e2e-runner` (Xvfb + WebDriver execution lane)
- `spikes/tauri-desktop/e2e/test/dual-instance.mjs`
  - launches two isolated desktop app instances
  - drives real UI interactions via WebDriver + `tauri-driver`
  - writes machine-readable artifacts on pass/fail
- `tests/e2e-harness/config/scenarios/*.json`
  - named fault scenarios (`clean`, `slow-relay`, `relay-partition`, `lossy-peer`, `split-brain-window`, `reconnect-storm`)
- `scripts/e2e/*`
  - scenario runner
  - toxiproxy toxic application
  - artifact indexing
  - lightweight triage summary generation

## One-command run

```bash
docker compose -f docker-compose.e2e.yml up --build --abort-on-container-exit --exit-code-from e2e-runner
```

Optional overrides:

```bash
AETHOS_E2E_SCENARIO=slow-relay AETHOS_E2E_MODE=mixed docker compose -f docker-compose.e2e.yml up --abort-on-container-exit --exit-code-from e2e-runner
```

Modes:
- `relay` (relay-focused)
- `peer` (loopback peer-focused)
- `mixed` (relay + peer)

## Artifacts

Artifacts are written under:

- `tests/e2e-harness/artifacts/<run-id>/`

Primary files:
- `run-index.json` (topology + run metadata)
- `run-result.json` (pass metadata)
- `failure-summary.json` (failure metadata + log tails)
- `artifact-index.json` (artifact manifest)
- `triage-summary.json` (agent-oriented anomaly hints)
- `wayfarer-1-failure.png`, `wayfarer-2-failure.png` (on failure)

## Scenario config

Add new scenario JSON at `tests/e2e-harness/config/scenarios/<name>.json`.

Fields:
- `relay.toxiproxy.enabled`
- `relay.toxiproxy.toxics[]`
- `peer.netem.enabled`
- `peer.netem.commands[]`

## Agent triage workflow

1. Run scenario via `scripts/e2e/run-scenario.sh --scenario <name> --mode <mode>`
2. Read `artifact-index.json`
3. Read `failure-summary.json` (if present)
4. Read `triage-summary.json` for quick causal hints
5. Re-run same scenario/run-id config after patch

## Notes

- This bead is Linux-first and optimized for deterministic automation + artifacts.
- WebDriver lane uses `tauri-driver` and `WebKitWebDriver`.
- Current relay container is mock relay scaffolding to keep local execution deterministic.
