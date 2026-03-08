# AGENTS.md

> Source requested: `natemellendorf/aethos-relay` AGENTS.md.
> Network access to GitHub raw endpoints was blocked in this environment during this run,
> so this file is a placeholder to be replaced with the upstream canonical content.

## Working agreement for this repository

- Keep all implementation work for Linux client in this repository.
- Prioritize native Linux-first choices (Ubuntu/Debian).
- Maintain protocol compatibility with upstream Aethos protocol and relay contract.
- Build in incremental milestones documented in `docs/project-charter.md`.

## Git lifecycle expectations

- Hooks are repository-managed via `scripts/setup-git-hooks.sh` and should be installed in local dev environments.
- Commit gate (`pre-commit`): run full project lint checks before commit is accepted.
- Merge gate (`pre-merge-commit`): when merging local `main` tip, run full test suite before merge commit completes.
- Push gate (`pre-push`): when pushing `origin/main`, create a GitHub prerelease only on explicit request (`AETHOS_CREATE_PRERELEASE=1` or `git push -o prerelease`).
- Official releases should be cut only through `scripts/release/create-release.sh` (or promoted from prerelease via `--from-prerelease`, which creates the clean `vX.Y.Z` official tag) to keep versioning/changelog/tag flow consistent.

## Release policy

- Release version bump is inferred from conventional-commit style messages since the last `v*` tag.
- Breaking changes (`!` or `BREAKING CHANGE:`) trigger major bumps.
- `feat:` changes trigger minor bumps.
- Other changes trigger patch bumps.
- CI binary artifact builds run automatically only for official releases; prerelease binary builds require explicit opt-in.
