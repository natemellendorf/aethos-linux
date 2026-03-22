#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app_state;
mod media_v1;

#[allow(dead_code)]
#[path = "../../../../src/aethos_core/mod.rs"]
mod aethos_core;
#[allow(dead_code)]
#[path = "../../../../src/relay/mod.rs"]
mod relay;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use app_state::{
    load_app_settings, load_chat_state, normalize_chat_state, now_unix_ms, now_unix_secs,
    save_app_settings, save_chat_state, AppSettings, ChatAttachment, ChatDirection,
    ChatMediaTransfer, ChatMessage, MediaTransferStatus, OutboundState, PersistedChatState,
};
use base64::Engine;
use image::{imageops::FilterType, ImageBuffer, Luma, Rgba, RgbaImage};
use qrcode::QrCode;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use socket2::{Domain, Protocol, Socket, Type};
use tauri::Emitter;
use url::Url;

use crate::aethos_core::gossip_sync::record_local_payload as gossip_record_local_payload;
use crate::aethos_core::gossip_sync::{
    build_hello_frame as build_gossip_hello_frame,
    build_relay_ingest_frame as build_gossip_relay_ingest_frame,
    build_request_frame as build_gossip_request_frame,
    build_summary_frame as build_gossip_summary_frame, has_item as gossip_has_item,
    import_transfer_items, parse_frame as parse_gossip_frame,
    select_request_item_ids_from_summary as gossip_select_request_item_ids_from_summary,
    serialize_frame as serialize_gossip_frame, transfer_items_for_request as gossip_transfer_items,
    GossipSyncFrame, ReceiptFrame, GOSSIP_LAN_PORT, MAX_FRAME_BYTES, MAX_TRANSFER_BYTES,
    MAX_TRANSFER_ITEMS,
};
use crate::aethos_core::identity_store::{
    delete_wayfarer_id, ensure_local_identity, load_contact_aliases, load_local_signing_key_seed,
    regenerate_local_identity, save_contact_aliases,
};
use crate::aethos_core::logging::{
    app_log_file_path, log_info, log_verbose, set_verbose_logging_enabled, verbose_logging_enabled,
};
use crate::aethos_core::protocol::{
    build_envelope_payload_b64_from_utf8, bytes_to_hex_lower, decode_envelope_payload_b64,
    is_valid_wayfarer_id,
};
use crate::relay::client::{
    connect_to_relay_gossipv1_with_auth, normalize_http_endpoint, relay_session_snapshot,
    run_relay_encounter_gossipv1, run_relay_encounter_gossipv1_for_duration, to_ws_endpoint,
};
use media_v1::{
    get_completed_media_path as media_get_completed_media_path, is_media_candidate_attachment,
    is_media_wire_message, load_completed_media as media_load_completed_media,
    max_object_bytes as media_max_object_bytes, maybe_queue_capabilities_for_peer,
    process_incoming_media_message, run_housekeeping_tick as media_run_housekeeping_tick,
    send_media_manifest_and_chunks, MediaMessageProcess,
};

const SHARE_QR_FILE_NAME: &str = "share-wayfarer-qr.png";
const CHAT_SNAPSHOT_EVENT: &str = "chat_snapshot";
const SOUND_EVENT: &str = "sound_event";
const MAX_INLINE_ATTACHMENT_BYTES: u64 = 2 * 1024 * 1024;
const LAN_TCP_CAPABILITY: &str = "lan_tcp_transfer_v1";
const LAN_TCP_FAILURE_COOLDOWN_SECS: u64 = 45;
const LAN_FALLBACK_TRANSFER_MAX_ITEMS: u32 = 2;
const LAN_FALLBACK_TRANSFER_MAX_BYTES: u64 = 1024;
const LAN_DUP_REQUEST_COOLDOWN_MS: u64 = 700;
const LAN_FALLBACK_CHUNK_PACING_MS: u64 = 3;
const LAN_OUTBOUND_REQUEST_DEBOUNCE_MS: u64 = 700;

