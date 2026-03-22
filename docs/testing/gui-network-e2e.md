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
- `spikes/tauri-desktop/e2e/scripts/generate-aethos-large-png.mjs`
  - canonical deterministic large-media generator used by harness scenarios
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

Non-compose local runner:

```bash
# Peer LAN-only baseline (relay disabled)
bash scripts/e2e/run-scenario.sh --scenario clean --mode peer

# 100-message LAN burst (1 message/second), expect eventual full convergence
AETHOS_E2E_BURST_COUNT=100 \
AETHOS_E2E_BURST_INTERVAL_MS=1000 \
AETHOS_E2E_BURST_RECEIVE_TIMEOUT_MS=900000 \
bash scripts/e2e/run-scenario.sh --scenario clean --mode peer

# Relay scenarios require toxiproxy + relay endpoint wiring
AETHOS_E2E_TOXIPROXY_URL=http://127.0.0.1:8474 \
AETHOS_E2E_RELAY_ENDPOINT=http://127.0.0.1:19082 \
bash scripts/e2e/run-scenario.sh --scenario relay-partition --mode relay
```

Direct desktop E2E run (without compose):

```bash
# one-time
cargo install tauri-driver --locked
npm --prefix spikes/tauri-desktop/e2e install

# each run (auto-preflight for WebKitWebDriver on Linux apt-based systems)
npm --prefix spikes/tauri-desktop run e2e
```

Preflight behavior for `npm --prefix spikes/tauri-desktop run e2e`:
- sets `AETHOS_E2E=1` and runs an E2E-only preflight before tests
- ensures `tauri-driver` is installed (via `cargo install tauri-driver --locked` when missing)
- if `WebKitWebDriver` is missing on Linux + apt-get systems, installs `webkit2gtk-driver`
- opt out of tauri-driver auto-install with `AETHOS_E2E_AUTO_INSTALL_TAURI_DRIVER=0`
- opt out of auto-install with `AETHOS_E2E_AUTO_INSTALL_WEBKIT_DRIVER=0`

Mode behavior (deterministic env mapping):
- `peer`: relay disabled, loopback-only LAN gossip enabled
- `relay`: relay enabled, LAN loopback-only optimization disabled
- `mixed`: relay enabled + loopback LAN gossip enabled

Runner writes local runtime state and logs under:
- `spikes/tauri-desktop/e2e/workdir/<run-id>/`

These directories are intentionally gitignored to avoid permission and artifact churn in normal development.

Burst controls:
- `AETHOS_E2E_BURST_COUNT` (default `1`)
- `AETHOS_E2E_BURST_INTERVAL_MS` (default `1000`)
- `AETHOS_E2E_BURST_RECEIVE_TIMEOUT_MS` (default `600000`)

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

Convergence assertion:
- Baseline message convergence uses persisted state (`chat-history.json`) as source-of-truth.
- UI thread rendering checks are retained as supporting diagnostics, not sole pass/fail criteria.

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
- The current desktop E2E lane does **not** use Playwright test APIs; it uses Selenium WebDriver + `tauri-driver` against Tauri `wry` (WebKit on Linux), which is why `WebKitWebDriver` is required.
- WebDriver lane uses `tauri-driver` and `WebKitWebDriver`.
- Current relay container is mock relay scaffolding to keep local execution deterministic.
- `generate_e2e_large_image` (Rust Tauri command) remains available as a backend-side alternative utility; harness automation uses the Node generator above as source-of-truth.
