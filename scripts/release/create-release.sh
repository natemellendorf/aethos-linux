#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "${SCRIPT_DIR}/lib.sh"

DRY_RUN="0"
FROM_PRERELEASE_TAG=""

usage() {
  cat <<'EOF'
Create an official Aethos Linux release.

Usage:
  scripts/release/create-release.sh [--dry-run]
  scripts/release/create-release.sh --from-prerelease <tag> [--dry-run]

Behavior:
  promote mode:
  - promotes an existing prerelease into a new official release tag
  - derives official tag by stripping '-pre.*' suffix (example: v1.2.3-pre.* -> v1.2.3)

  cut mode:
  - requires clean git state on main
  - infers next semver from conventional commits since last v* tag
  - updates Cargo.toml version
  - runs cargo test
  - commits + tags release
  - creates GitHub release with generated notes
EOF
}

log() {
  printf '[aethos-release] %s\n' "$*"
}

fail() {
  printf '[aethos-release] ERROR: %s\n' "$*" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      DRY_RUN="1"
      shift
      ;;
    --from-prerelease)
      [[ $# -ge 2 ]] || fail "--from-prerelease requires a tag"
      FROM_PRERELEASE_TAG="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown option: $1"
      ;;
  esac
done

command -v gh >/dev/null 2>&1 || fail "gh CLI is required"

cd "$(repo_root)"

if [[ -n "${FROM_PRERELEASE_TAG}" ]]; then
  is_prerelease="$(gh release view "${FROM_PRERELEASE_TAG}" --json isPrerelease --jq '.isPrerelease' 2>/dev/null || true)"
  [[ -n "${is_prerelease}" ]] || fail "release ${FROM_PRERELEASE_TAG} not found on GitHub"

  if [[ "${is_prerelease}" != "true" && "${FROM_PRERELEASE_TAG}" != *-pre.* ]]; then
    fail "release ${FROM_PRERELEASE_TAG} is already official"
  fi

  official_tag="${FROM_PRERELEASE_TAG%%-pre.*}"
  [[ -n "${official_tag}" ]] || fail "failed to derive official tag from ${FROM_PRERELEASE_TAG}"
  [[ "${official_tag}" != "${FROM_PRERELEASE_TAG}" ]] || {
    fail "prerelease tag ${FROM_PRERELEASE_TAG} does not include '-pre' suffix"
  }

  if gh release view "${official_tag}" >/dev/null 2>&1; then
    fail "official release ${official_tag} already exists"
  fi

  target_commitish="$(gh release view "${FROM_PRERELEASE_TAG}" --json targetCommitish --jq '.targetCommitish')"
  prerelease_body="$(gh release view "${FROM_PRERELEASE_TAG}" --json body --jq '.body')"

  log "promoting prerelease: ${FROM_PRERELEASE_TAG} -> ${official_tag}"
  if [[ "${DRY_RUN}" == "1" ]]; then
    log "dry run complete"
    exit 0
  fi

  gh release create "${official_tag}" \
    --target "${target_commitish}" \
    --title "Aethos Linux ${official_tag}" \
    --notes "${prerelease_body}"

  gh release edit "${official_tag}" \
    --latest \
    --prerelease=false

  log "release promoted: ${official_tag}"
  exit 0
fi

[[ "$(git rev-parse --abbrev-ref HEAD)" == "main" ]] || fail "releases must run from main"
[[ -z "$(git status --porcelain)" ]] || fail "working tree must be clean"

current="$(current_version)"
next="$(next_version)"

if [[ "${current}" == "${next}" ]]; then
  fail "no version bump inferred from commits; nothing to release"
fi

tag="v${next}"
if git rev-parse "${tag}" >/dev/null 2>&1; then
  fail "tag ${tag} already exists locally"
fi

if gh release view "${tag}" >/dev/null 2>&1; then
  fail "release ${tag} already exists on GitHub"
fi

log "current version: ${current}"
log "next version:    ${next}"

if [[ "${DRY_RUN}" == "1" ]]; then
  log "dry run complete"
  exit 0
fi

perl -0777 -i -pe 's/^version = "[0-9]+\.[0-9]+\.[0-9]+"/version = "'"${next}"'"/m' Cargo.toml

cargo test

git add Cargo.toml
git commit -m "chore(release): ${tag}"
git tag -a "${tag}" -m "Release ${tag}"

gh release create "${tag}" \
  --target "$(git rev-parse HEAD)" \
  --title "Aethos Linux ${tag}" \
  --generate-notes

log "release created: ${tag}"
log "next step: git push origin main --follow-tags"