fn gossip_lan_port() -> u16 {
    std::env::var("AETHOS_GOSSIP_LAN_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(GOSSIP_LAN_PORT)
}

fn gossip_peer_ports() -> Vec<u16> {
    let mut ports = vec![gossip_lan_port()];
    if let Ok(raw) = std::env::var("AETHOS_GOSSIP_PEER_PORTS") {
        for part in raw.split(',') {
            if let Ok(port) = part.trim().parse::<u16>() {
                if port > 0 {
                    ports.push(port);
                }
            }
        }
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn lan_tcp_disabled() -> bool {
    std::env::var("AETHOS_DISABLE_LAN_TCP")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn gossip_localhost_fanout_enabled() -> bool {
    std::env::var("AETHOS_GOSSIP_LOCALHOST_FANOUT")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn gossip_eager_unicast_enabled() -> bool {
    std::env::var("AETHOS_GOSSIP_EAGER_UNICAST")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn gossip_loopback_only_enabled() -> bool {
    std::env::var("AETHOS_GOSSIP_LOOPBACK_ONLY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

struct GossipRuntime {
    enabled: AtomicBool,
    running: AtomicBool,
    last_activity_ms: AtomicU64,
    force_announce: AtomicBool,
    last_event: Mutex<String>,
}

impl GossipRuntime {
    fn new(initial_enabled: bool) -> Self {
        Self {
            enabled: AtomicBool::new(initial_enabled),
            running: AtomicBool::new(false),
            last_activity_ms: AtomicU64::new(0),
            force_announce: AtomicBool::new(initial_enabled),
            last_event: Mutex::new("idle".to_string()),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GossipStatus {
    enabled: bool,
    running: bool,
    last_activity_ms: u64,
    last_event: String,
}

static GOSSIP_RUNTIME: OnceLock<GossipRuntime> = OnceLock::new();
static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();
static RELAY_WORKER_RUNTIME: OnceLock<RelayWorkerRuntime> = OnceLock::new();

struct RelayWorkerRuntime {
    running: AtomicBool,
    command_tx: Mutex<Option<Sender<RelayWorkerCommand>>>,
    sync_epoch: AtomicU64,
    last_status: Mutex<String>,
    last_activity_ms: AtomicU64,
}

enum RelayWorkerCommand {
    SyncNow(&'static str),
}

impl RelayWorkerRuntime {
    fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            command_tx: Mutex::new(None),
            sync_epoch: AtomicU64::new(1),
            last_status: Mutex::new("idle".to_string()),
            last_activity_ms: AtomicU64::new(0),
        }
    }
}

fn relay_worker_runtime() -> &'static RelayWorkerRuntime {
    RELAY_WORKER_RUNTIME.get_or_init(RelayWorkerRuntime::new)
}

fn set_relay_worker_status(status: &str) {
    let runtime = relay_worker_runtime();
    if let Ok(mut slot) = runtime.last_status.lock() {
        *slot = status.to_string();
    }
    runtime
        .last_activity_ms
        .store(now_unix_ms(), Ordering::SeqCst);
}

fn request_relay_sync(reason: &str) {
    let runtime = relay_worker_runtime();
    runtime.sync_epoch.fetch_add(1, Ordering::SeqCst);
    if let Ok(slot) = runtime.command_tx.lock() {
        if let Some(tx) = slot.as_ref() {
            let command_reason = match reason {
                "bootstrap_state" => "bootstrap_state",
                "settings_updated" => "settings_updated",
                "send_message" => "send_message",
                "sync_inbox_command" => "sync_inbox_command",
                "relay_diagnostics" => "relay_diagnostics",
                _ => "manual",
            };
            let _ = tx.send(RelayWorkerCommand::SyncNow(command_reason));
        }
    }
    log_verbose(&format!("relay_worker_sync_requested: reason={reason}"));
}

fn relay_worker_wait_for_command(
    rx: &Receiver<RelayWorkerCommand>,
    timeout: Duration,
) -> Option<RelayWorkerCommand> {
    match rx.recv_timeout(timeout) {
        Ok(command) => Some(command),
        Err(mpsc::RecvTimeoutError::Timeout) => None,
        Err(mpsc::RecvTimeoutError::Disconnected) => None,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppDiagnostics {
    app: &'static str,
    version: &'static str,
    profile: &'static str,
    platform: &'static str,
    arch: &'static str,
    verbose_logging_enabled: bool,
    log_file_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IdentityView {
    wayfarer_id: String,
    device_id: String,
    verifying_key_b64: String,
    device_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapState {
    identity: IdentityView,
    settings: AppSettings,
    contacts: BTreeMap<String, String>,
    chat: PersistedChatState,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatSnapshot {
    contacts: BTreeMap<String, String>,
    chat: PersistedChatState,
}

#[derive(Clone, Debug, Serialize)]
struct SoundEventPayload {
    kind: String,
}

#[tauri::command]
fn app_version() -> String {
    embedded_release_version()
}

#[tauri::command]
fn open_external_url(url: String) -> Result<(), String> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err("only http(s) URLs are allowed".to_string());
    }
    webbrowser::open(trimmed).map_err(|err| format!("failed opening URL: {err}"))
}

fn embedded_release_version() -> String {
    let source = include_str!("../../../../Cargo.toml");
    source
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if !line.starts_with("version") {
                return None;
            }
            let value = line.split('=').nth(1)?.trim().trim_matches('"').to_string();
            if value.is_empty() {
                None
            } else {
                Some(value)
            }
        })
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpsertContactRequest {
    wayfarer_id: String,
    alias: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageRequest {
    wayfarer_id: String,
    body: String,
    #[serde(default)]
    attachment: Option<SendAttachmentRequest>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendAttachmentRequest {
    file_name: String,
    mime_type: String,
    size_bytes: u64,
    #[serde(default)]
    content_b64: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadCompletedMediaRequest {
    object_sha256_hex: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateE2ELargeImageRequest {
    seed: String,
    width: u32,
    height: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerateE2ELargeImageResponse {
    file_path: String,
    size_bytes: u64,
    sha256_hex: String,
    width: u32,
    height: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageResponse {
    message: ChatMessage,
    chat: PersistedChatState,
    contacts: BTreeMap<String, String>,
    encounter_status: String,
    pulled_messages: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncInboxResponse {
    chat: PersistedChatState,
    contacts: BTreeMap<String, String>,
    pulled_messages: usize,
    status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ShareQrResponse {
    wayfarer_id: String,
    file_path: String,
    png_base64: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelayDiagnosticsRequest {
    relay_endpoints: Vec<String>,
    auth_token: Option<String>,
    trace_item_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayEndpointDiagnostics {
    relay_http: String,
    relay_ws: String,
    handshake_status: String,
    encounter_status: String,
    transferred_items: usize,
    pulled_messages: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayHealthStatus {
    primary_status: String,
    secondary_status: String,
    chip_text: String,
    chip_state: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppLogTail {
    log_file_path: String,
    total_lines: usize,
    shown_lines: usize,
    content: String,
}

#[tauri::command]
fn app_diagnostics() -> AppDiagnostics {
    AppDiagnostics {
        app: "aethos-tauri-desktop",
        version: env!("CARGO_PKG_VERSION"),
        profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        platform: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        verbose_logging_enabled: verbose_logging_enabled(),
        log_file_path: app_log_file_path().display().to_string(),
    }
}

#[tauri::command]
fn read_app_log(max_lines: Option<usize>) -> Result<AppLogTail, String> {
    let limit = max_lines.unwrap_or(400).clamp(50, 5000);
    let path = app_log_file_path();
    let raw = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(format!(
                "failed reading app log at {}: {err}",
                path.display()
            ))
        }
    };

    let all_lines = raw.lines().collect::<Vec<_>>();
    let total_lines = all_lines.len();
    let tail_start = total_lines.saturating_sub(limit);
    let shown = all_lines[tail_start..].join("\n");

    Ok(AppLogTail {
        log_file_path: path.display().to_string(),
        total_lines,
        shown_lines: total_lines.saturating_sub(tail_start),
        content: shown,
    })
}

#[tauri::command]
fn clear_app_log() -> Result<AppLogTail, String> {
    let path = app_log_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed creating app log directory {}: {err}",
                parent.display()
            )
        })?;
    }
    fs::write(&path, "")
        .map_err(|err| format!("failed clearing app log at {}: {err}", path.display()))?;
    read_app_log(Some(500))
}

#[tauri::command]
fn bootstrap_state() -> Result<BootstrapState, String> {
    let mut settings = load_app_settings()?;
    let e2e_disable_relay = std::env::var("AETHOS_E2E_DISABLE_RELAY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let e2e_force_verbose = std::env::var("AETHOS_E2E_FORCE_VERBOSE")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let e2e_force_gossip = std::env::var("AETHOS_E2E_FORCE_GOSSIP")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let mut settings_changed = false;
    if e2e_disable_relay && settings.relay_sync_enabled {
        settings.relay_sync_enabled = false;
        settings_changed = true;
    }
    if e2e_force_verbose && !settings.verbose_logging_enabled {
        settings.verbose_logging_enabled = true;
        settings_changed = true;
    }
    if e2e_force_gossip && !settings.gossip_sync_enabled {
        settings.gossip_sync_enabled = true;
        settings_changed = true;
    }

    if !settings.gossip_sync_enabled {
        settings.gossip_sync_enabled = true;
        settings_changed = true;
    }

    if settings_changed {
        settings = save_app_settings(&settings)?;
    }
    set_verbose_logging_enabled(settings.verbose_logging_enabled);
    start_gossip_worker_if_needed(settings.gossip_sync_enabled);
    set_gossip_enabled(settings.gossip_sync_enabled);
    start_relay_worker_if_needed();
    if settings.relay_sync_enabled {
        request_relay_sync("bootstrap_state");
    }
    run_media_housekeeping_best_effort("bootstrap_state", settings.message_ttl_seconds);
    let identity = ensure_local_identity()?;
    let contacts = load_contact_aliases()?;
    let author_signing_seed = load_local_signing_key_seed().ok();
    if let Some(seed) = author_signing_seed.as_ref() {
        for wayfarer_id in contacts.keys() {
            let _ =
                maybe_queue_capabilities_for_peer(wayfarer_id, seed, settings.message_ttl_seconds);
        }
    }
    let chat = load_chat_state()?;

    Ok(BootstrapState {
        identity: IdentityView {
            wayfarer_id: identity.wayfarer_id,
            device_id: identity.device_id,
            verifying_key_b64: identity.verifying_key_b64,
            device_name: identity.device_name,
        },
        settings,
        contacts,
        chat,
    })
}

#[tauri::command]
fn rotate_wayfarer_id() -> Result<IdentityView, String> {
    let identity = regenerate_local_identity()?;
    Ok(IdentityView {
        wayfarer_id: identity.wayfarer_id,
        device_id: identity.device_id,
        verifying_key_b64: identity.verifying_key_b64,
        device_name: identity.device_name,
    })
}

#[tauri::command]
fn reset_wayfarer_id() -> Result<IdentityView, String> {
    delete_wayfarer_id()?;
    rotate_wayfarer_id()
}

#[tauri::command]
fn update_settings(settings: AppSettings) -> Result<AppSettings, String> {
    let saved = save_app_settings(&settings)?;
    set_verbose_logging_enabled(saved.verbose_logging_enabled);
    start_gossip_worker_if_needed(saved.gossip_sync_enabled);
    set_gossip_enabled(saved.gossip_sync_enabled);
    request_relay_sync("settings_updated");
    Ok(saved)
}

#[tauri::command]
fn gossip_status() -> GossipStatus {
    let ttl_seconds = load_app_settings()
        .map(|settings| settings.message_ttl_seconds)
        .unwrap_or(3600);
    run_media_housekeeping_best_effort("gossip_status", ttl_seconds);
    current_gossip_status()
}

#[tauri::command]
fn gossip_announce_now() -> GossipStatus {
    if let Some(runtime) = GOSSIP_RUNTIME.get() {
        runtime.force_announce.store(true, Ordering::SeqCst);
        set_gossip_event("announce queued");
    }
    current_gossip_status()
}

#[tauri::command]
async fn relay_health_status() -> Result<RelayHealthStatus, String> {
    tauri::async_runtime::spawn_blocking(relay_health_status_blocking)
        .await
        .map_err(|err| format!("relay_health_status task join failed: {err}"))?
}

fn relay_health_status_blocking() -> Result<RelayHealthStatus, String> {
    let settings = load_app_settings()?;
    if !settings.relay_sync_enabled {
        return Ok(RelayHealthStatus {
            primary_status: "relay sync disabled in settings".to_string(),
            secondary_status: "relay sync disabled in settings".to_string(),
            chip_text: "Relays: disabled".to_string(),
            chip_state: "disabled".to_string(),
        });
    }

    if settings.relay_endpoints.is_empty() {
        return Ok(RelayHealthStatus {
            primary_status: "no relay configured".to_string(),
            secondary_status: "no relay configured".to_string(),
            chip_text: "Relays: none configured (0/0)".to_string(),
            chip_state: "idle".to_string(),
        });
    }

    let identity = ensure_local_identity()?;
    let primary_endpoint = settings
        .relay_endpoints
        .first()
        .map(|value| to_ws_endpoint(&normalize_http_endpoint(value)));
    let secondary_endpoint = settings
        .relay_endpoints
        .get(1)
        .map(|value| to_ws_endpoint(&normalize_http_endpoint(value)));

    let worker_status = relay_worker_runtime()
        .last_status
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|_| "status unavailable".to_string());

    let primary_status = primary_endpoint
        .map(|relay_ws| {
            if let Some(active) = relay_session_snapshot(&relay_ws, &identity.wayfarer_id) {
                return format!(
                    "active relay session (attempt_id={} state={} trigger={} age_ms={})",
                    active.attempt_id, active.state, active.trigger, active.age_ms
                );
            }
            format!("idle (worker={worker_status})")
        })
        .unwrap_or_else(|| "idle".to_string());
    let secondary_status = secondary_endpoint
        .map(|relay_ws| {
            if let Some(active) = relay_session_snapshot(&relay_ws, &identity.wayfarer_id) {
                return format!(
                    "active relay session (attempt_id={} state={} trigger={} age_ms={})",
                    active.attempt_id, active.state, active.trigger, active.age_ms
                );
            }
            format!("idle (worker={worker_status})")
        })
        .unwrap_or_else(|| "idle".to_string());

    let primary_ok = primary_status.contains("connected + HELLO")
        || primary_status.contains("active relay session");
    let secondary_ok = secondary_status.contains("connected + HELLO")
        || secondary_status.contains("active relay session");
    let (chip_text, chip_state) = match (primary_ok, secondary_ok) {
        (true, true) => ("Relays: healthy (2/2)".to_string(), "ok".to_string()),
        (true, false) | (false, true) => ("Relays: degraded (1/2)".to_string(), "warn".to_string()),
        (false, false) => {
            let has_any_result = primary_status != "idle" || secondary_status != "idle";
            if has_any_result {
                ("Relays: unavailable (0/2)".to_string(), "down".to_string())
            } else {
                ("Relays: idle".to_string(), "idle".to_string())
            }
        }
    };

    Ok(RelayHealthStatus {
        primary_status,
        secondary_status,
        chip_text,
        chip_state,
    })
}

#[tauri::command]
fn upsert_contact(request: UpsertContactRequest) -> Result<BTreeMap<String, String>, String> {
    if !is_valid_wayfarer_id(&request.wayfarer_id) {
        return Err("invalid wayfarer_id; expected 64 lowercase hex chars".to_string());
    }

    let wayfarer_id = request.wayfarer_id.trim().to_string();
    let alias = request.alias.trim();
    if alias.is_empty() {
        return Err("alias cannot be empty".to_string());
    }

    let mut contacts = load_contact_aliases()?;
    contacts.insert(wayfarer_id.clone(), alias.to_string());
    save_contact_aliases(&contacts)?;
    if let Ok(seed) = load_local_signing_key_seed() {
        let ttl_seconds = load_app_settings()
            .map(|settings| settings.message_ttl_seconds)
            .unwrap_or(3600);
        let _ = maybe_queue_capabilities_for_peer(&wayfarer_id, &seed, ttl_seconds);
    }
    emit_chat_snapshot_event_best_effort("upsert_contact");
    Ok(contacts)
}

#[tauri::command]
fn remove_contact(wayfarer_id: String) -> Result<BTreeMap<String, String>, String> {
    let mut contacts = load_contact_aliases()?;
    contacts.remove(wayfarer_id.trim());
    save_contact_aliases(&contacts)?;
    emit_chat_snapshot_event_best_effort("remove_contact");
    Ok(contacts)
}

#[tauri::command]
fn save_chat(chat: PersistedChatState) -> Result<PersistedChatState, String> {
    let ttl_seconds = load_app_settings()
        .map(|settings| settings.message_ttl_seconds)
        .unwrap_or(3600);
    run_media_housekeeping_best_effort("save_chat", ttl_seconds);

    let mut normalized = chat;
    normalize_chat_state(&mut normalized);
    save_chat_state(&normalized)?;
    emit_chat_snapshot_event_best_effort("save_chat");
    Ok(normalized)
}

#[tauri::command]
fn chat_snapshot() -> Result<ChatSnapshot, String> {
    let ttl_seconds = load_app_settings()
        .map(|settings| settings.message_ttl_seconds)
        .unwrap_or(3600);
    run_media_housekeeping_best_effort("chat_snapshot", ttl_seconds);

    Ok(ChatSnapshot {
        contacts: load_contact_aliases()?,
        chat: load_chat_state()?,
    })
}

#[tauri::command]
fn load_completed_media(
    request: LoadCompletedMediaRequest,
) -> Result<media_v1::CompletedMediaPayload, String> {
    media_load_completed_media(request.object_sha256_hex.trim())
}

#[tauri::command]
fn get_completed_media_path(
    request: LoadCompletedMediaRequest,
) -> Result<media_v1::CompletedMediaPathPayload, String> {
    media_get_completed_media_path(request.object_sha256_hex.trim())
}

#[tauri::command]
fn open_completed_media_in_system_viewer(request: LoadCompletedMediaRequest) -> Result<(), String> {
    let payload = media_get_completed_media_path(request.object_sha256_hex.trim())?;
    let file_url = Url::from_file_path(Path::new(&payload.path))
        .map_err(|_| "failed building file URL for completed media path".to_string())?;
    webbrowser::open(file_url.as_str())
        .map_err(|err| format!("failed opening completed media in system viewer: {err}"))?;
    Ok(())
}

#[tauri::command]
fn generate_e2e_large_image(
    request: GenerateE2ELargeImageRequest,
) -> Result<GenerateE2ELargeImageResponse, String> {
    let width = request.width.clamp(256, 4096);
    let height = request.height.clamp(256, 4096);
    let seed = request.seed.trim();
    if seed.is_empty() {
        return Err("seed cannot be empty".to_string());
    }

    let output_dir = if let Ok(artifact_dir) = std::env::var("AETHOS_E2E_ARTIFACT_DIR") {
        if artifact_dir.trim().is_empty() {
            std::env::temp_dir().join("aethos-e2e")
        } else {
            PathBuf::from(artifact_dir)
        }
    } else {
        std::env::temp_dir().join("aethos-e2e")
    };
    fs::create_dir_all(&output_dir).map_err(|err| {
        format!(
            "failed creating e2e image output dir {}: {err}",
            output_dir.display()
        )
    })?;
    let file_name = format!(
        "aethos-large-{}-{}x{}.png",
        sanitize_seed_for_file(seed),
        width,
        height
    );
    let file_path = output_dir.join(file_name);

    let png_bytes = generate_aethos_e2e_png(seed, width, height)?;
    if png_bytes.len() as u64 > media_max_object_bytes() {
        return Err("generated e2e image exceeds media maxObjectBytes".to_string());
    }
    fs::write(&file_path, &png_bytes)
        .map_err(|err| format!("failed writing e2e image {}: {err}", file_path.display()))?;

    Ok(GenerateE2ELargeImageResponse {
        file_path: file_path.display().to_string(),
        size_bytes: png_bytes.len() as u64,
        sha256_hex: bytes_to_hex_lower(&sha2::Sha256::digest(&png_bytes)),
        width,
        height,
    })
}

#[tauri::command]
async fn send_message(request: SendMessageRequest) -> Result<SendMessageResponse, String> {
    tauri::async_runtime::spawn_blocking(move || send_message_blocking(request))
        .await
        .map_err(|err| format!("send_message task join failed: {err}"))?
}

fn send_message_blocking(request: SendMessageRequest) -> Result<SendMessageResponse, String> {
    let wayfarer_id = request.wayfarer_id.trim();
    if !is_valid_wayfarer_id(wayfarer_id) {
        return Err("invalid wayfarer_id; expected 64 lowercase hex chars".to_string());
    }

    let body = request.body.trim();
    let attachment = request
        .attachment
        .as_ref()
        .map(validate_send_attachment)
        .transpose()?;
    if body.is_empty() && attachment.is_none() {
        return Err("message must include text or a file attachment".to_string());
    }

    log_verbose(&format!(
        "send_message_start: to={} body_bytes={} attachment={}",
        wayfarer_id,
        body.len(),
        attachment
            .as_ref()
            .map(|value| format!("{}:{}", value.file_name, value.size_bytes))
            .unwrap_or_else(|| "none".to_string())
    ));

    let now_ms = now_unix_ms();
    let now_secs = now_unix_secs();
    let settings = load_app_settings()?;
    let identity = ensure_local_identity()?;
    let author_signing_seed = load_local_signing_key_seed()?;
    let contacts = load_contact_aliases()?;
    let _ = maybe_queue_capabilities_for_peer(
        wayfarer_id,
        &author_signing_seed,
        settings.message_ttl_seconds,
    );

    if let Some(media_attachment) = attachment
        .as_ref()
        .filter(|value| is_media_candidate_attachment(value))
    {
        let send_result = send_media_manifest_and_chunks(
            wayfarer_id,
            body,
            media_attachment,
            &author_signing_seed,
            settings.message_ttl_seconds,
        )?;

        if let Some(runtime) = GOSSIP_RUNTIME.get() {
            runtime.force_announce.store(true, Ordering::SeqCst);
            set_gossip_event("announce queued");
        }

        let mut chat = load_chat_state()?;
        chat.selected_contact = Some(wayfarer_id.to_string());
        mark_contact_seen(&mut chat, wayfarer_id);

        let thread = chat.threads.entry(wayfarer_id.to_string()).or_default();
        let local_id = format!("local-media-{now_ms}-{:08x}", rand::random::<u32>());
        thread.push(ChatMessage {
            msg_id: local_id.clone(),
            text: if body.is_empty() {
                format!("[Image] {}", send_result.file_name)
            } else {
                body.to_string()
            },
            timestamp: format_timestamp_from_unix(now_secs),
            created_at_unix: now_secs,
            created_at_unix_ms: now_ms,
            direction: ChatDirection::Outgoing,
            seen: true,
            manifest_id_hex: None,
            delivered_at: None,
            outbound_state: Some(OutboundState::Sending),
            expires_at_unix_ms: Some(send_result.expires_at_unix_ms),
            last_sync_attempt_unix_ms: Some(now_ms),
            last_sync_error: None,
            attachment: Some(ChatAttachment {
                file_name: send_result.file_name.clone(),
                mime_type: send_result.mime_type.clone(),
                size_bytes: send_result.total_bytes,
                content_b64: None,
            }),
            media: Some(ChatMediaTransfer {
                transfer_id: send_result.transfer_id.clone(),
                object_sha256_hex: send_result.object_sha256_hex.clone(),
                file_name: send_result.file_name.clone(),
                mime_type: send_result.mime_type.clone(),
                total_bytes: send_result.total_bytes,
                chunk_count: send_result.chunk_count,
                received_chunks: 0,
                received_bytes: 0,
                status: MediaTransferStatus::Pending,
                error: None,
                expires_at_unix_ms: Some(send_result.expires_at_unix_ms),
            }),
        });

        mark_outgoing_message(
            &mut chat,
            wayfarer_id,
            &local_id,
            &send_result.item_id,
            None,
        );
        save_chat_state(&chat)?;
        save_contact_aliases(&contacts)?;
        emit_chat_snapshot_event_best_effort("send_message_blocking_media");

        let message =
            latest_message_for_contact(&chat, wayfarer_id, &send_result.item_id, &local_id)
                .ok_or_else(|| "failed to load stored outbound media message".to_string())?;

        return Ok(SendMessageResponse {
            message,
            chat,
            contacts,
            encounter_status: "media manifest + chunks queued locally".to_string(),
            pulled_messages: 0,
        });
    }

    let expiry_ms = now_ms.saturating_add(settings.message_ttl_seconds.saturating_mul(1000));
    let local_id = format!("local-{now_ms}-{:08x}", rand::random::<u32>());
    let outbound_payload =
        build_outbound_chat_payload(body, &local_id, now_ms, attachment.as_ref());
    let payload =
        build_envelope_payload_b64_from_utf8(wayfarer_id, &outbound_payload, &author_signing_seed)?;
    let decoded = decode_envelope_payload_b64(&payload)?;
    let item_id = gossip_record_local_payload(&payload, expiry_ms)?;
    if let Some(runtime) = GOSSIP_RUNTIME.get() {
        runtime.force_announce.store(true, Ordering::SeqCst);
        set_gossip_event("announce queued");
    }
    log_info(&format!(
        "gossip_record_local_payload_ok: item_id={} to={} expiry_ms={}",
        item_id, wayfarer_id, expiry_ms
    ));
    log_verbose(&format!(
        "send_message_queue_saved: local_id={} item_id={} ttl_s={}",
        local_id, item_id, settings.message_ttl_seconds
    ));

    let mut chat = load_chat_state()?;
    chat.selected_contact = Some(wayfarer_id.to_string());
    mark_contact_seen(&mut chat, wayfarer_id);

    let thread = chat.threads.entry(wayfarer_id.to_string()).or_default();
    thread.push(ChatMessage {
        msg_id: local_id.clone(),
        text: body.to_string(),
        timestamp: format_timestamp_from_unix(now_secs),
        created_at_unix: now_secs,
        created_at_unix_ms: now_ms,
        direction: ChatDirection::Outgoing,
        seen: true,
        manifest_id_hex: Some(decoded.manifest_id_hex),
        delivered_at: None,
        outbound_state: Some(OutboundState::Sending),
        expires_at_unix_ms: Some(expiry_ms),
        last_sync_attempt_unix_ms: Some(now_ms),
        last_sync_error: None,
        attachment: attachment.clone().map(sanitize_attachment_for_chat_history),
        media: None,
    });

    let relay_http = settings
        .relay_endpoints
        .first()
        .cloned()
        .map(|endpoint| normalize_http_endpoint(&endpoint));
    let relay_enabled = settings.relay_sync_enabled && relay_http.is_some();

    let mut encounter_status = "message queued locally (LAN-only)".to_string();
    let pulled_messages_count = 0usize;

    if relay_enabled {
        let relay_ws = to_ws_endpoint(relay_http.as_deref().unwrap_or_default());
        request_relay_sync("send_message");
        encounter_status =
            if let Some(active) = relay_session_snapshot(&relay_ws, &identity.wayfarer_id) {
                format!(
                    "message queued locally; relay sync active (attempt_id={} state={} trigger={})",
                    active.attempt_id, active.state, active.trigger
                )
            } else {
                "message queued locally; relay worker scheduled".to_string()
            };
        mark_outgoing_message(&mut chat, wayfarer_id, &local_id, &item_id, None);
    } else {
        mark_outgoing_message(&mut chat, wayfarer_id, &local_id, &item_id, None);
    }

    save_chat_state(&chat)?;
    save_contact_aliases(&contacts)?;
    emit_chat_snapshot_event_best_effort("send_message_blocking");

    let message = latest_message_for_contact(&chat, wayfarer_id, &item_id, &local_id)
        .ok_or_else(|| "failed to load stored outbound message".to_string())?;

    log_verbose(&format!(
        "send_message_done: to={} msg_id={} pulled_messages={} encounter='{}'",
        wayfarer_id, message.msg_id, pulled_messages_count, encounter_status
    ));

    Ok(SendMessageResponse {
        message,
        chat,
        contacts,
        encounter_status,
        pulled_messages: pulled_messages_count,
    })
}

fn run_media_housekeeping_best_effort(context: &str, ttl_seconds_max: u64) {
    match media_run_housekeeping_tick(ttl_seconds_max) {
        Ok(true) => {
            emit_chat_snapshot_event_best_effort(context);
        }
        Ok(false) => {}
        Err(err) => {
            log_verbose(&format!(
                "media_housekeeping_failed context={context}: {err}"
            ));
        }
    }
}

#[tauri::command]
async fn sync_inbox() -> Result<SyncInboxResponse, String> {
    tauri::async_runtime::spawn_blocking(sync_inbox_blocking)
        .await
        .map_err(|err| format!("sync_inbox task join failed: {err}"))?
}

fn sync_inbox_blocking() -> Result<SyncInboxResponse, String> {
    let settings = load_app_settings()?;
    if !settings.relay_sync_enabled {
        return Ok(SyncInboxResponse {
            chat: load_chat_state()?,
            contacts: load_contact_aliases()?,
            pulled_messages: 0,
            status: "relay sync disabled".to_string(),
        });
    }

    if settings.relay_endpoints.is_empty() {
        return Ok(SyncInboxResponse {
            chat: load_chat_state()?,
            contacts: load_contact_aliases()?,
            pulled_messages: 0,
            status: "no relay endpoints configured".to_string(),
        });
    }

    request_relay_sync("sync_inbox_command");
    let chat = load_chat_state()?;
    let contacts = load_contact_aliases()?;

    Ok(SyncInboxResponse {
        chat,
        contacts,
        pulled_messages: 0,
        status: "relay sync scheduled".to_string(),
    })
}

#[tauri::command]
fn generate_share_qr(wayfarer_id: Option<String>) -> Result<ShareQrResponse, String> {
    let resolved_wayfarer = if let Some(value) = wayfarer_id {
        let trimmed = value.trim().to_ascii_lowercase();
        if !is_valid_wayfarer_id(&trimmed) {
            return Err("invalid wayfarer_id; expected 64 lowercase hex chars".to_string());
        }
        trimmed
    } else {
        ensure_local_identity()?.wayfarer_id
    };

    let path = generate_share_qr_png(&resolved_wayfarer)?;
    let bytes = fs::read(&path).map_err(|err| {
        format!(
            "failed reading generated qr image {}: {err}",
            path.display()
        )
    })?;
    let png_base64 = base64::engine::general_purpose::STANDARD.encode(bytes);

    Ok(ShareQrResponse {
        wayfarer_id: resolved_wayfarer,
        file_path: path.display().to_string(),
        png_base64,
    })
}

#[tauri::command]
fn decode_wayfarer_id_from_qr_bytes(bytes: Vec<u8>) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("empty image payload".to_string());
    }

    let image = image::load_from_memory(&bytes)
        .map_err(|err| format!("failed to decode image payload: {err}"))?
        .to_luma8();

    let mut prepared = rqrr::PreparedImage::prepare(image);
    let grids = prepared.detect_grids();
    if grids.is_empty() {
        return Err("no QR code detected in image".to_string());
    }

    for grid in grids {
        let Ok((_meta, content)) = grid.decode() else {
            continue;
        };

        if let Some(wayfarer_id) = extract_wayfarer_id_from_text(&content) {
            return Ok(wayfarer_id);
        }
    }

    Err("QR payload does not contain a valid Wayfarer ID".to_string())
}

#[tauri::command]
async fn run_relay_diagnostics(
    request: Option<RelayDiagnosticsRequest>,
) -> Result<Vec<RelayEndpointDiagnostics>, String> {
    tauri::async_runtime::spawn_blocking(move || run_relay_diagnostics_blocking(request))
        .await
        .map_err(|err| format!("run_relay_diagnostics task join failed: {err}"))?
}

fn run_relay_diagnostics_blocking(
    request: Option<RelayDiagnosticsRequest>,
) -> Result<Vec<RelayEndpointDiagnostics>, String> {
    request_relay_sync("relay_diagnostics");
    let settings = load_app_settings()?;
    let identity = ensure_local_identity()?;
    let auth_token = request.as_ref().and_then(|value| value.auth_token.clone());
    let trace_item_id = request
        .as_ref()
        .and_then(|value| value.trace_item_id.clone());

    let mut relay_endpoints = request
        .map(|value| value.relay_endpoints)
        .unwrap_or_else(|| settings.relay_endpoints.clone());
    relay_endpoints.retain(|value| !value.trim().is_empty());
    if relay_endpoints.is_empty() {
        relay_endpoints = settings.relay_endpoints;
    }

    let mut reports = Vec::with_capacity(relay_endpoints.len());
    for endpoint in relay_endpoints {
        let relay_http = normalize_http_endpoint(&endpoint);
        let relay_ws = to_ws_endpoint(&relay_http);
        let handshake_status =
            connect_to_relay_gossipv1_with_auth(&relay_ws, &identity, auth_token.as_deref());

        if !handshake_status.starts_with("connected") {
            reports.push(RelayEndpointDiagnostics {
                relay_http,
                relay_ws,
                handshake_status,
                encounter_status: "handshake failed".to_string(),
                transferred_items: 0,
                pulled_messages: 0,
            });
            continue;
        }

        let encounter = run_relay_encounter_gossipv1(
            &relay_ws,
            &identity,
            auth_token.as_deref(),
            trace_item_id.as_deref(),
        );

        match encounter {
            Ok(report) => reports.push(RelayEndpointDiagnostics {
                relay_http,
                relay_ws,
                handshake_status,
                encounter_status: "encounter complete".to_string(),
                transferred_items: report.transferred_items,
                pulled_messages: report.pulled_messages.len(),
            }),
            Err(err) => reports.push(RelayEndpointDiagnostics {
                relay_http,
                relay_ws,
                handshake_status,
                encounter_status: err,
                transferred_items: 0,
                pulled_messages: 0,
            }),
        }
    }

    Ok(reports)
}

fn gossip_runtime(initial_enabled: bool) -> &'static GossipRuntime {
    GOSSIP_RUNTIME.get_or_init(|| GossipRuntime::new(initial_enabled))
}

fn set_gossip_enabled(enabled: bool) {
    let runtime = gossip_runtime(enabled);
    runtime.enabled.store(enabled, Ordering::SeqCst);
    if enabled {
        runtime.force_announce.store(true, Ordering::SeqCst);
        set_gossip_event("listening");
    } else {
        set_gossip_event("disabled");
    }
}

fn set_gossip_event(event: &str) {
    if let Some(runtime) = GOSSIP_RUNTIME.get() {
        if let Ok(mut slot) = runtime.last_event.lock() {
            *slot = event.to_string();
        }
    }
}

fn current_gossip_status() -> GossipStatus {
    let runtime = gossip_runtime(false);
    let last_event = runtime
        .last_event
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|_| "status unavailable".to_string());

    GossipStatus {
        enabled: runtime.enabled.load(Ordering::SeqCst),
        running: runtime.running.load(Ordering::SeqCst),
        last_activity_ms: runtime.last_activity_ms.load(Ordering::SeqCst),
        last_event,
    }
}

fn start_relay_worker_if_needed() {
    let runtime = relay_worker_runtime();
    if runtime.running.swap(true, Ordering::SeqCst) {
        return;
    }

    let (tx, rx) = mpsc::channel::<RelayWorkerCommand>();
    if let Ok(mut slot) = runtime.command_tx.lock() {
        *slot = Some(tx);
    }

    thread::spawn(move || {
        let runtime = relay_worker_runtime();
        let e2e_disable_relay = std::env::var("AETHOS_E2E_DISABLE_RELAY")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if e2e_disable_relay {
            set_relay_worker_status("disabled_by_e2e_env");
            log_verbose("relay_worker_disabled_by_e2e_env");
            loop {
                let _ = relay_worker_wait_for_command(&rx, Duration::from_millis(1000));
            }
        }
        let auth = std::env::var("AETHOS_RELAY_AUTH_TOKEN").ok();
        let mut relay_http_endpoints: Vec<String> = Vec::new();
        let mut relay_ws_endpoints: Vec<String> = Vec::new();
        let mut relay_failures: Vec<u32> = Vec::new();
        let mut relay_next_attempt_at: Vec<Instant> = Vec::new();
        let mut rr_cursor = 0usize;
        let mut known_sync_epoch = runtime.sync_epoch.load(Ordering::SeqCst);

        set_relay_worker_status("starting");
        log_verbose("relay_worker_started");

        loop {
            let settings = match load_app_settings() {
                Ok(settings) => settings,
                Err(err) => {
                    set_relay_worker_status("settings_read_failed");
                    log_info(&format!("relay_worker_settings_read_failed: {err}"));
                    thread::sleep(Duration::from_secs(2));
                    continue;
                }
            };

            if !settings.relay_sync_enabled {
                set_relay_worker_status("disabled");
                let _ = relay_worker_wait_for_command(&rx, Duration::from_millis(800));
                continue;
            }

            let endpoints = settings
                .relay_endpoints
                .iter()
                .map(|endpoint| normalize_http_endpoint(endpoint))
                .filter(|endpoint| !endpoint.trim().is_empty())
                .collect::<Vec<_>>();

            if endpoints.is_empty() {
                set_relay_worker_status("no_relays_configured");
                let _ = relay_worker_wait_for_command(&rx, Duration::from_millis(800));
                continue;
            }

            if endpoints != relay_http_endpoints {
                relay_http_endpoints = endpoints.clone();
                relay_ws_endpoints = endpoints
                    .into_iter()
                    .map(|endpoint| to_ws_endpoint(&endpoint))
                    .collect();
                relay_failures = vec![0; relay_ws_endpoints.len()];
                relay_next_attempt_at = vec![Instant::now(); relay_ws_endpoints.len()];
                rr_cursor = 0;
                known_sync_epoch = runtime.sync_epoch.fetch_add(1, Ordering::SeqCst);
                log_verbose("relay_worker_endpoints_reloaded");
            }

            let identity = match ensure_local_identity() {
                Ok(identity) => identity,
                Err(err) => {
                    set_relay_worker_status("identity_unavailable");
                    log_info(&format!("relay_worker_identity_unavailable: {err}"));
                    let _ = relay_worker_wait_for_command(&rx, Duration::from_secs(1));
                    continue;
                }
            };

            let sync_epoch = runtime.sync_epoch.load(Ordering::SeqCst);
            let sync_trigger = if sync_epoch != known_sync_epoch {
                known_sync_epoch = sync_epoch;
                match relay_worker_wait_for_command(&rx, Duration::from_millis(1)) {
                    Some(RelayWorkerCommand::SyncNow(reason)) => reason,
                    None => "periodic",
                }
            } else {
                match relay_worker_wait_for_command(&rx, Duration::from_millis(250)) {
                    Some(RelayWorkerCommand::SyncNow(reason)) => {
                        known_sync_epoch = runtime.sync_epoch.load(Ordering::SeqCst);
                        reason
                    }
                    None => "periodic",
                }
            };

            let now = Instant::now();
            let mut selected_idx: Option<usize> = None;
            for offset in 0..relay_ws_endpoints.len() {
                let idx = (rr_cursor + offset) % relay_ws_endpoints.len();
                if relay_next_attempt_at[idx] <= now {
                    selected_idx = Some(idx);
                    rr_cursor = (idx + 1) % relay_ws_endpoints.len();
                    break;
                }
            }

            let Some(relay_slot) = selected_idx else {
                set_relay_worker_status("backoff_wait");
                let _ = relay_worker_wait_for_command(&rx, Duration::from_millis(350));
                continue;
            };

            let relay_ws = relay_ws_endpoints[relay_slot].clone();

            set_relay_worker_status(&format!(
                "connecting slot={} relay={}",
                relay_slot, relay_ws
            ));
            log_verbose(&format!(
                "relay_worker_sync_attempt: slot={} relay_ws={} trigger={}",
                relay_slot, relay_ws, sync_trigger
            ));

            match run_relay_encounter_gossipv1_for_duration(
                &relay_ws,
                &identity,
                auth.as_deref(),
                None,
                Duration::from_secs(45),
            ) {
                Ok(report) => {
                    relay_failures[relay_slot] = 0;
                    relay_next_attempt_at[relay_slot] = Instant::now();
                    let pulled = report.pulled_messages.len();
                    if let Err(err) = apply_relay_pulled_messages(
                        report.pulled_messages,
                        "relay_worker_encounter",
                    ) {
                        log_info(&format!(
                            "relay_worker_apply_pulled_messages_failed: slot={} relay_ws={} error={}",
                            relay_slot, relay_ws, err
                        ));
                    }
                    set_relay_worker_status(&format!(
                        "active slot={} relay={} transfers={} pulled={}",
                        relay_slot, relay_ws, report.transferred_items, pulled
                    ));
                    log_verbose(&format!(
                        "relay_worker_sync_success: slot={} relay_ws={} transfers={} pulled={}",
                        relay_slot, relay_ws, report.transferred_items, pulled
                    ));
                }
                Err(err) => {
                    relay_failures[relay_slot] = relay_failures[relay_slot].saturating_add(1);
                    let backoff_secs =
                        2_u64.saturating_pow(relay_failures[relay_slot].saturating_sub(1).min(6));
                    relay_next_attempt_at[relay_slot] =
                        Instant::now() + Duration::from_secs(backoff_secs.min(60));
                    set_relay_worker_status(&format!(
                        "disconnected slot={} relay={} error={}",
                        relay_slot, relay_ws, err
                    ));
                    log_info(&format!(
                        "relay_worker_sync_failed: slot={} relay_ws={} error={} backoff_s={}",
                        relay_slot,
                        relay_ws,
                        err,
                        backoff_secs.min(60)
                    ));
                }
            }
        }
    });
}

fn apply_relay_pulled_messages(
    pulled_messages: Vec<crate::relay::client::EncounterMessagePreview>,
    context: &str,
) -> Result<usize, String> {
    let ttl_seconds = load_app_settings()
        .map(|settings| settings.message_ttl_seconds)
        .unwrap_or(3600);
    run_media_housekeeping_best_effort("apply_relay_pulled_messages_pre", ttl_seconds);

    if pulled_messages.is_empty() {
        return Ok(0);
    }

    let mut chat = load_chat_state()?;
    let mut contacts = load_contact_aliases()?;
    let pulled_count = pulled_messages.len();
    merge_pulled_messages(&mut chat, &mut contacts, pulled_messages);
    save_chat_state(&chat)?;
    save_contact_aliases(&contacts)?;
    emit_chat_snapshot_event_best_effort(context);
    emit_sound_event_best_effort("sync", context);
    Ok(pulled_count)
}

fn start_gossip_worker_if_needed(initial_enabled: bool) {
    let runtime = gossip_runtime(initial_enabled);
    if runtime.running.swap(true, Ordering::SeqCst) {
        return;
    }

    thread::spawn(|| {
        let runtime = gossip_runtime(false);
        let socket = match bind_gossip_socket() {
            Ok(socket) => socket,
            Err(err) => {
                set_gossip_event(&format!("udp bind failed: {err}"));
                log_info(&format!(
                    "gossip_sync_disabled: failed binding udp/{}: {err}",
                    gossip_lan_port()
                ));
                runtime.running.store(false, Ordering::SeqCst);
                return;
            }
        };

        if let Err(err) = socket.set_nonblocking(true) {
            set_gossip_event(&format!("set_nonblocking failed: {err}"));
            log_info(&format!(
                "gossip_sync_disabled: set_nonblocking failed: {err}"
            ));
            runtime.running.store(false, Ordering::SeqCst);
            return;
        }
        if let Err(err) = socket.set_broadcast(true) {
            set_gossip_event(&format!("set_broadcast failed: {err}"));
            log_info(&format!(
                "gossip_sync_disabled: set_broadcast failed: {err}"
            ));
            runtime.running.store(false, Ordering::SeqCst);
            return;
        }

        set_gossip_event("listening");
        log_info(&format!(
            "gossip_sync_started on udp/{} (reuseaddr enabled)",
            gossip_lan_port()
        ));
        let mut peer_node_by_addr: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut peer_tcp_capable_by_ip: HashMap<String, bool> = HashMap::new();
        let mut tcp_backoff_until_by_ip: HashMap<String, Instant> = HashMap::new();
        let mut recent_served_request_by_peer: HashMap<String, (u64, Instant)> = HashMap::new();
        let mut recent_outbound_request_by_peer: HashMap<String, (u64, Instant)> = HashMap::new();
        let tcp_listener = match bind_gossip_tcp_listener() {
            Ok(listener) => Some(listener),
            Err(err) => {
                log_info(&format!("gossip_tcp_listener_disabled: {err}"));
                None
            }
        };
        let mut last_inventory_broadcast = Instant::now() - Duration::from_secs(10);

        loop {
            if !runtime.enabled.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(120));
                continue;
            }

            let force_announce = runtime.force_announce.swap(false, Ordering::SeqCst);
            if force_announce || last_inventory_broadcast.elapsed() >= Duration::from_secs(3) {
                if gossip_broadcast_inventory(&socket).is_ok() {
                    runtime
                        .last_activity_ms
                        .store(now_unix_ms(), Ordering::SeqCst);
                    set_gossip_event("active");
                    log_verbose("gossip_inventory_broadcasted");
                }
                last_inventory_broadcast = Instant::now();
            }

            if let Some(listener) = tcp_listener.as_ref() {
                loop {
                    match listener.accept() {
                        Ok((mut stream, peer_addr)) => {
                            let peer_ip_key = peer_addr.ip().to_string();
                            let peer_node_id = peer_node_by_addr.get(&peer_ip_key).cloned();
                            if let Err(err) = run_gossip_tcp_encounter_on_stream(
                                &mut stream,
                                peer_node_id,
                                runtime,
                                "inbound",
                                false,
                            ) {
                                log_verbose(&format!(
                                    "gossip_tcp_inbound_encounter_failed: peer={} error={}",
                                    peer_addr, err
                                ));
                            }
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(err) => {
                            log_verbose(&format!("gossip_tcp_accept_failed: {err}"));
                            break;
                        }
                    }
                }
            }

            let mut buf = [0u8; 65_535];
            match socket.recv_from(&mut buf) {
                Ok((len, source)) => {
                    runtime
                        .last_activity_ms
                        .store(now_unix_ms(), Ordering::SeqCst);
                    set_gossip_event("active");
                    log_verbose(&format!("gossip_recv bytes={} from={source}", len));
                    if let Err(err) = handle_gossip_frame(
                        &socket,
                        &buf[..len],
                        source,
                        &mut peer_node_by_addr,
                        &mut peer_tcp_capable_by_ip,
                        &mut tcp_backoff_until_by_ip,
                        &mut recent_served_request_by_peer,
                        &mut recent_outbound_request_by_peer,
                        runtime,
                    ) {
                        log_info(&format!("gossip_frame_handle_error from {source}: {err}"));
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(30));
                }
                Err(err) => {
                    set_gossip_event(&format!("recv error: {err}"));
                    log_info(&format!("gossip_recv_error: {err}"));
                    thread::sleep(Duration::from_millis(80));
                }
            }
        }
    });
}

fn bind_gossip_socket() -> Result<UdpSocket, String> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|err| format!("socket create failed: {err}"))?;
    socket
        .set_reuse_address(true)
        .map_err(|err| format!("set_reuse_address failed: {err}"))?;
    let bind_ip = if gossip_loopback_only_enabled() {
        std::net::Ipv4Addr::LOCALHOST
    } else {
        std::net::Ipv4Addr::UNSPECIFIED
    };
    let addr = std::net::SocketAddrV4::new(bind_ip, gossip_lan_port());
    socket
        .bind(&addr.into())
        .map_err(|err| format!("socket bind failed: {err}"))?;
    Ok(socket.into())
}

fn gossip_broadcast_inventory(socket: &UdpSocket) -> Result<(), String> {
    let identity = ensure_local_identity()?;
    let node_pubkey_raw = base64::engine::general_purpose::STANDARD
        .decode(&identity.verifying_key_b64)
        .map_err(|err| format!("gossip pubkey decode failed: {err}"))?;
    let node_pubkey = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(node_pubkey_raw);

    let peer_ports = gossip_peer_ports();
    let local_port = gossip_lan_port();
    if let Ok(hello) = build_lan_hello_frame(&identity.wayfarer_id, &node_pubkey) {
        for peer_port in &peer_ports {
            if *peer_port == local_port {
                continue;
            }
            if !gossip_loopback_only_enabled() {
                let _ = send_gossip_frame(socket, "255.255.255.255", *peer_port, &hello);
            }
            if gossip_localhost_fanout_enabled() || gossip_loopback_only_enabled() {
                let _ = send_gossip_frame(socket, "127.0.0.1", *peer_port, &hello);
            }
        }
    }
    if let Ok(ingest) = build_gossip_relay_ingest_frame(now_unix_ms()) {
        if let GossipSyncFrame::RelayIngest(frame) = &ingest {
            log_verbose(&format!(
                "gossip_inventory_snapshot: relay_ingest_items={}",
                frame.item_ids.len()
            ));
        }
    }
    if gossip_eager_unicast_enabled() {
        if let Ok(summary) = build_gossip_summary_frame(now_unix_ms()) {
            for peer_port in &peer_ports {
                if *peer_port == local_port {
                    continue;
                }
                let _ = send_gossip_frame(socket, "127.0.0.1", *peer_port, &summary);
            }
        }
        if let Ok(ingest) = build_gossip_relay_ingest_frame(now_unix_ms()) {
            for peer_port in &peer_ports {
                if *peer_port == local_port {
                    continue;
                }
                let _ = send_gossip_frame(socket, "127.0.0.1", *peer_port, &ingest);
            }
        }
    }
    log_verbose("gossip_broadcast_mode: hello-only (summary/ingest sent via unicast on HELLO)");
    Ok(())
}

fn handle_gossip_frame(
    socket: &UdpSocket,
    raw: &[u8],
    source: std::net::SocketAddr,
    peer_node_by_addr: &mut std::collections::HashMap<String, String>,
    peer_tcp_capable_by_ip: &mut HashMap<String, bool>,
    tcp_backoff_until_by_ip: &mut HashMap<String, Instant>,
    recent_served_request_by_peer: &mut HashMap<String, (u64, Instant)>,
    recent_outbound_request_by_peer: &mut HashMap<String, (u64, Instant)>,
    runtime: &GossipRuntime,
) -> Result<(), String> {
    let ttl_seconds = load_app_settings()
        .map(|settings| settings.message_ttl_seconds)
        .unwrap_or(3600);
    run_media_housekeeping_best_effort("handle_gossip_frame", ttl_seconds);

    if gossip_loopback_only_enabled() && !source.ip().is_loopback() {
        return Ok(());
    }
    let frame = parse_gossip_frame(raw)?;
    let identity = ensure_local_identity()?;
    let local_wayfarer = identity.wayfarer_id;
    let source_key = source.to_string();
    let source_ip_key = source.ip().to_string();

    match frame {
        GossipSyncFrame::Hello(hello) if hello.node_id != local_wayfarer => {
            let node_id = hello.node_id;
            peer_node_by_addr.insert(source_key.clone(), node_id.clone());
            peer_node_by_addr.insert(source_ip_key.clone(), node_id);
            let tcp_capable = hello
                .capabilities
                .iter()
                .any(|capability| capability == LAN_TCP_CAPABILITY);
            peer_tcp_capable_by_ip.insert(source_ip_key.clone(), tcp_capable);
            log_verbose(&format!(
                "gossip_peer_hello_mapped: source={} source_ip={} peers={} tcp_capable={}",
                source_key,
                source_ip_key,
                peer_node_by_addr.len(),
                tcp_capable
            ));
            if let Ok(summary) = build_gossip_summary_frame(now_unix_ms()) {
                let _ =
                    send_gossip_frame(socket, &source.ip().to_string(), source.port(), &summary);
            }
            if let Ok(ingest) = build_gossip_relay_ingest_frame(now_unix_ms()) {
                let _ = send_gossip_frame(socket, &source.ip().to_string(), source.port(), &ingest);
            }
        }
        GossipSyncFrame::Summary(summary) => {
            log_verbose(&format!(
                "gossip_recv_summary: from={} item_count={} preview_items={}",
                source,
                summary.item_count,
                summary
                    .preview_item_ids
                    .as_ref()
                    .map(|v| v.len())
                    .unwrap_or(0)
            ));
            log_verbose(&format!(
                "gossip_request_from_summary_skipped: to={} reason=prefer_relay_ingest_candidates",
                source
            ));
        }
        GossipSyncFrame::RelayIngest(ingest) => {
            log_verbose(&format!(
                "gossip_recv_relay_ingest: from={} item_ids={}",
                source,
                ingest.item_ids.len()
            ));
            let mut missing_item_ids = ingest
                .item_ids
                .into_iter()
                .filter(|item_id| gossip_has_item(item_id).map(|have| !have).unwrap_or(false))
                .collect::<Vec<_>>();
            missing_item_ids.sort();
            let request = build_gossip_request_frame(missing_item_ids, 256)?;
            if let GossipSyncFrame::Request(request_frame) = &request {
                let peer_key = source.ip().to_string();
                let fingerprint = request_fingerprint(&request_frame.want);
                if let Some((previous_fingerprint, seen_at)) =
                    recent_outbound_request_by_peer.get(&peer_key)
                {
                    if *previous_fingerprint == fingerprint
                        && seen_at.elapsed()
                            < Duration::from_millis(LAN_OUTBOUND_REQUEST_DEBOUNCE_MS)
                    {
                        log_verbose(&format!(
                            "gossip_outbound_request_debounced: to={} want_items={} debounce_ms={}",
                            source,
                            request_frame.want.len(),
                            LAN_OUTBOUND_REQUEST_DEBOUNCE_MS
                        ));
                        return Ok(());
                    }
                }
                log_verbose(&format!(
                    "gossip_send_request_from_relay_ingest: to={} want_items={}",
                    source,
                    request_frame.want.len()
                ));
                recent_outbound_request_by_peer.insert(peer_key, (fingerprint, Instant::now()));
            }
            let _ = send_gossip_frame(socket, &source.ip().to_string(), source.port(), &request);
        }
        GossipSyncFrame::Request(_req) => {
            if _req.want.is_empty() {
                return Ok(());
            }
            let peer_key = source.ip().to_string();
            let fingerprint = request_fingerprint(&_req.want);
            if let Some((previous_fingerprint, seen_at)) =
                recent_served_request_by_peer.get(&peer_key)
            {
                if *previous_fingerprint == fingerprint
                    && seen_at.elapsed() < Duration::from_millis(LAN_DUP_REQUEST_COOLDOWN_MS)
                {
                    log_verbose(&format!(
                        "gossip_request_duplicate_ignored: from={} want_items={} cooldown_ms={}",
                        source,
                        _req.want.len(),
                        LAN_DUP_REQUEST_COOLDOWN_MS
                    ));
                    return Ok(());
                }
            }

            let peer_node_id = peer_node_by_addr
                .get(&source.to_string())
                .or_else(|| peer_node_by_addr.get(&source.ip().to_string()))
                .cloned();
            let tcp_capable = peer_tcp_capable_by_ip
                .get(&peer_key)
                .copied()
                .unwrap_or(false);
            let tcp_backoff_active = tcp_backoff_until_by_ip
                .get(&peer_key)
                .map(|until| *until > Instant::now())
                .unwrap_or(false);

            if tcp_capable && !tcp_backoff_active {
                match run_gossip_tcp_encounter_with_peer(
                    source.ip(),
                    peer_node_id,
                    runtime,
                    "udp_request",
                ) {
                    Ok(_) => {
                        tcp_backoff_until_by_ip.remove(&peer_key);
                        recent_served_request_by_peer
                            .insert(peer_key, (fingerprint, Instant::now()));
                    }
                    Err(err) => {
                        tcp_backoff_until_by_ip.insert(
                            peer_key.clone(),
                            Instant::now() + Duration::from_secs(LAN_TCP_FAILURE_COOLDOWN_SECS),
                        );
                        log_verbose(&format!(
                            "gossip_tcp_encounter_from_udp_request_failed: peer={} error={} cooldown_s={}",
                            source, err, LAN_TCP_FAILURE_COOLDOWN_SECS
                        ));
                        serve_udp_transfer_for_request(socket, source, &_req.want)?;
                        recent_served_request_by_peer
                            .insert(peer_key, (fingerprint, Instant::now()));
                    }
                }
            } else {
                if !tcp_capable {
                    log_verbose(&format!(
                        "gossip_tcp_skipped_not_capable: peer={} capability={}",
                        source, LAN_TCP_CAPABILITY
                    ));
                }
                serve_udp_transfer_for_request(socket, source, &_req.want)?;
                recent_served_request_by_peer.insert(peer_key, (fingerprint, Instant::now()));
            }
        }
        GossipSyncFrame::Transfer(transfer) => {
            let peer_node_id = peer_node_by_addr
                .get(&source_key)
                .or_else(|| peer_node_by_addr.get(&source_ip_key))
                .cloned()
                .unwrap_or_else(|| source.ip().to_string());
            let transport_peer = source.to_string();
            log_verbose(&format!(
                "gossip_transfer_from_resolved_sender: source={} resolved={}",
                source_key, peer_node_id
            ));
            let result = import_transfer_items(
                &local_wayfarer,
                Some(&transport_peer),
                Some(&peer_node_id),
                &transfer.objects,
                now_unix_ms(),
            )?;

            if !result.rejected_items.is_empty() {
                let mut reasons = std::collections::BTreeMap::<String, usize>::new();
                for rejected in &result.rejected_items {
                    *reasons.entry(rejected.code.clone()).or_insert(0) += 1;
                }
                let reason_parts = reasons
                    .into_iter()
                    .map(|(code, count)| format!("{}:{}", code, count))
                    .collect::<Vec<_>>();
                log_verbose(&format!(
                    "gossip_transfer_rejected_summary: from={} rejected_total={} reasons={}",
                    source,
                    result.rejected_items.len(),
                    reason_parts.join(",")
                ));
            }

            if !result.new_messages.is_empty() {
                let imported_count = result.new_messages.len();
                let mut chat = load_chat_state()?;
                let mut contacts = load_contact_aliases()?;
                let pulled = result
                    .new_messages
                    .into_iter()
                    .map(|item| crate::relay::client::EncounterMessagePreview {
                        author_wayfarer_id: item.author_wayfarer_id,
                        session_peer: item.session_peer,
                        transport_peer: item.transport_peer,
                        item_id: item.item_id,
                        text: item.text,
                        received_at_unix: item.received_at_unix,
                        manifest_id_hex: item.manifest_id_hex,
                    })
                    .collect::<Vec<_>>();
                merge_pulled_messages(&mut chat, &mut contacts, pulled);
                save_chat_state(&chat)?;
                save_contact_aliases(&contacts)?;
                emit_chat_snapshot_event_best_effort("gossip_transfer_import");
                emit_sound_event_best_effort("sync", "gossip_transfer_import");
                runtime
                    .last_activity_ms
                    .store(now_unix_ms(), Ordering::SeqCst);
                set_gossip_event("received messages");
                log_verbose(&format!(
                    "gossip_transfer_imported_messages={} from={}",
                    imported_count, source
                ));
            }

            let receipt = GossipSyncFrame::Receipt(ReceiptFrame {
                received: result.accepted_item_ids,
            });
            let _ = send_gossip_frame(socket, &source.ip().to_string(), source.port(), &receipt);
        }
        GossipSyncFrame::Receipt(_receipt) => {}
        _ => {}
    }

    Ok(())
}

fn send_gossip_frame(
    socket: &UdpSocket,
    host: &str,
    port: u16,
    frame: &GossipSyncFrame,
) -> Result<(), String> {
    let raw = serialize_gossip_frame(frame)?;
    let addr = format!("{host}:{port}");
    let result = socket
        .send_to(&raw, &addr)
        .map(|_| ())
        .map_err(|err| format!("gossip send failed ({addr}): {err}"));
    if let Err(err) = &result {
        log_info(err);
    } else {
        log_verbose(&format!(
            "gossip_send_ok: frame_type={} bytes={} addr={}",
            gossip_frame_type(frame),
            raw.len(),
            addr
        ));
    }
    result
}

fn bind_gossip_tcp_listener() -> Result<TcpListener, String> {
    if lan_tcp_disabled() {
        return Err("lan tcp disabled by env".to_string());
    }
    let tcp_port = gossip_lan_port();
    let listener = TcpListener::bind(("0.0.0.0", tcp_port))
        .map_err(|err| format!("tcp bind failed on {}: {err}", tcp_port))?;
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("tcp listener set_nonblocking failed: {err}"))?;
    log_info(&format!("gossip_tcp_listener_started on tcp/{tcp_port}"));
    Ok(listener)
}

fn run_gossip_tcp_encounter_with_peer(
    peer_ip: std::net::IpAddr,
    peer_node_id: Option<String>,
    runtime: &GossipRuntime,
    trigger: &str,
) -> Result<(), String> {
    let addr = format!("{}:{}", peer_ip, gossip_lan_port());
    let mut stream =
        TcpStream::connect(&addr).map_err(|err| format!("tcp connect failed ({addr}): {err}"))?;
    stream
        .set_nodelay(true)
        .map_err(|err| format!("tcp set_nodelay failed ({addr}): {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(3000)))
        .map_err(|err| format!("tcp set_read_timeout failed ({addr}): {err}"))?;
    stream
        .set_write_timeout(Some(Duration::from_millis(3000)))
        .map_err(|err| format!("tcp set_write_timeout failed ({addr}): {err}"))?;

    run_gossip_tcp_encounter_on_stream(&mut stream, peer_node_id, runtime, trigger, true)
}

fn run_gossip_tcp_encounter_on_stream(
    stream: &mut TcpStream,
    peer_node_id: Option<String>,
    runtime: &GossipRuntime,
    trigger: &str,
    initiate: bool,
) -> Result<(), String> {
    let identity = ensure_local_identity()?;
    let local_wayfarer = identity.wayfarer_id;

    let node_pubkey_raw = base64::engine::general_purpose::STANDARD
        .decode(&identity.verifying_key_b64)
        .map_err(|err| format!("gossip pubkey decode failed: {err}"))?;
    let node_pubkey = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(node_pubkey_raw);

    if initiate {
        if let Ok(hello) = build_lan_hello_frame(&local_wayfarer, &node_pubkey) {
            send_gossip_frame_tcp(stream, &hello)?;
        }
        if let Ok(summary) = build_gossip_summary_frame(now_unix_ms()) {
            send_gossip_frame_tcp(stream, &summary)?;
        }
        if let Ok(ingest) = build_gossip_relay_ingest_frame(now_unix_ms()) {
            send_gossip_frame_tcp(stream, &ingest)?;
        }
    }

    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(3000) {
        let frame = match read_gossip_frame_tcp(stream) {
            Ok(frame) => frame,
            Err(err)
                if err.contains("timeout")
                    || err.contains("WouldBlock")
                    || err.contains("UnexpectedEof") =>
            {
                break;
            }
            Err(err) => return Err(err),
        };

        match frame {
            GossipSyncFrame::Hello(_) => {
                if let Ok(summary) = build_gossip_summary_frame(now_unix_ms()) {
                    send_gossip_frame_tcp(stream, &summary)?;
                }
                if let Ok(ingest) = build_gossip_relay_ingest_frame(now_unix_ms()) {
                    send_gossip_frame_tcp(stream, &ingest)?;
                }
            }
            GossipSyncFrame::Summary(summary) => {
                let request = build_request_from_summary(&summary, 256)?;
                send_gossip_frame_tcp(stream, &request)?;
            }
            GossipSyncFrame::RelayIngest(ingest) => {
                let mut missing_item_ids = ingest
                    .item_ids
                    .into_iter()
                    .filter(|item_id| gossip_has_item(item_id).map(|have| !have).unwrap_or(false))
                    .collect::<Vec<_>>();
                missing_item_ids.sort();
                let request = build_gossip_request_frame(missing_item_ids, 256)?;
                send_gossip_frame_tcp(stream, &request)?;
            }
            GossipSyncFrame::Request(req) => {
                let objects = gossip_transfer_items(
                    &req.want,
                    MAX_TRANSFER_ITEMS as u32,
                    MAX_TRANSFER_BYTES,
                    now_unix_ms(),
                )
                .unwrap_or_default();
                let transfer =
                    GossipSyncFrame::Transfer(crate::aethos_core::gossip_sync::TransferFrame {
                        objects,
                    });
                send_gossip_frame_tcp(stream, &transfer)?;
            }
            GossipSyncFrame::Transfer(transfer) => {
                let peer_label = peer_node_id
                    .as_deref()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown-peer".to_string());
                let transport_peer = stream
                    .peer_addr()
                    .map(|addr| addr.to_string())
                    .unwrap_or_else(|_| "tcp-peer".to_string());
                let result = import_transfer_items(
                    &local_wayfarer,
                    Some(&transport_peer),
                    Some(&peer_label),
                    &transfer.objects,
                    now_unix_ms(),
                )?;

                if !result.new_messages.is_empty() {
                    let mut chat = load_chat_state()?;
                    let mut contacts = load_contact_aliases()?;
                    let pulled = result
                        .new_messages
                        .into_iter()
                        .map(|item| crate::relay::client::EncounterMessagePreview {
                            author_wayfarer_id: item.author_wayfarer_id,
                            session_peer: item.session_peer,
                            transport_peer: item.transport_peer,
                            item_id: item.item_id,
                            text: item.text,
                            received_at_unix: item.received_at_unix,
                            manifest_id_hex: item.manifest_id_hex,
                        })
                        .collect::<Vec<_>>();
                    merge_pulled_messages(&mut chat, &mut contacts, pulled);
                    save_chat_state(&chat)?;
                    save_contact_aliases(&contacts)?;
                    emit_chat_snapshot_event_best_effort("gossip_tcp_transfer_import");
                    emit_sound_event_best_effort("sync", "gossip_tcp_transfer_import");
                    runtime
                        .last_activity_ms
                        .store(now_unix_ms(), Ordering::SeqCst);
                    set_gossip_event("received messages");
                }

                let receipt = GossipSyncFrame::Receipt(ReceiptFrame {
                    received: result.accepted_item_ids,
                });
                send_gossip_frame_tcp(stream, &receipt)?;
            }
            GossipSyncFrame::Receipt(_) => {}
        }
    }

    log_verbose(&format!("gossip_tcp_encounter_done: trigger={trigger}"));
    Ok(())
}

fn serve_udp_transfer_for_request(
    socket: &UdpSocket,
    source: std::net::SocketAddr,
    want: &[String],
) -> Result<(), String> {
    if want.is_empty() {
        return Ok(());
    }
    let mut pending = want.to_vec();
    while !pending.is_empty() {
        let objects = gossip_transfer_items(
            &pending,
            LAN_FALLBACK_TRANSFER_MAX_ITEMS,
            LAN_FALLBACK_TRANSFER_MAX_BYTES,
            now_unix_ms(),
        )
        .unwrap_or_default();
        if objects.is_empty() {
            break;
        }

        let mut sent_ids = std::collections::HashSet::new();
        for object in &objects {
            sent_ids.insert(object.item_id.clone());
        }
        let before = pending.len();
        pending.retain(|item_id| !sent_ids.contains(item_id));
        let removed = before.saturating_sub(pending.len());
        if removed == 0 {
            break;
        }

        let transfer =
            GossipSyncFrame::Transfer(crate::aethos_core::gossip_sync::TransferFrame { objects });
        send_gossip_frame(socket, &source.ip().to_string(), source.port(), &transfer)?;
        std::thread::sleep(Duration::from_millis(LAN_FALLBACK_CHUNK_PACING_MS));
    }
    Ok(())
}

fn request_fingerprint(item_ids: &[String]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    item_ids.len().hash(&mut hasher);
    for item_id in item_ids {
        item_id.hash(&mut hasher);
    }
    hasher.finish()
}

fn build_lan_hello_frame(wayfarer_id: &str, node_pubkey: &str) -> Result<GossipSyncFrame, String> {
    let mut frame = build_gossip_hello_frame(wayfarer_id, node_pubkey)?;
    if let GossipSyncFrame::Hello(hello) = &mut frame {
        if !lan_tcp_disabled()
            && !hello
                .capabilities
                .iter()
                .any(|capability| capability == LAN_TCP_CAPABILITY)
        {
            hello.capabilities.push(LAN_TCP_CAPABILITY.to_string());
        }
    }
    Ok(frame)
}

fn send_gossip_frame_tcp(stream: &mut TcpStream, frame: &GossipSyncFrame) -> Result<(), String> {
    let payload = serialize_gossip_frame(frame)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(format!("tcp frame exceeds max bytes: {}", payload.len()));
    }
    let len = u32::try_from(payload.len()).map_err(|_| "tcp frame too large".to_string())?;
    stream
        .write_all(&len.to_be_bytes())
        .map_err(|err| format!("tcp write length failed: {err}"))?;
    stream
        .write_all(&payload)
        .map_err(|err| format!("tcp write payload failed: {err}"))?;
    Ok(())
}

fn read_gossip_frame_tcp(stream: &mut TcpStream) -> Result<GossipSyncFrame, String> {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .map_err(|err| format!("tcp read length failed: {err}"))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(format!("tcp frame length exceeds max: {len}"));
    }
    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .map_err(|err| format!("tcp read payload failed: {err}"))?;
    parse_gossip_frame(&payload)
}

fn build_request_from_summary(
    summary: &crate::aethos_core::gossip_sync::SummaryFrame,
    max_want: usize,
) -> Result<GossipSyncFrame, String> {
    let want = gossip_select_request_item_ids_from_summary(summary, max_want)?;
    build_gossip_request_frame(want, max_want)
}

fn gossip_frame_type(frame: &GossipSyncFrame) -> &'static str {
    match frame {
        GossipSyncFrame::Hello(_) => "HELLO",
        GossipSyncFrame::Summary(_) => "SUMMARY",
        GossipSyncFrame::Request(_) => "REQUEST",
        GossipSyncFrame::Transfer(_) => "TRANSFER",
        GossipSyncFrame::Receipt(_) => "RECEIPT",
        GossipSyncFrame::RelayIngest(_) => "RELAY_INGEST",
    }
}

fn mark_outgoing_message(
    chat: &mut PersistedChatState,
    contact: &str,
    local_id: &str,
    item_id: &str,
    error: Option<String>,
) {
    if let Some(thread) = chat.threads.get_mut(contact) {
        if let Some(message) = thread.iter_mut().find(|item| item.msg_id == local_id) {
            message.last_sync_attempt_unix_ms = Some(now_unix_ms());
            if let Some(err) = error {
                log_verbose(&format!(
                    "outgoing_message_queued_without_ack: contact={} local_id={} item_id={} reason={}",
                    contact, local_id, item_id, err
                ));
                message.msg_id = item_id.to_string();
                message.outbound_state = Some(OutboundState::Sending);
                message.delivered_at = None;
                message.last_sync_error = None;
            } else {
                message.msg_id = item_id.to_string();
                message.outbound_state = Some(OutboundState::Sent);
                message.delivered_at = Some(format_timestamp_from_unix_ms(now_unix_ms()));
                message.last_sync_error = None;
            }
        }
    }
}

fn mark_contact_seen(chat: &mut PersistedChatState, contact: &str) {
    if let Some(thread) = chat.threads.get_mut(contact) {
        for message in thread.iter_mut() {
            if matches!(message.direction, ChatDirection::Incoming) {
                message.seen = true;
            }
        }
    }
    chat.new_contacts.retain(|value| value != contact);
}

fn merge_pulled_messages(
    chat: &mut PersistedChatState,
    contacts: &mut BTreeMap<String, String>,
    pulled_messages: Vec<crate::relay::client::EncounterMessagePreview>,
) {
    let settings = load_app_settings().ok();
    let ttl_seconds_max = settings
        .as_ref()
        .map(|value| value.message_ttl_seconds)
        .unwrap_or(3600);
    let identity = ensure_local_identity().ok();
    let author_signing_seed = load_local_signing_key_seed().ok();

    for pulled in pulled_messages {
        if let (Some(identity), Some(seed)) = (identity.as_ref(), author_signing_seed.as_ref()) {
            match process_incoming_media_message(
                &pulled,
                chat,
                contacts,
                ttl_seconds_max,
                &identity.wayfarer_id,
                seed,
            ) {
                Ok(MediaMessageProcess::NotMedia) => {}
                Ok(MediaMessageProcess::HandledSuppressed) => {
                    continue;
                }
                Ok(MediaMessageProcess::HandledManifest) => {
                    continue;
                }
                Err(err) => {
                    log_verbose(&format!(
                        "media_message_process_failed: item_id={} error={}",
                        pulled.item_id, err
                    ));
                    continue;
                }
            }
        }

        let sender_label = resolve_contact_id_for_preview(&pulled);
        let sender_alias = resolve_contact_alias_for_preview(&pulled);
        if let Some(receipt_manifest) = extract_receipt_manifest_id(&pulled.text) {
            apply_delivery_receipt(
                chat,
                &sender_label,
                &receipt_manifest,
                pulled.received_at_unix,
            );
            continue;
        }

        let is_new_contact = !contacts.contains_key(&sender_label);
        if is_new_contact {
            contacts.insert(sender_label.clone(), sender_alias);
            if !chat.new_contacts.iter().any(|value| value == &sender_label) {
                chat.new_contacts.push(sender_label.clone());
            }
        }

        let seen_on_insert = chat.selected_contact.as_deref() == Some(sender_label.as_str());

        let thread = chat.threads.entry(sender_label.clone()).or_default();
        let exists = thread.iter().any(|existing| {
            existing.msg_id == pulled.item_id
                || (pulled.manifest_id_hex.is_some()
                    && existing.manifest_id_hex == pulled.manifest_id_hex)
        });
        if exists {
            continue;
        }

        let message_unix_ms = extract_sent_at_unix_ms_if_json(&pulled.text).unwrap_or_else(|| {
            if pulled.received_at_unix > 1_000_000_000_000 {
                pulled.received_at_unix as u64
            } else {
                (pulled.received_at_unix.max(0) as u64).saturating_mul(1000)
            }
        });
        let message_unix = (message_unix_ms / 1000) as i64;
        thread.push(ChatMessage {
            msg_id: pulled.item_id,
            text: extract_chat_text_if_json(&pulled.text),
            timestamp: format_timestamp_from_unix(message_unix),
            created_at_unix: message_unix,
            created_at_unix_ms: message_unix_ms,
            direction: ChatDirection::Incoming,
            seen: seen_on_insert,
            manifest_id_hex: pulled.manifest_id_hex,
            delivered_at: None,
            outbound_state: None,
            expires_at_unix_ms: None,
            last_sync_attempt_unix_ms: None,
            last_sync_error: None,
            attachment: extract_attachment_if_json(&pulled.text)
                .map(sanitize_attachment_for_chat_history),
            media: None,
        });
        sort_thread_messages(thread);

        if chat.selected_contact.is_none() {
            chat.selected_contact = Some(sender_label.clone());
            mark_contact_seen(chat, &sender_label);
        }
    }

    normalize_chat_state(chat);
}

fn resolve_contact_id_for_preview(
    pulled: &crate::relay::client::EncounterMessagePreview,
) -> String {
    if let Some(author) = pulled.author_wayfarer_id.as_ref() {
        if is_valid_wayfarer_id(author) {
            return author.clone();
        }
    }

    "unknown-peer".to_string()
}

fn resolve_contact_alias_for_preview(
    pulled: &crate::relay::client::EncounterMessagePreview,
) -> String {
    if let Some(author) = pulled.author_wayfarer_id.as_ref() {
        if is_valid_wayfarer_id(author) {
            return author.clone();
        }
    }

    if let Some(session_peer) = pulled.session_peer.as_ref() {
        return format!("Unknown peer ({session_peer})");
    }

    if let Some(transport_peer) = pulled.transport_peer.as_ref() {
        return format!("Unknown peer ({transport_peer})");
    }

    "Unknown peer".to_string()
}

fn apply_delivery_receipt(
    chat: &mut PersistedChatState,
    contact: &str,
    receipt_manifest_id: &str,
    received_at_unix: i64,
) {
    let Some(thread) = chat.threads.get_mut(contact) else {
        return;
    };

    for message in thread.iter_mut().rev() {
        if matches!(message.direction, ChatDirection::Outgoing)
            && message.manifest_id_hex.as_deref() == Some(receipt_manifest_id)
        {
            message.outbound_state = Some(OutboundState::Sent);
            message.delivered_at = Some(format_timestamp_from_unix(received_at_unix));
            message.last_sync_error = None;
            break;
        }
    }
}

fn latest_message_for_contact(
    chat: &PersistedChatState,
    contact: &str,
    item_id: &str,
    local_id: &str,
) -> Option<ChatMessage> {
    chat.threads.get(contact).and_then(|thread| {
        thread
            .iter()
            .rev()
            .find(|message| message.msg_id == item_id || message.msg_id == local_id)
            .cloned()
            .or_else(|| {
                thread
                    .iter()
                    .rev()
                    .find(|message| matches!(message.direction, ChatDirection::Outgoing))
                    .cloned()
            })
    })
}

fn extract_chat_text_if_json(input: &str) -> String {
    if is_media_wire_message(input) {
        return String::new();
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
        return input.to_string();
    };

    if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            return text.to_string();
        }
    }

    if let Some(file_name) = value
        .get("attachment")
        .and_then(|v| v.get("file_name").or_else(|| v.get("fileName")))
        .and_then(|v| v.as_str())
    {
        return format!("[File] {file_name}");
    }

    input.to_string()
}

fn extract_sent_at_unix_ms_if_json(input: &str) -> Option<u64> {
    let value = serde_json::from_str::<serde_json::Value>(input).ok()?;
    value
        .get("sent_at_unix_ms")
        .or_else(|| value.get("sentAtUnixMs"))
        .and_then(|v| v.as_u64())
}

fn sort_thread_messages(thread: &mut [ChatMessage]) {
    thread.sort_by(|left, right| {
        left.created_at_unix
            .cmp(&right.created_at_unix)
            .then_with(|| left.msg_id.cmp(&right.msg_id))
    });
}

fn extract_attachment_if_json(input: &str) -> Option<ChatAttachment> {
    let value = serde_json::from_str::<serde_json::Value>(input).ok()?;
    let attachment = value.get("attachment")?;

    let file_name = attachment
        .get("file_name")
        .or_else(|| attachment.get("fileName"))
        .and_then(|v| v.as_str())?
        .to_string();
    let mime_type = attachment
        .get("mime_type")
        .or_else(|| attachment.get("mimeType"))
        .and_then(|v| v.as_str())?
        .to_string();
    let size_bytes = attachment
        .get("size_bytes")
        .or_else(|| attachment.get("sizeBytes"))
        .and_then(|v| v.as_u64())?;
    let content_b64 = attachment
        .get("content_b64")
        .or_else(|| attachment.get("contentB64"))
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());

    Some(ChatAttachment {
        file_name,
        mime_type,
        size_bytes,
        content_b64,
    })
}

fn sanitize_attachment_for_chat_history(mut attachment: ChatAttachment) -> ChatAttachment {
    attachment.file_name = sanitize_download_file_name(&attachment.file_name, "aethos-attachment");
    attachment.content_b64 = None;
    attachment
}

fn sanitize_download_file_name(input: &str, fallback: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let collapsed = out.trim_matches('.').trim_matches('_').trim();
    if collapsed.is_empty() {
        fallback.to_string()
    } else {
        collapsed.chars().take(180).collect()
    }
}

fn build_outbound_chat_payload(
    text: &str,
    client_msg_id: &str,
    sent_at_unix_ms: u64,
    attachment: Option<&ChatAttachment>,
) -> String {
    let mut value = serde_json::json!({
        "text": text,
        "client_msg_id": client_msg_id,
        "sent_at_unix_ms": sent_at_unix_ms,
    });

    if let Some(attachment) = attachment {
        value["attachment"] = serde_json::json!({
            "file_name": attachment.file_name,
            "mime_type": attachment.mime_type,
            "size_bytes": attachment.size_bytes,
            "content_b64": attachment.content_b64,
        });
    }

    value.to_string()
}

fn validate_send_attachment(attachment: &SendAttachmentRequest) -> Result<ChatAttachment, String> {
    let file_name = attachment.file_name.trim();
    if file_name.is_empty() {
        return Err("attachment file name cannot be empty".to_string());
    }

    if attachment.size_bytes == 0 {
        return Err("attachment cannot be empty".to_string());
    }

    let is_media_candidate = attachment
        .mime_type
        .trim()
        .to_ascii_lowercase()
        .starts_with("image/");
    let max_allowed_bytes = if is_media_candidate {
        media_max_object_bytes()
    } else {
        MAX_INLINE_ATTACHMENT_BYTES
    };
    if attachment.size_bytes > max_allowed_bytes {
        return Err(format!(
            "attachment too large; max {} bytes",
            max_allowed_bytes
        ));
    }

    let decoded = read_attachment_bytes(attachment)?;
    if decoded.len() as u64 != attachment.size_bytes {
        return Err("attachment size mismatch".to_string());
    }

    Ok(ChatAttachment {
        file_name: file_name.to_string(),
        mime_type: attachment.mime_type.trim().to_string(),
        size_bytes: attachment.size_bytes,
        content_b64: Some(base64::engine::general_purpose::STANDARD.encode(decoded)),
    })
}

fn read_attachment_bytes(attachment: &SendAttachmentRequest) -> Result<Vec<u8>, String> {
    if let Some(content_b64) = attachment.content_b64.as_ref() {
        return base64::engine::general_purpose::STANDARD
            .decode(content_b64.trim())
            .map_err(|_| "attachment base64 content is invalid".to_string());
    }

    Err("attachment must include contentB64".to_string())
}

fn emit_chat_snapshot_event() -> Result<(), String> {
    let Some(handle) = APP_HANDLE.get() else {
        return Ok(());
    };

    let snapshot = ChatSnapshot {
        contacts: load_contact_aliases()?,
        chat: load_chat_state()?,
    };

    handle
        .emit(CHAT_SNAPSHOT_EVENT, snapshot)
        .map_err(|err| format!("failed emitting {CHAT_SNAPSHOT_EVENT}: {err}"))
}

fn emit_chat_snapshot_event_best_effort(context: &str) {
    if let Err(err) = emit_chat_snapshot_event() {
        log_verbose(&format!(
            "chat_snapshot_emit_failed context={context}: {err}"
        ));
    }
}

fn emit_sound_event(kind: &str) -> Result<(), String> {
    let Some(handle) = APP_HANDLE.get() else {
        return Ok(());
    };

    handle
        .emit(
            SOUND_EVENT,
            SoundEventPayload {
                kind: kind.to_string(),
            },
        )
        .map_err(|err| format!("failed emitting {SOUND_EVENT}: {err}"))
}

fn emit_sound_event_best_effort(kind: &str, context: &str) {
    match emit_sound_event(kind) {
        Ok(()) => log_verbose(&format!("sound_played: {kind}")),
        Err(err) => log_verbose(&format!(
            "sound_emit_failed context={context} kind={kind}: {err}"
        )),
    }
}

fn extract_receipt_manifest_id(input: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(input).ok()?;
    let candidate = value
        .get("receipt_manifest_id")
        .and_then(|item| item.as_str())
        .or_else(|| {
            value
                .get("receiptManifestId")
                .and_then(|item| item.as_str())
        })
        .or_else(|| value.get("manifest_id_hex").and_then(|item| item.as_str()))
        .or_else(|| value.get("manifestIdHex").and_then(|item| item.as_str()))?;

    let normalized = candidate.trim().to_ascii_lowercase();
    if normalized.len() == 64 && normalized.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(normalized)
    } else {
        None
    }
}

fn format_timestamp_from_unix(unix_secs: i64) -> String {
    unix_secs.to_string()
}

fn format_timestamp_from_unix_ms(unix_ms: u64) -> String {
    (unix_ms / 1000).to_string()
}

fn extract_wayfarer_id_from_text(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if is_valid_wayfarer_id(trimmed) {
        return Some(trimmed.to_string());
    }

    let lowered = trimmed.to_ascii_lowercase();
    for prefix in ["aethos:", "wayfarer:", "aethos://"] {
        if let Some(rest) = lowered.strip_prefix(prefix) {
            let candidate = rest.trim_matches('/').trim();
            if is_valid_wayfarer_id(candidate) {
                return Some(candidate.to_string());
            }
        }
    }

    let bytes = lowered.as_bytes();
    for start in 0..bytes.len() {
        if start + 64 > bytes.len() {
            break;
        }
        let candidate = &lowered[start..start + 64];
        if is_valid_wayfarer_id(candidate) {
            return Some(candidate.to_string());
        }
    }

    None
}

fn generate_share_qr_png(wayfarer_id: &str) -> Result<PathBuf, String> {
    let code = QrCode::new(wayfarer_id.as_bytes())
        .map_err(|err| format!("failed generating QR payload: {err}"))?;
    let scale: u32 = 8;
    let border: u32 = 4;
    let luma: ImageBuffer<Luma<u8>, Vec<u8>> = code
        .render::<Luma<u8>>()
        .quiet_zone(false)
        .module_dimensions(scale, scale)
        .build();

    let inner_w = luma.width();
    let inner_h = luma.height();
    let width = inner_w + border * scale * 2;
    let height = inner_h + border * scale * 2;
    let mut rgba = RgbaImage::from_pixel(width, height, Rgba([255, 255, 255, 255]));

    for y in 0..inner_h {
        for x in 0..inner_w {
            let px = luma.get_pixel(x, y).0[0];
            let color = if px < 128 {
                Rgba([16, 18, 28, 255])
            } else {
                Rgba([255, 255, 255, 255])
            };
            rgba.put_pixel(x + border * scale, y + border * scale, color);
        }
    }

    let monogram = a_monogram_icon_rgba((width / 6).max(36));
    let monogram = image::imageops::resize(
        &monogram,
        monogram.width(),
        monogram.height(),
        FilterType::Triangle,
    );
    overlay_center(&mut rgba, &monogram);

    let path = share_qr_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed creating share qr dir {}: {err}", parent.display()))?;
    }
    rgba.save(&path)
        .map_err(|err| format!("failed saving share QR image at {}: {err}", path.display()))?;
    Ok(path)
}

fn share_qr_file_path() -> PathBuf {
    if let Ok(base) = std::env::var("AETHOS_STATE_DIR") {
        if !base.trim().is_empty() {
            return Path::new(&base).join(SHARE_QR_FILE_NAME);
        }
    }

    if let Ok(xdg_state_home) = std::env::var("XDG_STATE_HOME") {
        if !xdg_state_home.trim().is_empty() {
            return Path::new(&xdg_state_home)
                .join("aethos-linux")
                .join(SHARE_QR_FILE_NAME);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            #[cfg(target_os = "macos")]
            {
                return Path::new(&home)
                    .join("Library")
                    .join("Application Support")
                    .join("aethos-linux")
                    .join(SHARE_QR_FILE_NAME);
            }

            #[cfg(not(target_os = "macos"))]
            {
                return Path::new(&home)
                    .join(".local")
                    .join("state")
                    .join("aethos-linux")
                    .join(SHARE_QR_FILE_NAME);
            }
        }
    }

    std::env::temp_dir().join(SHARE_QR_FILE_NAME)
}

fn overlay_center(base: &mut RgbaImage, overlay: &RgbaImage) {
    let offset_x = base.width().saturating_sub(overlay.width()) / 2;
    let offset_y = base.height().saturating_sub(overlay.height()) / 2;

    for y in 0..overlay.height() {
        for x in 0..overlay.width() {
            let src = overlay.get_pixel(x, y);
            let alpha = src[3] as f32 / 255.0;
            if alpha <= 0.0 {
                continue;
            }

            let dst = base.get_pixel_mut(x + offset_x, y + offset_y);
            for i in 0..3 {
                let blended = (src[i] as f32 * alpha) + (dst[i] as f32 * (1.0 - alpha));
                dst[i] = blended.round() as u8;
            }
            dst[3] = 255;
        }
    }
}

fn a_monogram_icon_rgba(size: u32) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(size, size, Rgba([255, 255, 255, 0]));
    let cx = size as f32 * 0.5;
    let cy = size as f32 * 0.5;
    let radius = size as f32 * 0.44;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= radius {
                img.put_pixel(x, y, Rgba([250, 252, 255, 245]));
            }
        }
    }

    let stroke = Rgba([33, 79, 188, 255]);
    let left_x = (size as f32 * 0.32) as i32;
    let right_x = (size as f32 * 0.68) as i32;
    let top_y = (size as f32 * 0.28) as i32;
    let bottom_y = (size as f32 * 0.74) as i32;
    let cross_y = (size as f32 * 0.54) as i32;

    draw_line(
        &mut img,
        left_x,
        bottom_y,
        (size as f32 * 0.5) as i32,
        top_y,
        stroke,
    );
    draw_line(
        &mut img,
        right_x,
        bottom_y,
        (size as f32 * 0.5) as i32,
        top_y,
        stroke,
    );
    draw_line(
        &mut img,
        (size as f32 * 0.39) as i32,
        cross_y,
        (size as f32 * 0.61) as i32,
        cross_y,
        stroke,
    );

    img
}

