#!/usr/bin/env bash
set -euo pipefail

DEFAULT_REPO="natemellendorf/aethos-client"

REPO="${AETHOS_REPO:-$DEFAULT_REPO}"
REF="${AETHOS_REF:-}"
PREFIX="${AETHOS_PREFIX:-$HOME/.local}"
BIN_DIR="${AETHOS_BIN_DIR:-$PREFIX/bin}"
OS_NAME=""
ARCH_NAME=""
TARGET_TRIPLE=""
ARCHIVE_EXT="tar.gz"

usage() {
  cat <<'EOF'
Aethos installer (Linux/macOS)

Usage:
  install.sh [options]

Options:
  --repo <owner/name>  GitHub repository (default: natemellendorf/aethos-client)
  --ref <tag>          Release tag to install (default: latest official release)
  --prefix <dir>       Install prefix (default: ~/.local)
  --bin-dir <dir>      Binary install dir (default: <prefix>/bin)
  --help               Show this help

Environment overrides:
  AETHOS_REPO, AETHOS_REF, AETHOS_PREFIX, AETHOS_BIN_DIR

Examples:
  curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-client/main/scripts/install.sh | bash
  curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-client/main/scripts/install.sh | bash -s -- --ref v0.2.1
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

ensure_runtime_deps() {
  local os
  os="$(uname -s)"

  case "$os" in
    Darwin)
      has_cmd brew || fail "Homebrew is required on macOS. Install from https://brew.sh then run: brew install gtk4 pkg-config"
      if ! brew list --versions gtk4 >/dev/null 2>&1; then
        fail "Missing macOS runtime dependency: gtk4. Install with: brew install gtk4 pkg-config"
      fi
      if ! has_cmd pkg-config; then
        fail "Missing pkg-config. Install with: brew install pkg-config"
      fi
      ;;
    Linux)
      # Linux runtime packages vary by distro. If GTK runtime is missing,
      # launch will fail with a dynamic linker error and package manager should
      # be used to install gtk4/glib runtime packages.
      ;;
  esac
}

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  OS_NAME="$os"
  ARCH_NAME="$arch"

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

select_release_asset() {
  local release_json="$1"

  RELEASE_JSON="$release_json" python3 - "$OS_NAME" "$ARCH_NAME" "$REF" "$TARGET_TRIPLE" <<'PY'
import json
import os
import sys

os_name, arch_name, ref, target = sys.argv[1:5]
data = json.loads(os.environ.get("RELEASE_JSON", "{}"))
assets = data.get("assets", [])

def first_match(predicate):
    for item in assets:
        name = item.get("name", "")
        if predicate(name):
            return name, item.get("browser_download_url", "")
    return "", ""

# 1) Legacy CLI tarball naming (still supported)
legacy = f"aethos-{ref}-{target}.tar.gz"
for item in assets:
    if item.get("name") == legacy:
        print(f"{legacy}\t{item.get('browser_download_url', '')}")
        raise SystemExit(0)

name_l = lambda n: n.lower()
arch_tokens = {
    "x86_64": ["x86_64", "amd64", "x64"],
    "aarch64": ["aarch64", "arm64"],
    "arm64": ["aarch64", "arm64"],
}.get(arch_name, [arch_name.lower()])

def has_arch(name):
    lower = name_l(name)
    return any(tok in lower for tok in arch_tokens)

if os_name == "Linux":
    name, url = first_match(lambda n: name_l(n).endswith(".appimage") and has_arch(n))
    if not name:
        name, url = first_match(lambda n: name_l(n).endswith(".appimage"))
    if not name:
        name, url = first_match(lambda n: name_l(n).endswith(".deb") and has_arch(n))
elif os_name == "Darwin":
    name, url = first_match(lambda n: name_l(n).endswith(".dmg") and has_arch(n))
    if not name:
        name, url = first_match(lambda n: name_l(n).endswith(".dmg"))
else:
    name, url = "", ""

if not name:
    print("\t")
else:
    print(f"{name}\t{url}")
PY
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

  python3 -c 'import json,sys
asset = sys.argv[1]
data = json.loads(sys.stdin.read())
for item in data.get("assets", []):
    if item.get("name") == asset:
        print(item.get("browser_download_url", ""))
        break
else:
    print("")' "$asset_name" <<<"${release_json}"
}

install_from_release_asset() {
  local tmp_dir release_json asset_name asset_url archive_path

  tmp_dir="$(mktemp -d)"
  trap "rm -rf '$tmp_dir'" EXIT

  release_json="$(fetch_release_json)"
  IFS=$'\t' read -r asset_name asset_url < <(select_release_asset "${release_json}")

  [[ -n "${asset_url}" ]] || fail "release ${REF} does not contain a supported installer asset for ${OS_NAME}/${ARCH_NAME}"

  archive_path="${tmp_dir}/${asset_name}"
  log "Downloading ${asset_name}"
  curl -fL "${asset_url}" -o "${archive_path}"

  case "${asset_name,,}" in
    *.tar.gz)
      mkdir -p "${tmp_dir}/extract"
      tar -xzf "${archive_path}" -C "${tmp_dir}/extract"
      local binary_path="${tmp_dir}/extract/aethos"
      [[ -f "${binary_path}" ]] || fail "archive missing expected binary: aethos"
      install -d "${BIN_DIR}"
      install -m 0755 "${binary_path}" "${BIN_DIR}/aethos"
      ln -sfn "${BIN_DIR}/aethos" "${BIN_DIR}/aethos-linux"
      log "Installed: ${BIN_DIR}/aethos"
      ;;
    *.appimage)
      install -d "${BIN_DIR}"
      install -m 0755 "${archive_path}" "${BIN_DIR}/aethos"
      ln -sfn "${BIN_DIR}/aethos" "${BIN_DIR}/aethos-linux"
      log "Installed AppImage launcher at: ${BIN_DIR}/aethos"
      ;;
    *.deb)
      if has_cmd sudo; then
        log "Installing Debian package via sudo dpkg -i"
        sudo dpkg -i "${archive_path}" || fail "dpkg install failed"
      else
        fail "Downloaded .deb but sudo is unavailable. Install manually: sudo dpkg -i ${archive_path}"
      fi
      ;;
    *.dmg)
      if has_cmd open; then
        log "Opening macOS DMG installer"
        open "${archive_path}" || fail "failed opening DMG"
      else
        fail "Downloaded .dmg to ${archive_path}; open it manually"
      fi
      ;;
    *)
      fail "unsupported installer asset selected: ${asset_name}"
      ;;
  esac

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
  ensure_runtime_deps
  resolve_ref
  install_from_release_asset
  log "Done. Run 'aethos' (or complete installer prompts if applicable)"
}

main "$@"
