#![allow(dead_code)]

#[path = "../aethos_core/mod.rs"]
mod aethos_core;
#[path = "../relay/mod.rs"]
mod relay;

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

use aethos_core::gossip_sync::record_local_payload;
use aethos_core::identity_store::{ensure_local_identity, load_local_signing_key_seed};
use aethos_core::protocol::build_wayfarer_chat_envelope_payload_b64;
use relay::client::run_relay_encounter_gossipv1_for_duration;

const DEFAULT_TIMEOUT_SECONDS: u64 = 90;
const DEFAULT_ENCOUNTER_SECONDS: u64 = 5;
const DEFAULT_TTL_SECONDS: u64 = 120;

#[derive(Debug, Clone)]
struct SenderConfig {
    relay_ws: String,
    to_wayfarer_id: String,
    message: String,
    timeout: Duration,
    encounter_window: Duration,
    ttl_seconds: u64,
    artifact_dir: PathBuf,
    auth_token: Option<String>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("interop_sender_error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let config = parse_args()?;
    fs::create_dir_all(&config.artifact_dir).map_err(|err| {
        format!(
            "failed to create artifact directory at {}: {err}",
            config.artifact_dir.display()
        )
    })?;

    let identity = ensure_local_identity()?;
    let signing_seed = load_local_signing_key_seed()?;
    let started_at_ms = now_unix_ms();

    println!("wayfarer_id={}", identity.wayfarer_id);
    println!("relay_ws={}", config.relay_ws);
    println!("to_wayfarer_id={}", config.to_wayfarer_id);
    println!("message={}", config.message);

    write_json_artifact(
        &config.artifact_dir,
        "sender-info.json",
        &json!({
            "wayfarer_id": identity.wayfarer_id,
            "relay_ws": config.relay_ws,
            "to_wayfarer_id": config.to_wayfarer_id,
            "message": config.message,
            "started_at_unix_ms": started_at_ms,
            "timeout_seconds": config.timeout.as_secs(),
            "encounter_seconds": config.encounter_window.as_secs(),
            "ttl_seconds": config.ttl_seconds,
        }),
    )?;

    let payload = build_wayfarer_chat_envelope_payload_b64(
        &config.to_wayfarer_id,
        &config.message,
        &signing_seed,
        now_unix_ms() as i64,
    )?;
    let expiry_unix_ms = now_unix_ms().saturating_add(config.ttl_seconds.saturating_mul(1_000));
    let item_id = record_local_payload(&payload, expiry_unix_ms)?;

    let deadline = Instant::now() + config.timeout;
    let mut attempts = 0usize;
    let mut trace_requested = false;
    let mut trace_receipted = false;
    let mut transferred_any = false;
    let mut last_error: Option<String> = None;

    while Instant::now() < deadline {
        attempts = attempts.saturating_add(1);
        let remaining = deadline.saturating_duration_since(Instant::now());
        let encounter_window = remaining.min(config.encounter_window);

        match run_relay_encounter_gossipv1_for_duration(
            &config.relay_ws,
            &identity,
            config.auth_token.as_deref(),
            Some(&item_id),
            encounter_window,
        ) {
            Ok(report) => {
                if report.transferred_items > 0 {
                    transferred_any = true;
                }
                trace_requested |= report.trace_requested_by_peer;
                trace_receipted |= report.trace_receipted_by_peer;

                if trace_receipted || trace_requested {
                    let finished_at_ms = now_unix_ms();
                    write_json_artifact(
                        &config.artifact_dir,
                        "sender-result.json",
                        &json!({
                            "result": "success",
                            "wayfarer_id": identity.wayfarer_id,
                            "relay_ws": config.relay_ws,
                            "to_wayfarer_id": config.to_wayfarer_id,
                            "message": config.message,
                            "item_id": item_id,
                            "attempts": attempts,
                            "transferred_any": transferred_any,
                            "trace_requested_by_peer": trace_requested,
                            "trace_receipted_by_peer": trace_receipted,
                            "started_at_unix_ms": started_at_ms,
                            "finished_at_unix_ms": finished_at_ms,
                            "elapsed_ms": finished_at_ms.saturating_sub(started_at_ms),
                        }),
                    )?;
                    println!(
                        "send_success item_id={} attempts={} trace_requested_by_peer={} trace_receipted_by_peer={}",
                        item_id, attempts, trace_requested, trace_receipted
                    );
                    return Ok(());
                }
            }
            Err(err) => {
                last_error = Some(err);
            }
        }

        thread::sleep(Duration::from_millis(250));
    }

    let finished_at_ms = now_unix_ms();
    write_json_artifact(
        &config.artifact_dir,
        "sender-result.json",
        &json!({
            "result": "timeout",
            "wayfarer_id": identity.wayfarer_id,
            "relay_ws": config.relay_ws,
            "to_wayfarer_id": config.to_wayfarer_id,
            "message": config.message,
            "item_id": item_id,
            "attempts": attempts,
            "transferred_any": transferred_any,
            "trace_requested_by_peer": trace_requested,
            "trace_receipted_by_peer": trace_receipted,
            "started_at_unix_ms": started_at_ms,
            "finished_at_unix_ms": finished_at_ms,
            "elapsed_ms": finished_at_ms.saturating_sub(started_at_ms),
            "last_error": last_error,
        }),
    )?;

    Err(format!(
        "timed out waiting for transfer/receipt signals after {} seconds",
        config.timeout.as_secs()
    ))
}

fn parse_args() -> Result<SenderConfig, String> {
    let mut relay_ws: Option<String> = None;
    let mut to_wayfarer_id: Option<String> = None;
    let mut message: Option<String> = None;
    let mut timeout_seconds = DEFAULT_TIMEOUT_SECONDS;
    let mut encounter_seconds = DEFAULT_ENCOUNTER_SECONDS;
    let mut ttl_seconds = DEFAULT_TTL_SECONDS;
    let mut artifact_dir = PathBuf::from("tests/artifacts/interop");
    let mut auth_token: Option<String> = std::env::var("AETHOS_RELAY_AUTH_TOKEN").ok();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--relay" => {
                relay_ws = Some(
                    args.next()
                        .ok_or_else(|| "missing value for --relay".to_string())?,
                )
            }
            "--to" => {
                to_wayfarer_id = Some(
                    args.next()
                        .ok_or_else(|| "missing value for --to".to_string())?,
                )
            }
            "--message" => {
                message = Some(
                    args.next()
                        .ok_or_else(|| "missing value for --message".to_string())?,
                )
            }
            "--timeout-seconds" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "missing value for --timeout-seconds".to_string())?;
                timeout_seconds = raw
                    .parse::<u64>()
                    .map_err(|err| format!("invalid --timeout-seconds '{raw}': {err}"))?;
            }
            "--encounter-seconds" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "missing value for --encounter-seconds".to_string())?;
                encounter_seconds = raw
                    .parse::<u64>()
                    .map_err(|err| format!("invalid --encounter-seconds '{raw}': {err}"))?;
            }
            "--ttl-seconds" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "missing value for --ttl-seconds".to_string())?;
                ttl_seconds = raw
                    .parse::<u64>()
                    .map_err(|err| format!("invalid --ttl-seconds '{raw}': {err}"))?;
            }
            "--artifact-dir" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "missing value for --artifact-dir".to_string())?;
                artifact_dir = PathBuf::from(raw);
            }
            "--auth-token" => {
                auth_token = Some(
                    args.next()
                        .ok_or_else(|| "missing value for --auth-token".to_string())?,
                )
            }
            "--help" | "-h" => return Err(usage()),
            _ => return Err(format!("unknown argument: {arg}\n{}", usage())),
        }
    }

    let relay_ws = relay_ws.ok_or_else(|| format!("missing required --relay\n{}", usage()))?;
    let to_wayfarer_id =
        to_wayfarer_id.ok_or_else(|| format!("missing required --to\n{}", usage()))?;
    let message = message.ok_or_else(|| format!("missing required --message\n{}", usage()))?;

    if timeout_seconds == 0 {
        return Err("--timeout-seconds must be > 0".to_string());
    }
    if encounter_seconds == 0 {
        return Err("--encounter-seconds must be > 0".to_string());
    }
    if ttl_seconds == 0 {
        return Err("--ttl-seconds must be > 0".to_string());
    }
    if message.trim().is_empty() {
        return Err("--message must not be empty".to_string());
    }

    Ok(SenderConfig {
        relay_ws,
        to_wayfarer_id,
        message,
        timeout: Duration::from_secs(timeout_seconds),
        encounter_window: Duration::from_secs(encounter_seconds),
        ttl_seconds,
        artifact_dir,
        auth_token,
    })
}

fn usage() -> String {
    "usage: cargo run --bin relay_interop_sender -- --relay <ws://host:port/ws> --to <target-wayfarer-id> --message <text> [--timeout-seconds <n>] [--encounter-seconds <n>] [--ttl-seconds <n>] [--artifact-dir <dir>] [--auth-token <token>]".to_string()
}

fn write_json_artifact(
    artifact_dir: &Path,
    file_name: &str,
    value: &serde_json::Value,
) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| format!("failed to serialize artifact {file_name}: {err}"))?;
    let path = artifact_dir.join(file_name);
    fs::write(&path, bytes)
        .map_err(|err| format!("failed to write artifact {}: {err}", path.display()))
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
