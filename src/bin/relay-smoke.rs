#![allow(dead_code)]

#[path = "../aethos_core/mod.rs"]
mod aethos_core;
#[path = "../relay/mod.rs"]
mod relay;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use aethos_core::gossip_sync::{eligible_item_ids, record_local_payload};
use aethos_core::identity_store::{ensure_local_identity, load_local_signing_key_seed};
use aethos_core::logging::set_verbose_logging_enabled;
use aethos_core::protocol::{build_wayfarer_chat_envelope_payload_b64, is_valid_wayfarer_id};
use relay::client::{
    connect_to_relay_gossipv1_with_auth, run_relay_encounter_gossipv1_for_duration, to_ws_endpoint,
};

const DEFAULT_RELAY_ENDPOINT: &str = "wss://aethos-relay.network";
const DEFAULT_TTL_SECONDS: u64 = 120;
const DEFAULT_MAX_ROUNDS: usize = 3;
const DEFAULT_ENCOUNTER_WINDOW_MS: u64 = 2_000;

#[derive(Debug, Clone, Serialize)]
struct RoundResult {
    round: usize,
    elapsed_ms: u128,
    transferred_items: usize,
    pulled_messages: usize,
    remote_closed: bool,
    trace_requested_by_peer: bool,
    trace_receipted_by_peer: bool,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SmokeSummary {
    run_id: String,
    status: String,
    relay_endpoint: String,
    relay_ws: String,
    local_wayfarer_id: Option<String>,
    target_wayfarer_id: Option<String>,
    item_id: Option<String>,
    ttl_seconds: u64,
    max_rounds: usize,
    encounter_window_ms: u64,
    handshake_status: Option<String>,
    local_store_contains_item: bool,
    any_round_succeeded: bool,
    trace_requested_by_peer: bool,
    trace_receipted_by_peer: bool,
    rounds: Vec<RoundResult>,
    message: String,
}

fn main() -> ExitCode {
    let run_id = std::env::var("AETHOS_RELAY_SMOKE_RUN_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("run-{}", now_unix_ms()));
    let artifact_dir = artifact_dir_for_run(&run_id);

    if let Err(err) = fs::create_dir_all(&artifact_dir) {
        eprintln!(
            "relay-smoke: failed creating artifact dir {}: {err}",
            artifact_dir.display()
        );
        return ExitCode::FAILURE;
    }

    let mut log_lines = Vec::<String>::new();
    append_log(
        &mut log_lines,
        &format!("relay_smoke_start run_id={run_id}"),
    );

    let live_enabled = env_enabled("AETHOS_RUN_LIVE_RELAY_SMOKE");
    if !live_enabled {
        let summary = SmokeSummary {
            run_id: run_id.clone(),
            status: "skipped".to_string(),
            relay_endpoint: DEFAULT_RELAY_ENDPOINT.to_string(),
            relay_ws: DEFAULT_RELAY_ENDPOINT.to_string(),
            local_wayfarer_id: None,
            target_wayfarer_id: None,
            item_id: None,
            ttl_seconds: DEFAULT_TTL_SECONDS,
            max_rounds: DEFAULT_MAX_ROUNDS,
            encounter_window_ms: DEFAULT_ENCOUNTER_WINDOW_MS,
            handshake_status: None,
            local_store_contains_item: false,
            any_round_succeeded: false,
            trace_requested_by_peer: false,
            trace_receipted_by_peer: false,
            rounds: Vec::new(),
            message: "skipped: set AETHOS_RUN_LIVE_RELAY_SMOKE=1 to enable live relay smoke"
                .to_string(),
        };
        append_log(
            &mut log_lines,
            "relay_smoke_skipped reason=opt_in_env_not_set env=AETHOS_RUN_LIVE_RELAY_SMOKE",
        );
        return finalize(&artifact_dir, &summary, &log_lines, true);
    }

    let relay_endpoint = std::env::var("AETHOS_RELAY_ENDPOINT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_RELAY_ENDPOINT.to_string());
    let relay_ws = to_ws_endpoint(&relay_endpoint);
    let ttl_seconds = parse_u64_env(
        "AETHOS_RELAY_SMOKE_TTL_SECONDS",
        DEFAULT_TTL_SECONDS,
        45,
        600,
    );
    let max_rounds = parse_usize_env("AETHOS_RELAY_SMOKE_MAX_ROUNDS", DEFAULT_MAX_ROUNDS, 1, 10);
    let encounter_window_ms = parse_u64_env(
        "AETHOS_RELAY_SMOKE_ENCOUNTER_WINDOW_MS",
        DEFAULT_ENCOUNTER_WINDOW_MS,
        250,
        20_000,
    );

    let mut summary = SmokeSummary {
        run_id,
        status: "failed".to_string(),
        relay_endpoint: relay_endpoint.clone(),
        relay_ws: relay_ws.clone(),
        local_wayfarer_id: None,
        target_wayfarer_id: None,
        item_id: None,
        ttl_seconds,
        max_rounds,
        encounter_window_ms,
        handshake_status: None,
        local_store_contains_item: false,
        any_round_succeeded: false,
        trace_requested_by_peer: false,
        trace_receipted_by_peer: false,
        rounds: Vec::new(),
        message: String::new(),
    };

    set_verbose_logging_enabled(
        env_enabled("AETHOS_RELAY_SMOKE_VERBOSE") || env_enabled("AETHOS_VERBOSE_LOGGING"),
    );

    let target_wayfarer_id = match std::env::var("AETHOS_TARGET_WAYFARER_ID") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            summary.message =
                "missing required env AETHOS_TARGET_WAYFARER_ID (64 lowercase hex)".to_string();
            append_log(
                &mut log_lines,
                "relay_smoke_fail reason=missing_target_wayfarer_id",
            );
            return finalize(&artifact_dir, &summary, &log_lines, false);
        }
    };
    summary.target_wayfarer_id = Some(target_wayfarer_id.clone());

    if !is_valid_wayfarer_id(&target_wayfarer_id) {
        summary.message =
            "invalid AETHOS_TARGET_WAYFARER_ID (expected 64 lowercase hex chars)".to_string();
        append_log(
            &mut log_lines,
            "relay_smoke_fail reason=invalid_target_wayfarer_id_format",
        );
        return finalize(&artifact_dir, &summary, &log_lines, false);
    }

    let identity = match ensure_local_identity() {
        Ok(identity) => identity,
        Err(err) => {
            summary.message = format!("failed to load local identity: {err}");
            append_log(
                &mut log_lines,
                &format!("relay_smoke_fail reason=identity_error error={err}"),
            );
            return finalize(&artifact_dir, &summary, &log_lines, false);
        }
    };
    summary.local_wayfarer_id = Some(identity.wayfarer_id.clone());

    let local_seed = match load_local_signing_key_seed() {
        Ok(seed) => seed,
        Err(err) => {
            summary.message = format!("failed to load local signing seed: {err}");
            append_log(
                &mut log_lines,
                &format!("relay_smoke_fail reason=signing_seed_error error={err}"),
            );
            return finalize(&artifact_dir, &summary, &log_lines, false);
        }
    };

    let auth_token = std::env::var("AETHOS_RELAY_AUTH_TOKEN").ok();
    append_log(
        &mut log_lines,
        &format!(
            "relay_smoke_connect_start relay_ws={} local_wayfarer_id={}",
            relay_ws, identity.wayfarer_id
        ),
    );
    let handshake_status =
        connect_to_relay_gossipv1_with_auth(&relay_ws, &identity, auth_token.as_deref());
    summary.handshake_status = Some(handshake_status.clone());
    append_log(
        &mut log_lines,
        &format!("relay_smoke_connect_result status={handshake_status}"),
    );

    if !handshake_status.starts_with("connected + HELLO") {
        summary.message = format!("relay handshake failed: {handshake_status}");
        return finalize(&artifact_dir, &summary, &log_lines, false);
    }

    let message_body = format!("relay-smoke:{}:{}", summary.run_id, now_unix_ms());
    let payload_b64 = match build_wayfarer_chat_envelope_payload_b64(
        &target_wayfarer_id,
        &message_body,
        &local_seed,
        now_unix_ms() as i64,
    ) {
        Ok(payload) => payload,
        Err(err) => {
            summary.message = format!("failed to build envelope payload: {err}");
            append_log(
                &mut log_lines,
                &format!("relay_smoke_fail reason=build_payload_error error={err}"),
            );
            return finalize(&artifact_dir, &summary, &log_lines, false);
        }
    };

    let expiry_unix_ms = now_unix_ms().saturating_add(ttl_seconds.saturating_mul(1_000));
    let item_id = match record_local_payload(&payload_b64, expiry_unix_ms) {
        Ok(item_id) => item_id,
        Err(err) => {
            summary.message = format!("failed to persist envelope in local store: {err}");
            append_log(
                &mut log_lines,
                &format!("relay_smoke_fail reason=record_local_payload_error error={err}"),
            );
            return finalize(&artifact_dir, &summary, &log_lines, false);
        }
    };
    summary.item_id = Some(item_id.clone());
    append_log(
        &mut log_lines,
        &format!(
            "relay_smoke_publish_local item_id={} ttl_seconds={} expires_at_unix_ms={}",
            item_id, ttl_seconds, expiry_unix_ms
        ),
    );

    let local_ids = match eligible_item_ids(now_unix_ms()) {
        Ok(item_ids) => item_ids,
        Err(err) => {
            summary.message = format!("failed reading local store after publish: {err}");
            append_log(
                &mut log_lines,
                &format!("relay_smoke_fail reason=local_store_read_error error={err}"),
            );
            return finalize(&artifact_dir, &summary, &log_lines, false);
        }
    };
    summary.local_store_contains_item = local_ids.iter().any(|value| value == &item_id);
    if !summary.local_store_contains_item {
        summary.message =
            "published item did not appear in local gossip store candidate set".to_string();
        append_log(
            &mut log_lines,
            "relay_smoke_fail reason=local_store_missing_published_item",
        );
        return finalize(&artifact_dir, &summary, &log_lines, false);
    }

    for round in 1..=max_rounds {
        let started = Instant::now();
        let result = run_relay_encounter_gossipv1_for_duration(
            &relay_ws,
            &identity,
            auth_token.as_deref(),
            Some(&item_id),
            Duration::from_millis(encounter_window_ms),
        );

        match result {
            Ok(report) => {
                summary.any_round_succeeded = true;
                summary.trace_requested_by_peer |= report.trace_requested_by_peer;
                summary.trace_receipted_by_peer |= report.trace_receipted_by_peer;
                append_log(
                    &mut log_lines,
                    &format!(
                        "relay_smoke_round_ok round={} transferred_items={} pulled_messages={} trace_requested_by_peer={} trace_receipted_by_peer={} remote_closed={}",
                        round,
                        report.transferred_items,
                        report.pulled_messages.len(),
                        report.trace_requested_by_peer,
                        report.trace_receipted_by_peer,
                        report.remote_closed,
                    ),
                );

                summary.rounds.push(RoundResult {
                    round,
                    elapsed_ms: started.elapsed().as_millis(),
                    transferred_items: report.transferred_items,
                    pulled_messages: report.pulled_messages.len(),
                    remote_closed: report.remote_closed,
                    trace_requested_by_peer: report.trace_requested_by_peer,
                    trace_receipted_by_peer: report.trace_receipted_by_peer,
                    error: None,
                });
            }
            Err(err) => {
                append_log(
                    &mut log_lines,
                    &format!("relay_smoke_round_err round={} error={err}", round),
                );
                summary.rounds.push(RoundResult {
                    round,
                    elapsed_ms: started.elapsed().as_millis(),
                    transferred_items: 0,
                    pulled_messages: 0,
                    remote_closed: false,
                    trace_requested_by_peer: false,
                    trace_receipted_by_peer: false,
                    error: Some(err),
                });
            }
        }

        if summary.trace_requested_by_peer || summary.trace_receipted_by_peer {
            break;
        }
    }

    if !summary.any_round_succeeded {
        summary.message =
            "all encounter rounds failed; see round errors in summary.json".to_string();
        return finalize(&artifact_dir, &summary, &log_lines, false);
    }

    summary.status = "passed".to_string();
    summary.message = if summary.trace_receipted_by_peer {
        "success: handshake ok, local store has item, and peer receipted traced item".to_string()
    } else if summary.trace_requested_by_peer {
        "success: handshake ok, local store has item, and peer requested traced item".to_string()
    } else {
        "success: handshake ok, local store has item, and bounded encounter completed".to_string()
    };

    finalize(&artifact_dir, &summary, &log_lines, true)
}

fn artifact_dir_for_run(run_id: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("artifacts")
        .join("relay-smoke")
        .join(run_id)
}

fn finalize(
    artifact_dir: &Path,
    summary: &SmokeSummary,
    log_lines: &[String],
    success_exit: bool,
) -> ExitCode {
    let mut writes_ok = true;

    let log_path = artifact_dir.join("run.log");
    if let Err(err) = fs::write(&log_path, format!("{}\n", log_lines.join("\n"))) {
        writes_ok = false;
        eprintln!("relay-smoke: failed writing {}: {err}", log_path.display());
    }

    let summary_path = artifact_dir.join("summary.json");
    match serde_json::to_string_pretty(summary) {
        Ok(serialized) => {
            if let Err(err) = fs::write(&summary_path, format!("{serialized}\n")) {
                writes_ok = false;
                eprintln!(
                    "relay-smoke: failed writing {}: {err}",
                    summary_path.display()
                );
            }
        }
        Err(err) => {
            writes_ok = false;
            eprintln!("relay-smoke: failed serializing summary: {err}");
        }
    }

    let artifact_index = serde_json::json!({
        "run_id": summary.run_id,
        "status": summary.status,
        "artifacts": ["run.log", "summary.json"]
    });
    let artifact_index_path = artifact_dir.join("artifact-index.json");
    match serde_json::to_string_pretty(&artifact_index) {
        Ok(serialized) => {
            if let Err(err) = fs::write(&artifact_index_path, format!("{serialized}\n")) {
                writes_ok = false;
                eprintln!(
                    "relay-smoke: failed writing {}: {err}",
                    artifact_index_path.display()
                );
            }
        }
        Err(err) => {
            writes_ok = false;
            eprintln!("relay-smoke: failed serializing artifact-index: {err}");
        }
    }

    println!(
        "relay-smoke status={} message={}",
        summary.status, summary.message
    );
    println!("relay-smoke artifacts={}", artifact_dir.display());

    if success_exit && writes_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn append_log(lines: &mut Vec<String>, message: &str) {
    lines.push(format!("[{}] {message}", now_unix_ms()));
}

fn parse_u64_env(key: &str, fallback: u64, min: u64, max: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map(|value| value.clamp(min, max))
        .unwrap_or(fallback)
}

fn parse_usize_env(key: &str, fallback: usize, min: usize, max: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .map(|value| value.clamp(min, max))
        .unwrap_or(fallback)
}

fn env_enabled(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn now_unix_ms() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(_) => 0,
    }
}
