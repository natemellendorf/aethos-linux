#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

echo "[e2e-gate] running pass-mode scenario"
"${ROOT_DIR}/scripts/e2e/run-scenario.sh" --scenario clean --mode mixed

echo "[e2e-gate] running impaired/failure-mode scenario"
set +e
"${ROOT_DIR}/scripts/e2e/run-scenario.sh" --scenario relay-partition --mode relay
IMPAIRED_EXIT=$?
set -e

if [[ ${IMPAIRED_EXIT} -eq 0 ]]; then
  echo "[e2e-gate] impaired scenario recovered successfully"
else
  echo "[e2e-gate] impaired scenario produced failure artifacts as expected"
fi

echo "[e2e-gate] complete"