fn draw_line(img: &mut RgbaImage, mut x0: i32, mut y0: i32, x1: i32, y1: i32, color: Rgba<u8>) {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        for oy in -1..=1 {
            for ox in -1..=1 {
                let px = x0 + ox;
                let py = y0 + oy;
                if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                    img.put_pixel(px as u32, py as u32, color);
                }
            }
        }

        if x0 == x1 && y0 == y1 {
            break;
        }

        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

fn sanitize_seed_for_file(seed: &str) -> String {
    let mut out = String::with_capacity(seed.len());
    for ch in seed.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' {
            out.push(ch);
        }
    }
    if out.is_empty() {
        "seed".to_string()
    } else {
        out.chars().take(48).collect()
    }
}

fn deterministic_byte(seed: &[u8], x: u32, y: u32, channel: u8) -> u8 {
    let mut hasher = sha2::Sha256::new();
    hasher.update(seed);
    hasher.update(x.to_le_bytes());
    hasher.update(y.to_le_bytes());
    hasher.update([channel]);
    let digest = hasher.finalize();
    digest[0]
}

fn generate_aethos_e2e_png(seed: &str, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let seed_bytes = sha2::Sha256::digest(seed.as_bytes());
    let mut image = RgbaImage::new(width, height);

    let palette = [
        [12u8, 21u8, 48u8],
        [34u8, 77u8, 180u8],
        [18u8, 139u8, 188u8],
        [137u8, 88u8, 212u8],
        [222u8, 241u8, 255u8],
    ];

    for y in 0..height {
        for x in 0..width {
            let t = deterministic_byte(&seed_bytes, x, y, 0) as usize;
            let base = palette[t % palette.len()];
            let noise = deterministic_byte(&seed_bytes, x, y, 1);
            let glow = ((x as f32 / width as f32) * 255.0) as u8;
            let r = base[0].saturating_add(noise / 3).saturating_add(glow / 6);
            let g = base[1].saturating_add(noise / 4).saturating_add(glow / 8);
            let b = base[2].saturating_add(noise / 5).saturating_add(glow / 10);
            image.put_pixel(x, y, Rgba([r, g, b, 255]));
        }
    }

    let title = format!(
        "AETHOS {}",
        sanitize_seed_for_file(seed).to_ascii_uppercase()
    );
    draw_seeded_caption(&mut image, &seed_bytes, &title);

    let mut encoded = Vec::new();
    {
        let dyn_image = image::DynamicImage::ImageRgba8(image);
        let mut cursor = std::io::Cursor::new(&mut encoded);
        dyn_image
            .write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|err| format!("failed encoding e2e png: {err}"))?;
    }
    Ok(encoded)
}

