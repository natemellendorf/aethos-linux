#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME_ROOT="${XDG_RUNTIME_DIR:-/tmp}"
STATE_DIR="${RUNTIME_ROOT}/aethos-linux-dev"
LOG_DIR="${ROOT_DIR}/.dev/logs"
APP_DIR="${ROOT_DIR}/spikes/tauri-desktop"

APP_CMD="cd \"${APP_DIR}\" && npm run tauri:dev"
APP_LOG="${LOG_DIR}/app.log"
APP_PID="${STATE_DIR}/app.pid"

RELAY1_CMD="${AETHOS_RELAY_PRIMARY_CMD:-}"
RELAY2_CMD="${AETHOS_RELAY_SECONDARY_CMD:-}"
RELAY1_LOG="${LOG_DIR}/relay-primary.log"
RELAY2_LOG="${LOG_DIR}/relay-secondary.log"
RELAY1_PID="${STATE_DIR}/relay-primary.pid"
RELAY2_PID="${STATE_DIR}/relay-secondary.pid"

NO_BUILD="0"

usage() {
  cat <<'EOF'
Aethos local development lifecycle script (Tauri desktop)

Usage:
  scripts/dev.sh <command> [options]

Commands:
  start      Build (default) and start local processes
  stop       Stop local processes
  restart    Stop then start local processes
  status     Show process status
  logs       Follow logs (app by default)

Options:
  --no-build            Skip dependency install for start/restart
  --service <name>      logs only: app|relay-primary|relay-secondary
  -h, --help            Show this help

Environment:
  AETHOS_RELAY_PRIMARY_CMD     Optional command to run primary relay
  AETHOS_RELAY_SECONDARY_CMD   Optional command to run secondary relay

Examples:
  scripts/dev.sh start
  scripts/dev.sh status
  scripts/dev.sh logs --service app
  AETHOS_RELAY_PRIMARY_CMD="cargo run --manifest-path ../aethos-relay/Cargo.toml" scripts/dev.sh start
EOF
}

log() {
  printf '[aethos-dev] %s\n' "$*"
}

fail() {
  printf '[aethos-dev] ERROR: %s\n' "$*" >&2
  exit 1
}

ensure_dirs() {
  mkdir -p "${STATE_DIR}" "${LOG_DIR}"
}

is_pid_running() {
  local pid="$1"
  [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null
}

read_pid() {
  local pid_file="$1"
  [[ -f "${pid_file}" ]] || return 1
  local pid
  pid="$(<"${pid_file}")"
  [[ "${pid}" =~ ^[0-9]+$ ]] || return 1
  printf '%s\n' "${pid}"
}

start_managed() {
  local name="$1"
  local command="$2"
  local log_file="$3"
  local pid_file="$4"

  if [[ -z "${command}" ]]; then
    return 0
  fi

  if local running_pid="$(read_pid "${pid_file}" 2>/dev/null)" && is_pid_running "${running_pid}"; then
    log "${name} already running (pid ${running_pid})"
    return 0
  fi

  log "starting ${name}"
  nohup bash -lc "${command}" >"${log_file}" 2>&1 &
  local pid=$!
  printf '%s\n' "${pid}" >"${pid_file}"
  log "${name} started (pid ${pid}, log ${log_file})"
}

stop_managed() {
  local name="$1"
  local pid_file="$2"

  local pid
  if ! pid="$(read_pid "${pid_file}" 2>/dev/null)"; then
    rm -f "${pid_file}"
    return 0
  fi

  if ! is_pid_running "${pid}"; then
    rm -f "${pid_file}"
    return 0
  fi

  log "stopping ${name} (pid ${pid})"
  kill "${pid}" 2>/dev/null || true

  local retries=40
  while is_pid_running "${pid}" && [[ ${retries} -gt 0 ]]; do
    sleep 0.1
    retries=$((retries - 1))
  done

  if is_pid_running "${pid}"; then
    log "${name} did not exit in time; sending SIGKILL"
    kill -9 "${pid}" 2>/dev/null || true
  fi

  rm -f "${pid_file}"
}

cmd_start() {
  ensure_dirs

  if [[ "${NO_BUILD}" != "1" ]]; then
    log "installing/updating Tauri desktop dependencies"
    npm --prefix "${APP_DIR}" install
  fi

  start_managed "app" "${APP_CMD}" "${APP_LOG}" "${APP_PID}"
  start_managed "relay-primary" "${RELAY1_CMD}" "${RELAY1_LOG}" "${RELAY1_PID}"
  start_managed "relay-secondary" "${RELAY2_CMD}" "${RELAY2_LOG}" "${RELAY2_PID}"
}

cmd_stop() {
  stop_managed "relay-secondary" "${RELAY2_PID}"
  stop_managed "relay-primary" "${RELAY1_PID}"
  stop_managed "app" "${APP_PID}"
}

cmd_status() {
  ensure_dirs
  local name pid_file pid
  for name in app relay-primary relay-secondary; do
    case "${name}" in
      app) pid_file="${APP_PID}" ;;
      relay-primary) pid_file="${RELAY1_PID}" ;;
      relay-secondary) pid_file="${RELAY2_PID}" ;;
    esac

    if pid="$(read_pid "${pid_file}" 2>/dev/null)" && is_pid_running "${pid}"; then
      printf '%-16s running (pid %s)\n' "${name}" "${pid}"
    else
      printf '%-16s stopped\n' "${name}"
    fi
  done
}

cmd_logs() {
  ensure_dirs
  local service="app"

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --service)
        [[ $# -ge 2 ]] || fail "--service requires a value"
        service="$2"
        shift 2
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        fail "unknown logs option: $1"
        ;;
    esac
  done

  local file
  case "${service}" in
    app) file="${APP_LOG}" ;;
    relay-primary) file="${RELAY1_LOG}" ;;
    relay-secondary) file="${RELAY2_LOG}" ;;
    *) fail "invalid service: ${service}" ;;
  esac

  touch "${file}"
  log "following ${service} logs: ${file}"
  tail -f "${file}"
}

main() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --no-build)
        NO_BUILD="1"
        shift
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        break
        ;;
    esac
  done

  local command="${1:-status}"
  if [[ $# -gt 0 ]]; then
    shift
  fi

  case "${command}" in
    start)
      cmd_start
      ;;
    stop)
      cmd_stop
      ;;
    restart)
      cmd_stop
      cmd_start
      ;;
    status)
      cmd_status
      ;;
    logs)
      cmd_logs "$@"
      ;;
    -h|--help)
      usage
      ;;
    *)
      fail "unknown command: ${command}"
      ;;
  esac
}

main "$@"
