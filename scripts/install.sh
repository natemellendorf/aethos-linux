#!/usr/bin/env bash
set -euo pipefail

DEFAULT_REPO="natemellendorf/aethos-linux"

REPO="${AETHOS_REPO:-$DEFAULT_REPO}"
REF="${AETHOS_REF:-}"
PREFIX="${AETHOS_PREFIX:-$HOME/.local}"
BIN_DIR="${AETHOS_BIN_DIR:-$PREFIX/bin}"

usage() {
  cat <<'EOF'
Aethos installer (Linux/macOS)

Usage:
  install.sh [options]

Options:
  --repo <owner/name>  GitHub repository (default: natemellendorf/aethos-linux)
  --ref <tag>          Release tag to install (default: latest official release)
  --prefix <dir>       Install prefix (default: ~/.local)
  --bin-dir <dir>      Binary install dir (default: <prefix>/bin)
  --help               Show this help

Environment overrides:
  AETHOS_REPO, AETHOS_REF, AETHOS_PREFIX, AETHOS_BIN_DIR

Examples:
  curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-linux/main/scripts/install.sh | bash
  curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-linux/main/scripts/install.sh | bash -s -- --ref v0.2.1
EOF
}

log() {
  printf '[aethos-install] %s\n' "$*"
}

fail() {
  printf '[aethos-install] ERROR: %s\n' "$*" >&2
  exit 1
}

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

need_prereqs() {
  has_cmd curl || fail "curl is required"
  has_cmd python3 || fail "python3 is required"
  has_cmd tar || fail "tar is required"
}

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux)
      case "$arch" in
        x86_64) TARGET_TRIPLE="x86_64-unknown-linux-gnu" ;;
        aarch64|arm64) TARGET_TRIPLE="aarch64-unknown-linux-gnu" ;;
        *) fail "unsupported Linux arch: ${arch}" ;;
      esac
      ARCHIVE_EXT="tar.gz"
      ;;
    Darwin)
      case "$arch" in
        x86_64) TARGET_TRIPLE="x86_64-apple-darwin" ;;
        aarch64|arm64) TARGET_TRIPLE="aarch64-apple-darwin" ;;
        *) fail "unsupported macOS arch: ${arch}" ;;
      esac
      ARCHIVE_EXT="tar.gz"
      ;;
    *)
      fail "unsupported OS: ${os} (use scripts/install.ps1 on Windows)"
      ;;
  esac
}

resolve_ref() {
  if [[ -n "${REF}" ]]; then
    log "Using explicit release tag: ${REF}"
    return
  fi

  local api_url json tag
  api_url="https://api.github.com/repos/${REPO}/releases/latest"
  json="$(curl -fsSL -H 'Accept: application/vnd.github+json' "${api_url}" || true)"
  tag="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read()).get("tag_name",""))' <<<"${json}" 2>/dev/null || true)"

  [[ -n "${tag}" ]] || fail "unable to resolve latest official release tag"
  REF="${tag}"
  log "Resolved latest release tag: ${REF}"
}

fetch_release_json() {
  local api_url
  api_url="https://api.github.com/repos/${REPO}/releases/tags/${REF}"
  curl -fsSL -H 'Accept: application/vnd.github+json' "${api_url}" \
    || fail "failed fetching release metadata for tag ${REF}"
}

asset_download_url() {
  local release_json="$1"
  local asset_name="$2"

  python3 - "$asset_name" <<'PY' <<<"${release_json}"
import json
import sys

asset = sys.argv[1]
data = json.loads(sys.stdin.read())
for item in data.get("assets", []):
    if item.get("name") == asset:
        print(item.get("browser_download_url", ""))
        sys.exit(0)
print("")
PY
}

install_from_release_asset() {
  local tmp_dir release_json asset_name asset_url archive_path

  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "$tmp_dir"' EXIT

  release_json="$(fetch_release_json)"
  asset_name="aethos-${REF}-${TARGET_TRIPLE}.${ARCHIVE_EXT}"
  asset_url="$(asset_download_url "${release_json}" "${asset_name}")"

  [[ -n "${asset_url}" ]] || fail "release ${REF} does not contain asset ${asset_name}"

  archive_path="${tmp_dir}/${asset_name}"
  log "Downloading ${asset_name}"
  curl -fL "${asset_url}" -o "${archive_path}"

  mkdir -p "${tmp_dir}/extract"
  tar -xzf "${archive_path}" -C "${tmp_dir}/extract"

  local binary_path="${tmp_dir}/extract/aethos"
  [[ -f "${binary_path}" ]] || fail "archive missing expected binary: aethos"

  install -d "${BIN_DIR}"
  install -m 0755 "${binary_path}" "${BIN_DIR}/aethos"
  ln -sfn "${BIN_DIR}/aethos" "${BIN_DIR}/aethos-linux"

  log "Installed: ${BIN_DIR}/aethos"
  log "Alias:     ${BIN_DIR}/aethos-linux -> ${BIN_DIR}/aethos"

  if [[ ":$PATH:" != *":${BIN_DIR}:"* ]]; then
    log "${BIN_DIR} is not in PATH. Add with:"
    log "  export PATH=\"${BIN_DIR}:\$PATH\""
  fi
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --repo)
        [[ $# -ge 2 ]] || fail "--repo requires a value"
        REPO="$2"
        shift 2
        ;;
      --ref)
        [[ $# -ge 2 ]] || fail "--ref requires a value"
        REF="$2"
        shift 2
        ;;
      --prefix)
        [[ $# -ge 2 ]] || fail "--prefix requires a value"
        PREFIX="$2"
        BIN_DIR="$PREFIX/bin"
        shift 2
        ;;
      --bin-dir)
        [[ $# -ge 2 ]] || fail "--bin-dir requires a value"
        BIN_DIR="$2"
        shift 2
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        fail "unknown argument: $1"
        ;;
    esac
  done
}

main() {
  parse_args "$@"
  need_prereqs
  detect_target
  resolve_ref
  install_from_release_asset
  log "Done. Run 'aethos'"
}

main "$@"
