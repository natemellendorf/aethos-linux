#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DESKTOP_DIR="${ROOT_DIR}/spikes/tauri-desktop"
SCENARIO="clean"
MODE="peer"
RUN_ID="run-$(date +%s)-$RANDOM"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --scenario)
      SCENARIO="$2"
      shift 2
      ;;
    --mode)
      MODE="$2"
      shift 2
      ;;
    --run-id)
      RUN_ID="$2"
      shift 2
      ;;
    *)
      echo "unknown arg: $1" >&2
      exit 1
      ;;
  esac
done

ARTIFACT_DIR="${ROOT_DIR}/tests/e2e-harness/artifacts/${RUN_ID}"
mkdir -p "${ARTIFACT_DIR}"

export AETHOS_E2E_RUN_ID="${RUN_ID}"
export AETHOS_E2E_TEST_CASE_ID="${MODE}-${SCENARIO}"
export AETHOS_E2E_SCENARIO="${SCENARIO}"
export AETHOS_E2E_ARTIFACT_DIR="${ARTIFACT_DIR}"

if [[ -n "${AETHOS_E2E_RELAY_ENDPOINT:-}" ]]; then
  export AETHOS_E2E_RELAY_ENDPOINT
fi

if [[ "${MODE}" == "relay" ]]; then
  export AETHOS_E2E_DISABLE_GOSSIP=1
  export AETHOS_E2E_DISABLE_LAN_TCP=1
elif [[ "${MODE}" == "peer" ]]; then
  export AETHOS_E2E_DISABLE_RELAY=1
  export AETHOS_E2E_LOOPBACK_ONLY=1
elif [[ "${MODE}" == "mixed" ]]; then
  export AETHOS_E2E_LOOPBACK_ONLY=1
fi

if [[ "${SCENARIO}" == "clean" ]]; then
  true
else
  python3 "${ROOT_DIR}/scripts/e2e/toxiproxy_apply.py" \
    --scenario-file "${ROOT_DIR}/tests/e2e-harness/config/scenarios/${SCENARIO}.json" \
    --toxiproxy-url "${AETHOS_E2E_TOXIPROXY_URL:-http://127.0.0.1:8474}" || true
fi

set +e
cd "${DESKTOP_DIR}"
npm run e2e
EXIT_CODE=$?
set -e

python3 "${ROOT_DIR}/scripts/e2e/index_artifacts.py" \
  --artifact-dir "${ARTIFACT_DIR}" \
  --run-id "${RUN_ID}" \
  --scenario "${SCENARIO}" \
  --mode "${MODE}" \
  --exit-code "${EXIT_CODE}"

python3 "${ROOT_DIR}/scripts/e2e/summarize_logs.py" \
  --artifact-dir "${ARTIFACT_DIR}" || true

exit "${EXIT_CODE}"