fn draw_seeded_caption(image: &mut RgbaImage, seed: &[u8], caption: &str) {
    let scale = (image.width().min(image.height()) / 256).max(2);
    let mut cursor_x = (image.width() / 12).max(6);
    let base_y = image.height().saturating_sub((12 * scale).max(16));

    for (idx, ch) in caption.chars().enumerate() {
        let glyph = glyph_5x7(ch);
        let jitter = (seed[idx % seed.len()] % 5) as i32 - 2;
        for row in 0..7u32 {
            for col in 0..5u32 {
                if (glyph[row as usize] >> (4 - col)) & 1 == 0 {
                    continue;
                }
                for oy in 0..scale {
                    for ox in 0..scale {
                        let px = cursor_x as i32 + (col * scale + ox) as i32 + jitter;
                        let py = base_y as i32 + (row * scale + oy) as i32;
                        if px < 0
                            || py < 0
                            || px as u32 >= image.width()
                            || py as u32 >= image.height()
                        {
                            continue;
                        }
                        image.put_pixel(px as u32, py as u32, Rgba([233, 246, 255, 255]));
                    }
                }
            }
        }
        cursor_x = cursor_x.saturating_add(6 * scale);
        if cursor_x >= image.width().saturating_sub(6 * scale) {
            break;
        }
    }
}

