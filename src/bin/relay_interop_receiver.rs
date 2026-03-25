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

use aethos_core::identity_store::ensure_local_identity;
use relay::client::run_relay_encounter_gossipv1_for_duration;

const DEFAULT_TIMEOUT_SECONDS: u64 = 45;
const DEFAULT_POLL_INTERVAL_MS: u64 = 350;

#[derive(Debug, Clone)]
struct ReceiverConfig {
    relay_ws: String,
    marker: String,
    timeout: Duration,
    poll_interval: Duration,
    artifact_dir: PathBuf,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("interop_receiver_error: {err}");
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
    println!("wayfarer_id={}", identity.wayfarer_id);
    println!("relay_ws={}", config.relay_ws);
    println!("marker={}", config.marker);

    let started_at_ms = now_unix_ms();
    write_json_artifact(
        &config.artifact_dir,
        "receiver-info.json",
        &json!({
            "wayfarer_id": identity.wayfarer_id,
            "relay_ws": config.relay_ws,
            "marker": config.marker,
            "started_at_unix_ms": started_at_ms,
            "timeout_seconds": config.timeout.as_secs(),
            "poll_interval_ms": config.poll_interval.as_millis(),
        }),
    )?;

    let deadline = Instant::now() + config.timeout;
    let mut attempts = 0usize;
    let mut observed_messages = 0usize;
    let mut last_error: Option<String> = None;

    while Instant::now() < deadline {
        attempts = attempts.saturating_add(1);
        let remaining = deadline.saturating_duration_since(Instant::now());
        let encounter_window = remaining.min(Duration::from_secs(3));

        match run_relay_encounter_gossipv1_for_duration(
            &config.relay_ws,
            &identity,
            None,
            None,
            encounter_window,
        ) {
            Ok(report) => {
                observed_messages = observed_messages.saturating_add(report.pulled_messages.len());

                if let Some(found) = report
                    .pulled_messages
                    .iter()
                    .find(|message| message.text.contains(&config.marker))
                {
                    let finished_at_ms = now_unix_ms();
                    println!(
                        "marker_received item_id={} attempts={} observed_messages={}",
                        found.item_id, attempts, observed_messages
                    );
                    write_json_artifact(
                        &config.artifact_dir,
                        "receiver-result.json",
                        &json!({
                            "result": "success",
                            "wayfarer_id": identity.wayfarer_id,
                            "relay_ws": config.relay_ws,
                            "marker": config.marker,
                            "matched_item_id": found.item_id,
                            "matched_text": found.text,
                            "attempts": attempts,
                            "observed_messages": observed_messages,
                            "started_at_unix_ms": started_at_ms,
                            "finished_at_unix_ms": finished_at_ms,
                            "elapsed_ms": finished_at_ms.saturating_sub(started_at_ms),
                        }),
                    )?;
                    return Ok(());
                }
            }
            Err(err) => {
                last_error = Some(err);
            }
        }

        let sleep_for = config
            .poll_interval
            .min(deadline.saturating_duration_since(Instant::now()));
        if sleep_for.is_zero() {
            break;
        }
        thread::sleep(sleep_for);
    }

    let finished_at_ms = now_unix_ms();
    write_json_artifact(
        &config.artifact_dir,
        "receiver-result.json",
        &json!({
            "result": "timeout",
            "wayfarer_id": identity.wayfarer_id,
            "relay_ws": config.relay_ws,
            "marker": config.marker,
            "attempts": attempts,
            "observed_messages": observed_messages,
            "started_at_unix_ms": started_at_ms,
            "finished_at_unix_ms": finished_at_ms,
            "elapsed_ms": finished_at_ms.saturating_sub(started_at_ms),
            "last_error": last_error,
        }),
    )?;

    Err(format!(
        "timed out waiting for marker after {} seconds",
        config.timeout.as_secs()
    ))
}

fn parse_args() -> Result<ReceiverConfig, String> {
    let mut relay_ws: Option<String> = None;
    let mut marker: Option<String> = None;
    let mut timeout_seconds = DEFAULT_TIMEOUT_SECONDS;
    let mut poll_interval_ms = DEFAULT_POLL_INTERVAL_MS;
    let mut artifact_dir = PathBuf::from("tests/artifacts/interop");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--relay" => {
                relay_ws = Some(
                    args.next()
                        .ok_or_else(|| "missing value for --relay".to_string())?,
                );
            }
            "--marker" => {
                marker = Some(
                    args.next()
                        .ok_or_else(|| "missing value for --marker".to_string())?,
                );
            }
            "--timeout-seconds" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "missing value for --timeout-seconds".to_string())?;
                timeout_seconds = raw
                    .parse::<u64>()
                    .map_err(|err| format!("invalid --timeout-seconds '{raw}': {err}"))?;
            }
            "--poll-interval-ms" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "missing value for --poll-interval-ms".to_string())?;
                poll_interval_ms = raw
                    .parse::<u64>()
                    .map_err(|err| format!("invalid --poll-interval-ms '{raw}': {err}"))?;
            }
            "--artifact-dir" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "missing value for --artifact-dir".to_string())?;
                artifact_dir = PathBuf::from(raw);
            }
            "--help" | "-h" => {
                return Err(usage());
            }
            _ => {
                return Err(format!("unknown argument: {arg}\n{}", usage()));
            }
        }
    }

    let relay_ws = relay_ws.ok_or_else(|| format!("missing required --relay\n{}", usage()))?;
    let marker = marker.ok_or_else(|| format!("missing required --marker\n{}", usage()))?;

    if marker.trim().is_empty() {
        return Err("--marker must not be empty".to_string());
    }

    if timeout_seconds == 0 {
        return Err("--timeout-seconds must be > 0".to_string());
    }

    if poll_interval_ms == 0 {
        return Err("--poll-interval-ms must be > 0".to_string());
    }

    Ok(ReceiverConfig {
        relay_ws,
        marker,
        timeout: Duration::from_secs(timeout_seconds),
        poll_interval: Duration::from_millis(poll_interval_ms),
        artifact_dir,
    })
}

fn usage() -> String {
    "usage: cargo run --bin relay_interop_receiver -- --relay <ws://host:port/ws> --marker <unique-marker> [--timeout-seconds <n>] [--poll-interval-ms <n>] [--artifact-dir <dir>]".to_string()
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
