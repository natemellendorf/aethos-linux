# Wayfarer payload contract conformance (Tauri desktop)

This note documents the Tauri desktop work to conform to the upstream Wayfarer “app body” payload contract and fixture taxonomy, using the authoritative fixtures vendored via the `third_party/aethos` submodule.

## Prior divergence

Earlier iterations of the Tauri desktop spike accepted and produced payload bodies that were *functionally* decodable but not contract-conformant:

- **Encoding determinism was not enforced**: the same semantic CBOR value could be accepted even when the byte-level encoding was not deterministic/canonical.
- **Type routing was inconsistent**: unknown payload `type` values could be treated as errors (or accidentally routed into typed decoders), rather than being safely skippable.
- **Reserved payload types lacked an explicit policy**: types reserved by the contract were not clearly separated from “unknown future” types.

These gaps made cross-client interop brittle and undermined fixture-based verification.

## What changed (implementation-linked)

The contract-conformance logic now lives in `spikes/tauri-desktop/src-tauri/src/app_body.rs` and is driven directly by the upstream fixture taxonomy:

- `classify_wayfarer_app_body(body: &[u8]) -> ClassificationResult` performs:
  - **Exact CBOR decode** (`decode_cbor_value_exact`)
  - **Deterministic re-encode** (`encode_cbor_value_deterministic`) and **byte-for-byte equality** enforcement (`non_deterministic_cbor_encoding` rejection)
  - **Strict `type` extraction** (`missing_type`, `type_must_be_text`)
  - **Outcome routing** with explicit labels: `accept/display`, `accept/store-no-display`, `reject`, `unsupported-safe-skip`
- Typed decoders are strict and scope-limited:
  - `wayfarer.chat.v1` → `AcceptDisplay` only when `text` and `created_at_unix_ms` are present and valid.
  - `wayfarer.media_manifest.v1` → `AcceptStoreNoDisplay` only when `transfer_ref`, `media_kind`, `assets[]`, and `created_at_unix_ms` validate.
- Reserved contract types are explicitly accepted for storage without display, without running typed decoders.

## Behavior matrix (summary)

| Payload category | Example `type` | Expected outcome | Decoder ran? | Notes |
|---|---|---:|---:|---|
| Displayable chat | `wayfarer.chat.v1` | `accept/display` | Yes | Strict field validation; deterministic CBOR required |
| Store-only media manifest | `wayfarer.media_manifest.v1` | `accept/store-no-display` | Yes | Validates `media_kind` ∈ {image, video, audio, file} and non-empty `assets` |
| Malformed known type | `wayfarer.chat.v1` (bad shape) | `reject` | Yes | Reject reason is namespaced (e.g. `malformed_wayfarer_chat_v1:*`) |
| Reserved type | `wayfarer.profile.v1` (etc.) | `accept/store-no-display` | No | Explicit reserved allowlist; no typed routing |
| Unknown future type | `future.poll.v1` | `unsupported-safe-skip` | No | Safe to ignore; not an error; not routed |
| Invalid CBOR | (n/a) | `reject` | No | Fails before any typed decoding |

## Fixtures (authoritative, pinned)

Contract verification uses the upstream fixture taxonomy located at:

`third_party/aethos/Fixtures/App/wayfarer-payload-taxonomy/`

The `third_party/aethos` submodule is expected to be pinned to commit:

`8e23a0615bf2ad709655a9f429d9710c8384f35c`

The Tauri crate test runner reads `manifest.json` and asserts per-fixture classification outcomes (and selected decoded-map expectations).

## Verification commands

Run the fixture-driven contract tests from the Tauri Rust crate:

```bash
cd spikes/tauri-desktop/src-tauri
cargo test
```

To focus on the taxonomy runner:

```bash
cd spikes/tauri-desktop/src-tauri
cargo test taxonomy_fixtures_classify_with_expected_outcomes
```

And to sanity-check unknown/reserved behaviors:

```bash
cd spikes/tauri-desktop/src-tauri
cargo test unsupported_types_safe_skip_without_running_typed_decoders reserved_types_accept_store_no_display
```
