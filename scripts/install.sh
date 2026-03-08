#!/usr/bin/env bash
set -euo pipefail

DEFAULT_REPO="natemellendorf/aethos-linux"
DEFAULT_REF="main"

REPO="${AETHOS_REPO:-$DEFAULT_REPO}"
REF="${AETHOS_REF:-$DEFAULT_REF}"
PREFIX="${AETHOS_PREFIX:-$HOME/.local}"
BIN_DIR="${AETHOS_BIN_DIR:-$PREFIX/bin}"
SKIP_DEPS="${AETHOS_SKIP_DEPS:-0}"

usage() {
  cat <<'EOF'
Aethos Linux installer

Usage:
  install.sh [options]

Options:
  --repo <owner/name>  GitHub repository (default: natemellendorf/aethos-linux)
  --ref <git-ref>      Branch, tag, or commit to install (default: main)
  --prefix <dir>       Install prefix (default: ~/.local)
  --bin-dir <dir>      Binary install dir (default: <prefix>/bin)
  --skip-deps          Skip OS/Rust dependency bootstrap
  --help               Show this help

Environment overrides:
  AETHOS_REPO, AETHOS_REF, AETHOS_PREFIX, AETHOS_BIN_DIR, AETHOS_SKIP_DEPS

Examples:
  curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-linux/main/scripts/install.sh | bash
  curl -fsSL https://raw.githubusercontent.com/natemellendorf/aethos-linux/main/scripts/install.sh | bash -s -- --ref v0.1.0
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

need_linux() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    fail "This installer currently supports Linux only."
  fi
}

ensure_apt_deps() {
  if [[ "$SKIP_DEPS" == "1" ]]; then
    log "Skipping dependency bootstrap (--skip-deps)."
    return
  fi

  if ! has_cmd apt-get; then
    log "apt-get not found; skipping OS package install."
    return
  fi

  local sudo_cmd=""
  if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
    if has_cmd sudo; then
      sudo_cmd="sudo"
    else
      fail "Need root or sudo to install system dependencies. Re-run with --skip-deps if already installed."
    fi
  fi

  log "Installing Ubuntu/Debian dependencies (build-essential, pkg-config, libgtk-4-dev, libglib2.0-dev, curl)."
  $sudo_cmd apt-get update -y
  $sudo_cmd apt-get install -y build-essential pkg-config libgtk-4-dev libglib2.0-dev curl ca-certificates
}

ensure_rust() {
  if has_cmd cargo; then
    return
  fi

  if [[ "$SKIP_DEPS" == "1" ]]; then
    fail "cargo not found and --skip-deps was provided. Install Rust first: https://rustup.rs"
  fi

  if ! has_cmd curl; then
    fail "curl is required to bootstrap Rust via rustup."
  fi

  log "Installing Rust toolchain via rustup."
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal

  if [[ -f "$HOME/.cargo/env" ]]; then
    # shellcheck disable=SC1090
    source "$HOME/.cargo/env"
  fi

  has_cmd cargo || fail "cargo is still unavailable after rustup install."
}

download_source() {
  local target_dir="$1"
  local archive_url="https://github.com/${REPO}/archive/${REF}.tar.gz"
  local archive_path="$target_dir/source.tar.gz"

  log "Downloading source archive: ${archive_url}"
  curl -fL "$archive_url" -o "$archive_path"

  tar -xzf "$archive_path" -C "$target_dir"

  local extracted=""
  local candidate
  for candidate in "$target_dir"/*; do
    if [[ -d "$candidate" && -f "$candidate/Cargo.toml" ]]; then
      extracted="$candidate"
      break
    fi
  done

  [[ -n "$extracted" ]] || fail "Unable to find extracted source directory with Cargo.toml."
  printf '%s\n' "$extracted"
}

build_and_install() {
  local source_dir="$1"

  log "Building aethos-linux in release mode."
  cargo build --release --locked --manifest-path "$source_dir/Cargo.toml"

  local binary_path="$source_dir/target/release/aethos-linux"
  [[ -x "$binary_path" ]] || fail "Build succeeded but binary not found at $binary_path"

  install -d "$BIN_DIR"
  install -m 0755 "$binary_path" "$BIN_DIR/aethos-linux"
  log "Installed: $BIN_DIR/aethos-linux"
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
      --skip-deps)
        SKIP_DEPS="1"
        shift
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        fail "Unknown argument: $1"
        ;;
    esac
  done
}

main() {
  parse_args "$@"
  need_linux
  ensure_apt_deps
  ensure_rust

  local tmp_dir
  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "$tmp_dir"' EXIT

  local source_dir
  source_dir="$(download_source "$tmp_dir")"
  build_and_install "$source_dir"

  log "Done. Run 'aethos-linux' after adding $BIN_DIR to PATH if needed."
}

main "$@"
