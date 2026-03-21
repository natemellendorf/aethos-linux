use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

const APP_LOG_FILE_NAME: &str = "aethos-linux.log";

static VERBOSE_LOGGING_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_verbose_logging_enabled(enabled: bool) {
    VERBOSE_LOGGING_ENABLED.store(enabled, Ordering::SeqCst);
}

pub fn verbose_logging_enabled() -> bool {
    VERBOSE_LOGGING_ENABLED.load(Ordering::SeqCst)
}

pub fn log_info(message: &str) {
    if let Err(err) = append_local_log_inner(message) {
        eprintln!("local log warning: {err}");
    }
}

pub fn log_verbose(message: &str) {
    if verbose_logging_enabled() {
        log_info(message);
    }
}

pub fn app_log_file_path() -> PathBuf {
    if let Ok(xdg_state_home) = std::env::var("XDG_STATE_HOME") {
        if !xdg_state_home.trim().is_empty() {
            return Path::new(&xdg_state_home)
                .join("aethos-linux")
                .join(APP_LOG_FILE_NAME);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        return Path::new(&home)
            .join(".local")
            .join("state")
            .join("aethos-linux")
            .join(APP_LOG_FILE_NAME);
    }

    std::env::temp_dir().join(APP_LOG_FILE_NAME)
}

fn append_local_log_inner(message: &str) -> Result<(), String> {
    let path = app_log_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed creating app log directory: {err}"))?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| format!("failed opening app log file at {}: {err}", path.display()))?;

    let now = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(_) => 0,
    };

    if structured_logs_enabled() {
        let event = json!({
            "ts_unix": now,
            "run_id": std::env::var("AETHOS_E2E_RUN_ID").ok(),
            "test_case_id": std::env::var("AETHOS_E2E_TEST_CASE_ID").ok(),
            "scenario": std::env::var("AETHOS_E2E_SCENARIO").ok(),
            "node_label": std::env::var("AETHOS_E2E_NODE_LABEL").ok(),
            "message": message,
            "event": infer_event_name(message),
            "fields": extract_kv_fields(message),
        });
        writeln!(file, "{}", event)
            .map_err(|err| format!("failed writing app log file at {}: {err}", path.display()))
    } else {
        writeln!(file, "[{now}] {message}")
            .map_err(|err| format!("failed writing app log file at {}: {err}", path.display()))
    }
}

fn structured_logs_enabled() -> bool {
    std::env::var("AETHOS_STRUCTURED_LOGS")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn infer_event_name(message: &str) -> String {
    let token = message.split_whitespace().next().unwrap_or("log");
    token.trim_end_matches(':').to_string()
}

fn extract_kv_fields(message: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for token in message.split_whitespace() {
        if let Some((key, value)) = token.split_once('=') {
            let clean_key = key.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_');
            let clean_value = value
                .trim_matches(|ch: char| matches!(ch, ',' | ';' | ')' | '(' | '[' | ']' | '"'));
            if !clean_key.is_empty() && !clean_value.is_empty() {
                map.insert(clean_key.to_string(), json!(clean_value));
            }
        }
    }
    serde_json::Value::Object(map)
}
