# Fixture Versioning and Change Control

This suite is normative for encounter-ranking conformance in scheduler contract v1.

## Change categories

1. **Non-semantic edits** (typo cleanup in descriptions, formatting only)
   - MUST NOT change expected outputs.

2. **Fixture-semantic edits** (cargo inputs, expected rankings, selected prefix, score breakdowns, tie/stop reasons)
   - MUST receive deliberate protocol review.
   - MUST include rationale tied to scheduler contract text.
   - MUST update any affected acceptance mapping notes in `GUIDE.md`.

3. **Scoring/ranking contract edits** (weights, normalization, tie-break chain, stop reason semantics)
   - MUST be treated as contract-impacting.
   - MUST update this fixture suite in the same change.
   - MUST provide explicit migration notes for implementers.

## Required review checklist for semantic changes

- Identify exactly which lane(s) changed and why.
- Confirm hard tier precedence remains intact where required.
- Recompute and verify all affected `scoreNumerator` values.
- Re-run repeated-run stability checks.
- Validate all fixture JSON against `schema.json`.

## Versioning rule

- Keep `schemaVersion: encounter-ranking.v1` unchanged unless schema structure changes.
- If schema structure changes, publish a new schema version and a new fixture suite version intentionally (do not silently mutate v1 semantics).