fn glyph_5x7(ch: char) -> [u8; 7] {
    match ch {
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b11110, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'S' => [
            0b01111, 0b10000, 0b01110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        '0' => [
            0b01110, 0b10011, 0b10101, 0b11001, 0b10001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b11100,
        ],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 0b11111],
        ' ' => [0, 0, 0, 0, 0, 0, 0],
        _ => [
            0b11111, 0b00001, 0b00110, 0b00100, 0b00000, 0b00100, 0b00100,
        ],
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let _ = APP_HANDLE.set(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_diagnostics,
            app_version,
            read_app_log,
            clear_app_log,
            bootstrap_state,
            chat_snapshot,
            load_completed_media,
            get_completed_media_path,
            open_completed_media_in_system_viewer,
            generate_e2e_large_image,
            rotate_wayfarer_id,
            reset_wayfarer_id,
            update_settings,
            upsert_contact,
            remove_contact,
            save_chat,
            send_message,
            sync_inbox,
            gossip_status,
            gossip_announce_now,
            relay_health_status,
            generate_share_qr,
            decode_wayfarer_id_from_qr_bytes,
            open_external_url,
            run_relay_diagnostics
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn main() {
    apply_cli_state_overrides();
    run();
}

fn apply_cli_state_overrides() {
    let args = std::env::args().collect::<Vec<_>>();
    for arg in args.into_iter().skip(1) {
        if let Some(path) = arg.strip_prefix("--aethos-state-dir=") {
            if !path.trim().is_empty() {
                std::env::set_var("AETHOS_STATE_DIR", path);
                std::env::set_var("XDG_DATA_HOME", path);
                std::env::set_var("XDG_STATE_HOME", path);
            }
        } else if let Some(port) = arg.strip_prefix("--aethos-gossip-lan-port=") {
            if !port.trim().is_empty() {
                std::env::set_var("AETHOS_GOSSIP_LAN_PORT", port.trim());
            }
        } else if let Some(peer_ports) = arg.strip_prefix("--aethos-gossip-peer-ports=") {
            if !peer_ports.trim().is_empty() {
                std::env::set_var("AETHOS_GOSSIP_PEER_PORTS", peer_ports.trim());
            }
        } else if let Some(value) = arg.strip_prefix("--aethos-disable-lan-tcp=") {
            if !value.trim().is_empty() {
                std::env::set_var("AETHOS_DISABLE_LAN_TCP", value.trim());
            }
        } else if let Some(value) = arg.strip_prefix("--aethos-gossip-localhost-fanout=") {
            if !value.trim().is_empty() {
                std::env::set_var("AETHOS_GOSSIP_LOCALHOST_FANOUT", value.trim());
            }
        } else if let Some(value) = arg.strip_prefix("--aethos-gossip-eager-unicast=") {
            if !value.trim().is_empty() {
                std::env::set_var("AETHOS_GOSSIP_EAGER_UNICAST", value.trim());
            }
        } else if let Some(value) = arg.strip_prefix("--aethos-gossip-loopback-only=") {
            if !value.trim().is_empty() {
                std::env::set_var("AETHOS_GOSSIP_LOOPBACK_ONLY", value.trim());
            }
        } else if let Some(value) = arg.strip_prefix("--aethos-e2e-disable-relay=") {
            if !value.trim().is_empty() {
                std::env::set_var("AETHOS_E2E_DISABLE_RELAY", value.trim());
            }
        } else if let Some(value) = arg.strip_prefix("--aethos-e2e-force-verbose=") {
            if !value.trim().is_empty() {
                std::env::set_var("AETHOS_E2E_FORCE_VERBOSE", value.trim());
            }
        } else if let Some(value) = arg.strip_prefix("--aethos-e2e-force-gossip=") {
            if !value.trim().is_empty() {
                std::env::set_var("AETHOS_E2E_FORCE_GOSSIP", value.trim());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::shared_test_env_lock;
    use std::collections::BTreeMap;
    use std::net::{SocketAddr, TcpListener};

    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(original) = self.original.take() {
                std::env::set_var(self.key, original);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn maybe_relay_target() -> Option<String> {
        std::env::var("AETHOS_RELAY_TEST_HTTP")
            .ok()
            .filter(|value| !value.trim().is_empty())
    }

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let client = TcpStream::connect(addr).expect("connect client");
        let (server, _) = listener.accept().expect("accept server");
        (client, server)
    }

    fn with_state_dir<T>(state_dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let original_state_dir = std::env::var("AETHOS_STATE_DIR").ok();
        let original_xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
        let original_xdg_state_home = std::env::var("XDG_STATE_HOME").ok();
        std::env::set_var("AETHOS_STATE_DIR", state_dir);
        std::env::set_var("XDG_DATA_HOME", state_dir);
        std::env::set_var("XDG_STATE_HOME", state_dir);
        let result = f();
        if let Some(value) = original_state_dir {
            std::env::set_var("AETHOS_STATE_DIR", value);
        } else {
            std::env::remove_var("AETHOS_STATE_DIR");
        }
        if let Some(value) = original_xdg_data_home {
            std::env::set_var("XDG_DATA_HOME", value);
        } else {
            std::env::remove_var("XDG_DATA_HOME");
        }
        if let Some(value) = original_xdg_state_home {
            std::env::set_var("XDG_STATE_HOME", value);
        } else {
            std::env::remove_var("XDG_STATE_HOME");
        }
        result
    }

    #[test]
    fn embedded_release_version_is_semver_like() {
        let version = embedded_release_version();
        assert!(version.split('.').count() >= 3);
    }

    #[test]
    fn extract_wayfarer_id_recovers_embedded_values() {
        let id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(extract_wayfarer_id_from_text(id), Some(id.to_string()));
        assert_eq!(
            extract_wayfarer_id_from_text(&format!("aethos:{id}")),
            Some(id.to_string())
        );
        assert_eq!(
            extract_wayfarer_id_from_text(&format!("scan={id}&source=camera")),
            Some(id.to_string())
        );
    }

    #[test]
    fn qr_roundtrip_decodes_wayfarer_id() {
        let id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let path = generate_share_qr_png(id).expect("generate qr image");
        let bytes = fs::read(path).expect("read qr bytes");
        let decoded = decode_wayfarer_id_from_qr_bytes(bytes).expect("decode qr payload");
        assert_eq!(decoded, id);
    }

    #[test]
    fn empty_summary_still_produces_request_frame() {
        let summary = crate::aethos_core::gossip_sync::SummaryFrame {
            bloom_filter: vec![0u8; crate::aethos_core::gossip_sync::BLOOM_FILTER_BYTES],
            item_count: 0,
            preview_item_ids: None,
            preview_cursor: None,
        };

        let request =
            build_request_from_summary(&summary, 256).expect("build request from summary");
        match request {
            GossipSyncFrame::Request(request) => {
                assert!(request.want.is_empty());
            }
            other => panic!("expected REQUEST frame, got {other:?}"),
        }
    }

    #[test]
    fn populated_summary_produces_request_with_want_items() {
        let _lock = shared_test_env_lock().lock().expect("lock");
        let temp_dir = std::env::temp_dir().join(format!(
            "aethos-tauri-test-summary-{}",
            rand::random::<u64>()
        ));
        let _guard = EnvVarGuard::set("AETHOS_STATE_DIR", &temp_dir.display().to_string());

        let wanted_item =
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        let bloom = crate::aethos_core::gossip_sync::build_bloom_filter(&[wanted_item.clone()])
            .expect("build bloom filter");
        let summary = crate::aethos_core::gossip_sync::SummaryFrame {
            bloom_filter: bloom,
            item_count: 1,
            preview_item_ids: Some(vec![wanted_item.clone()]),
            preview_cursor: None,
        };

        let request =
            build_request_from_summary(&summary, 256).expect("build request from summary");
        match request {
            GossipSyncFrame::Request(request) => {
                assert!(request.want.iter().any(|item| item == &wanted_item));
            }
            other => panic!("expected REQUEST frame, got {other:?}"),
        }
    }

    #[test]
    fn imported_message_is_kept_when_sender_is_unresolved() {
        let mut chat = PersistedChatState::default();
        let mut contacts = BTreeMap::new();
        let pulled = vec![crate::relay::client::EncounterMessagePreview {
            author_wayfarer_id: None,
            session_peer: None,
            transport_peer: None,
            item_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            text: "hello from unresolved sender".to_string(),
            received_at_unix: 1,
            manifest_id_hex: None,
        }];

        merge_pulled_messages(&mut chat, &mut contacts, pulled);
        let thread = chat
            .threads
            .get("unknown-peer")
            .expect("thread should be created for unresolved sender");
        assert_eq!(thread.len(), 1);
        assert_eq!(thread[0].text, "hello from unresolved sender");
    }

    #[test]
    fn outbound_chat_payload_json_keeps_display_text() {
        let payload = build_outbound_chat_payload("hello", "local-123", 42, None);
        assert_eq!(extract_chat_text_if_json(&payload), "hello");
    }

    #[test]
    fn outbound_chat_payload_keeps_emoji_text() {
        let payload = build_outbound_chat_payload("hello 🌬️🚀", "local-emoji", 42, None);
        assert_eq!(extract_chat_text_if_json(&payload), "hello 🌬️🚀");
    }

    #[test]
    fn outbound_chat_payload_roundtrips_attachment_metadata() {
        let attachment = ChatAttachment {
            file_name: "note.txt".to_string(),
            mime_type: "text/plain".to_string(),
            size_bytes: 4,
            content_b64: Some(base64::engine::general_purpose::STANDARD.encode("test")),
        };
        let payload = build_outbound_chat_payload("", "local-file", 42, Some(&attachment));
        let extracted = extract_attachment_if_json(&payload).expect("attachment extracted");
        assert_eq!(extracted.file_name, "note.txt");
        assert_eq!(extracted.mime_type, "text/plain");
        assert_eq!(extracted.size_bytes, 4);
    }

    #[test]
    fn outbound_chat_payload_exposes_sent_at_for_thread_sorting() {
        let payload = build_outbound_chat_payload("hello", "local-123", 1700000000123, None);
        assert_eq!(
            extract_sent_at_unix_ms_if_json(&payload),
            Some(1_700_000_000_123)
        );
    }

    #[test]
    fn incoming_messages_are_sorted_by_sent_at_when_present() {
        let mut chat = PersistedChatState::default();
        let mut contacts = BTreeMap::new();

        let newer = serde_json::json!({
            "text": "newer",
            "client_msg_id": "m2",
            "sent_at_unix_ms": 2000
        })
        .to_string();
        let older = serde_json::json!({
            "text": "older",
            "client_msg_id": "m1",
            "sent_at_unix_ms": 1000
        })
        .to_string();

        merge_pulled_messages(
            &mut chat,
            &mut contacts,
            vec![
                crate::relay::client::EncounterMessagePreview {
                    author_wayfarer_id: Some(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                            .to_string(),
                    ),
                    session_peer: None,
                    transport_peer: None,
                    item_id: "fbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                        .to_string(),
                    text: newer,
                    received_at_unix: 20,
                    manifest_id_hex: None,
                },
                crate::relay::client::EncounterMessagePreview {
                    author_wayfarer_id: Some(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                            .to_string(),
                    ),
                    session_peer: None,
                    transport_peer: None,
                    item_id: "abbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                        .to_string(),
                    text: older,
                    received_at_unix: 10,
                    manifest_id_hex: None,
                },
            ],
        );

        let thread = chat
            .threads
            .get("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .expect("thread should exist");
        assert_eq!(thread.len(), 2);
        assert_eq!(thread[0].text, "older");
        assert_eq!(thread[1].text, "newer");
    }

    #[test]
    fn outbound_chat_payload_changes_manifest_for_same_text() {
        let recipient = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let signing_seed = [9u8; 32];

        let payload_a = build_envelope_payload_b64_from_utf8(
            recipient,
            &build_outbound_chat_payload("same text", "local-1", 1000, None),
            &signing_seed,
        )
        .expect("build payload a");
        let payload_b = build_envelope_payload_b64_from_utf8(
            recipient,
            &build_outbound_chat_payload("same text", "local-2", 1000, None),
            &signing_seed,
        )
        .expect("build payload b");

        let decoded_a = decode_envelope_payload_b64(&payload_a).expect("decode payload a");
        let decoded_b = decode_envelope_payload_b64(&payload_b).expect("decode payload b");

        assert_ne!(decoded_a.manifest_id_hex, decoded_b.manifest_id_hex);
    }

    #[test]
    fn tcp_frame_roundtrip_preserves_summary_frame() {
        let (mut sender, mut receiver) = tcp_pair();
        let frame = GossipSyncFrame::Summary(crate::aethos_core::gossip_sync::SummaryFrame {
            bloom_filter: vec![0u8; crate::aethos_core::gossip_sync::BLOOM_FILTER_BYTES],
            item_count: 0,
            preview_item_ids: None,
            preview_cursor: None,
        });

        send_gossip_frame_tcp(&mut sender, &frame).expect("send tcp frame");
        let decoded = read_gossip_frame_tcp(&mut receiver).expect("read tcp frame");
        match decoded {
            GossipSyncFrame::Summary(summary) => {
                assert_eq!(summary.item_count, 0);
                assert_eq!(
                    summary.bloom_filter.len(),
                    crate::aethos_core::gossip_sync::BLOOM_FILTER_BYTES
                );
            }
            other => panic!("expected SUMMARY, got {other:?}"),
        }
    }

    #[test]
    fn tcp_frame_rejects_oversize_length_prefix() {
        let (mut client, mut server) = tcp_pair();
        let oversized = (MAX_FRAME_BYTES as u32).saturating_add(1);
        server
            .write_all(&oversized.to_be_bytes())
            .expect("write oversize length");

        let err = read_gossip_frame_tcp(&mut client).expect_err("must reject oversize frame");
        assert!(err.contains("exceeds max"), "unexpected error: {err}");
    }

    #[test]
    fn udp_fallback_serves_transfer_for_requested_item() {
        let _lock = shared_test_env_lock()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let temp_dir = std::env::temp_dir().join(format!(
            "aethos-tauri-test-udp-fallback-{}",
            rand::random::<u64>()
        ));
        let item_id = with_state_dir(&temp_dir, || {
            let identity = ensure_local_identity().expect("ensure identity");
            let signing_seed = load_local_signing_key_seed().expect("load signing seed");
            let payload = build_envelope_payload_b64_from_utf8(
                &identity.wayfarer_id,
                &build_outbound_chat_payload("udp fallback", "local-test", now_unix_ms(), None),
                &signing_seed,
            )
            .expect("build envelope");
            gossip_record_local_payload(&payload, now_unix_ms().saturating_add(60_000))
                .expect("record local payload")
        });

        let sender = UdpSocket::bind(("127.0.0.1", 0)).expect("bind sender udp");
        let receiver = UdpSocket::bind(("127.0.0.1", 0)).expect("bind receiver udp");
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("set receiver timeout");
        let target: SocketAddr = receiver.local_addr().expect("receiver addr");

        with_state_dir(&temp_dir, || {
            serve_udp_transfer_for_request(&sender, target, &[item_id.clone()])
                .expect("serve udp fallback transfer");
        });

        let mut buf = [0u8; 65_535];
        let (len, _) = receiver
            .recv_from(&mut buf)
            .expect("receive transfer datagram");
        let frame = parse_gossip_frame(&buf[..len]).expect("parse transfer frame");
        match frame {
            GossipSyncFrame::Transfer(transfer) => {
                assert!(transfer
                    .objects
                    .iter()
                    .any(|object| object.item_id == item_id));
            }
            other => panic!("expected TRANSFER, got {other:?}"),
        }
    }

    #[test]
    fn lan_hello_frame_includes_tcp_capability() {
        let identity = ensure_local_identity().expect("ensure identity");
        let node_pubkey_raw = base64::engine::general_purpose::STANDARD
            .decode(&identity.verifying_key_b64)
            .expect("decode pubkey");
        let node_pubkey = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(node_pubkey_raw);
        let frame =
            build_lan_hello_frame(&identity.wayfarer_id, &node_pubkey).expect("build hello");

        match frame {
            GossipSyncFrame::Hello(hello) => {
                assert!(hello
                    .capabilities
                    .iter()
                    .any(|capability| capability == LAN_TCP_CAPABILITY));
            }
            other => panic!("expected HELLO, got {other:?}"),
        }
    }

    #[test]
    fn local_dual_state_dirs_have_distinct_wayfarer_ids() {
        let _lock = shared_test_env_lock()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let base = std::env::temp_dir();
        let dir_a = base.join(format!("aethos-local-suite-a-{}", rand::random::<u64>()));
        let dir_b = base.join(format!("aethos-local-suite-b-{}", rand::random::<u64>()));

        let id_a = with_state_dir(&dir_a, || {
            ensure_local_identity().expect("identity a").wayfarer_id
        });
        let id_b = with_state_dir(&dir_b, || {
            ensure_local_identity().expect("identity b").wayfarer_id
        });

        assert_ne!(id_a, id_b);
    }

    #[test]
    fn local_dual_state_dirs_can_exchange_messages_via_reconciliation_suite() {
        let _lock = shared_test_env_lock()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let base = std::env::temp_dir();
        let dir_a = base.join(format!("aethos-local-suite-a-{}", rand::random::<u64>()));
        let dir_b = base.join(format!("aethos-local-suite-b-{}", rand::random::<u64>()));

        let identity_a = with_state_dir(&dir_a, || ensure_local_identity().expect("identity a"));
        let identity_b = with_state_dir(&dir_b, || ensure_local_identity().expect("identity b"));
        assert_ne!(identity_a.wayfarer_id, identity_b.wayfarer_id);

        let transfer_objects = with_state_dir(&dir_a, || {
            let seed = load_local_signing_key_seed().expect("signing seed a");
            let mut objects = Vec::new();
            for idx in 0..9u64 {
                let payload = build_envelope_payload_b64_from_utf8(
                    &identity_b.wayfarer_id,
                    &build_outbound_chat_payload(
                        &format!("suite-a-to-b-{idx}"),
                        &format!("local-a-{idx}"),
                        now_unix_ms().saturating_add(idx),
                        None,
                    ),
                    &seed,
                )
                .expect("build payload a->b");
                let envelope_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(&payload)
                    .expect("decode envelope bytes");
                let digest = sha2::Sha256::digest(&envelope_bytes);
                let item_id = digest
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                objects.push(crate::aethos_core::gossip_sync::TransferObject {
                    item_id,
                    envelope_b64: payload,
                    expiry_unix_ms: now_unix_ms().saturating_add(60_000),
                    hop_count: 1,
                });
            }
            objects
        });

        for object in transfer_objects {
            let single = vec![object];

            with_state_dir(&dir_b, || {
                let result = import_transfer_items(
                    &identity_b.wayfarer_id,
                    Some("local-suite-transport"),
                    Some(&identity_a.wayfarer_id),
                    &single,
                    now_unix_ms(),
                )
                .expect("import on b");
                assert!(
                    !result.accepted_item_ids.is_empty(),
                    "expected accepted item id for imported transfer"
                );

                if !result.new_messages.is_empty() {
                    let mut chat = load_chat_state().expect("load chat b");
                    let mut contacts = load_contact_aliases().expect("load contacts b");
                    let pulled = result
                        .new_messages
                        .into_iter()
                        .map(|item| crate::relay::client::EncounterMessagePreview {
                            author_wayfarer_id: item.author_wayfarer_id,
                            session_peer: item.session_peer,
                            transport_peer: item.transport_peer,
                            item_id: item.item_id,
                            text: item.text,
                            received_at_unix: item.received_at_unix,
                            manifest_id_hex: item.manifest_id_hex,
                        })
                        .collect::<Vec<_>>();
                    merge_pulled_messages(&mut chat, &mut contacts, pulled);
                    save_chat_state(&chat).expect("save chat b");
                    save_contact_aliases(&contacts).expect("save contacts b");
                }
            });
        }

        let chat_b = with_state_dir(&dir_b, || load_chat_state().expect("load chat b"));
        let suite_messages = chat_b
            .threads
            .values()
            .flat_map(|thread| thread.iter())
            .filter(|message| message.text.starts_with("suite-a-to-b-"))
            .count();
        assert!(
            suite_messages >= 8,
            "expected >=8 suite messages, got {suite_messages}"
        );
    }

    #[test]
    fn request_fingerprint_is_stable_and_order_sensitive() {
        let a = vec![
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
        ];
        let b = vec![
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
        ];
        let c = vec![
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ];

        assert_eq!(request_fingerprint(&a), request_fingerprint(&b));
        assert_ne!(request_fingerprint(&a), request_fingerprint(&c));
    }

    #[test]
    #[ignore = "requires AETHOS_RELAY_TEST_HTTP reachable relay target"]
    fn relay_e2e_send_message_path() {
        let Some(relay_http) = maybe_relay_target() else {
            return;
        };

        let temp_dir =
            std::env::temp_dir().join(format!("aethos-tauri-test-send-{}", rand::random::<u64>()));
        std::env::set_var("AETHOS_STATE_DIR", &temp_dir);

        let mut settings = AppSettings::default();
        settings.relay_sync_enabled = true;
        settings.relay_endpoints = vec![relay_http];
        let _ = save_app_settings(&settings).expect("save settings");

        let identity = ensure_local_identity().expect("ensure identity");
        let request = SendMessageRequest {
            wayfarer_id: identity.wayfarer_id,
            body: "e2e relay send test".to_string(),
            attachment: None,
        };

        let response = send_message_blocking(request).expect("send message through relay path");
        assert!(!response.message.msg_id.is_empty());
    }

    #[test]
    #[ignore = "requires AETHOS_RELAY_TEST_HTTP reachable relay target"]
    fn relay_e2e_sync_inbox_path() {
        let Some(relay_http) = maybe_relay_target() else {
            return;
        };

        let temp_dir =
            std::env::temp_dir().join(format!("aethos-tauri-test-sync-{}", rand::random::<u64>()));
        std::env::set_var("AETHOS_STATE_DIR", &temp_dir);

        let mut settings = AppSettings::default();
        settings.relay_sync_enabled = true;
        settings.relay_endpoints = vec![relay_http];
        let _ = save_app_settings(&settings).expect("save settings");

        let response = sync_inbox_blocking().expect("sync inbox through relay path");
        assert!(!response.status.is_empty());
    }
}
