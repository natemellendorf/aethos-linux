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
- Merge gate additionally runs GUI+network E2E gate (`scripts/e2e/pre-merge-gate.sh`) so both pass-mode and impaired/failure-mode scenarios are exercised and artifacted.
- Push gate (`pre-push`): when pushing `origin/main`, create a GitHub prerelease only on explicit request (`AETHOS_CREATE_PRERELEASE=1` or `git push -o prerelease`).
- Official releases should be cut only through `scripts/release/create-release.sh` (or promoted from prerelease via `--from-prerelease`, which creates the clean `vX.Y.Z` official tag) to keep versioning/changelog/tag flow consistent.

## Release policy

- Release version bump is inferred from conventional-commit style messages since the last `v*` tag.
- Breaking changes (`!` or `BREAKING CHANGE:`) trigger major bumps.
- `feat:` changes trigger minor bumps.
- Other changes trigger patch bumps.
- CI binary artifact builds run automatically only for official releases; prerelease binary builds require explicit opt-in.

## E2E harness expectations (agent-operable)

- Use `docs/testing/gui-network-e2e.md` as the source of truth for the Linux-first GUI+network harness.
- Default autonomous flow should use named scenarios from `tests/e2e-harness/config/scenarios/` and collect artifacts under `tests/e2e-harness/artifacts/<run-id>/`.
- When debugging convergence/fault-tolerance issues, run with `scripts/e2e/run-scenario.sh` and include `artifact-index.json`, `failure-summary.json`, and `triage-summary.json` in analysis.
- Prefer reproducible, seed/config-driven reruns over ad-hoc manual test sessions.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->
