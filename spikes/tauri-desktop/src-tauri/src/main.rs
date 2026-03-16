mod app_state;

#[allow(dead_code)]
#[path = "../../../../src/aethos_core/mod.rs"]
mod aethos_core;
#[allow(dead_code)]
#[path = "../../../../src/relay/mod.rs"]
mod relay;

use std::collections::BTreeMap;
use std::fs;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use app_state::{
    load_app_settings, load_chat_state, normalize_chat_state, now_unix_ms, now_unix_secs,
    save_app_settings, save_chat_state, AppSettings, ChatDirection, ChatMessage, OutboundState,
    PersistedChatState,
};
use base64::Engine;
use image::{imageops::FilterType, ImageBuffer, Luma, Rgba, RgbaImage};
use qrcode::QrCode;
use serde::{Deserialize, Serialize};

use crate::aethos_core::gossip_sync::record_local_payload as gossip_record_local_payload;
use crate::aethos_core::gossip_sync::{
    build_hello_frame as build_gossip_hello_frame,
    build_relay_ingest_frame as build_gossip_relay_ingest_frame,
    build_request_frame as build_gossip_request_frame,
    build_summary_frame as build_gossip_summary_frame, has_item as gossip_has_item,
    import_transfer_items, parse_frame as parse_gossip_frame,
    select_request_item_ids_from_summary as gossip_select_request_item_ids_from_summary,
    serialize_frame as serialize_gossip_frame, transfer_items_for_request as gossip_transfer_items,
    GossipSyncFrame, ReceiptFrame, MAX_TRANSFER_BYTES, GOSSIP_LAN_PORT,
};
use crate::aethos_core::identity_store::{
    delete_wayfarer_id, ensure_local_identity, load_contact_aliases, regenerate_local_identity,
    save_contact_aliases,
};
use crate::aethos_core::logging::{
    app_log_file_path, set_verbose_logging_enabled, verbose_logging_enabled,
};
use crate::aethos_core::protocol::{
    build_envelope_payload_b64_from_utf8, decode_envelope_payload_b64, is_valid_wayfarer_id,
};
use crate::relay::client::{
    connect_to_relay_gossipv1_with_auth, normalize_http_endpoint, run_relay_encounter_gossipv1,
    to_ws_endpoint,
};

const SHARE_QR_FILE_NAME: &str = "share-wayfarer-qr.png";

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
fn bootstrap_state() -> Result<BootstrapState, String> {
    let mut settings = load_app_settings()?;
    if !settings.gossip_sync_enabled {
        settings.gossip_sync_enabled = true;
        settings = save_app_settings(&settings)?;
    }
    set_verbose_logging_enabled(settings.verbose_logging_enabled);
    start_gossip_worker_if_needed(settings.gossip_sync_enabled);
    set_gossip_enabled(settings.gossip_sync_enabled);
    let identity = ensure_local_identity()?;
    let contacts = load_contact_aliases()?;
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
    Ok(saved)
}

#[tauri::command]
fn gossip_status() -> GossipStatus {
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
    let auth = std::env::var("AETHOS_RELAY_AUTH_TOKEN").ok();
    let primary_endpoint = settings
        .relay_endpoints
        .first()
        .map(|value| to_ws_endpoint(&normalize_http_endpoint(value)));
    let secondary_endpoint = settings
        .relay_endpoints
        .get(1)
        .map(|value| to_ws_endpoint(&normalize_http_endpoint(value)));

    let primary_status = primary_endpoint
        .map(|relay_ws| connect_to_relay_gossipv1_with_auth(&relay_ws, &identity, auth.as_deref()))
        .unwrap_or_else(|| "idle".to_string());
    let secondary_status = secondary_endpoint
        .map(|relay_ws| connect_to_relay_gossipv1_with_auth(&relay_ws, &identity, auth.as_deref()))
        .unwrap_or_else(|| "idle".to_string());

    let primary_ok = primary_status.contains("connected + HELLO");
    let secondary_ok = secondary_status.contains("connected + HELLO");
    let (chip_text, chip_state) = match (primary_ok, secondary_ok) {
        (true, true) => ("Relays: healthy (2/2)".to_string(), "ok".to_string()),
        (true, false) | (false, true) => {
            ("Relays: degraded (1/2)".to_string(), "warn".to_string())
        }
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

    let alias = request.alias.trim();
    if alias.is_empty() {
        return Err("alias cannot be empty".to_string());
    }

    let mut contacts = load_contact_aliases()?;
    contacts.insert(request.wayfarer_id, alias.to_string());
    save_contact_aliases(&contacts)?;
    Ok(contacts)
}

#[tauri::command]
fn remove_contact(wayfarer_id: String) -> Result<BTreeMap<String, String>, String> {
    let mut contacts = load_contact_aliases()?;
    contacts.remove(wayfarer_id.trim());
    save_contact_aliases(&contacts)?;
    Ok(contacts)
}

#[tauri::command]
fn save_chat(chat: PersistedChatState) -> Result<PersistedChatState, String> {
    let mut normalized = chat;
    normalize_chat_state(&mut normalized);
    save_chat_state(&normalized)?;
    Ok(normalized)
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
    if body.is_empty() {
        return Err("message body cannot be empty".to_string());
    }

    let settings = load_app_settings()?;
    let identity = ensure_local_identity()?;
    let mut contacts = load_contact_aliases()?;
    let outbound_body = serde_json::json!({
        "text": body,
        "from_wayfarer_id": identity.wayfarer_id,
    })
    .to_string();
    let payload = build_envelope_payload_b64_from_utf8(wayfarer_id, &outbound_body)?;
    let decoded = decode_envelope_payload_b64(&payload)?;

    let now_ms = now_unix_ms();
    let now_secs = now_unix_secs();
    let expiry_ms = now_ms.saturating_add(settings.message_ttl_seconds.saturating_mul(1000));
    let local_id = format!("local-{now_ms}-{:08x}", rand::random::<u32>());
    let item_id = gossip_record_local_payload(&payload, expiry_ms)?;

    let mut chat = load_chat_state()?;
    chat.selected_contact = Some(wayfarer_id.to_string());
    mark_contact_seen(&mut chat, wayfarer_id);

    let thread = chat.threads.entry(wayfarer_id.to_string()).or_default();
    thread.push(ChatMessage {
        msg_id: local_id.clone(),
        text: body.to_string(),
        timestamp: format_timestamp_from_unix(now_secs),
        created_at_unix: now_secs,
        direction: ChatDirection::Outgoing,
        seen: true,
        manifest_id_hex: Some(decoded.manifest_id_hex),
        delivered_at: None,
        outbound_state: Some(OutboundState::Sending),
        expires_at_unix_ms: Some(expiry_ms),
        last_sync_attempt_unix_ms: Some(now_ms),
        last_sync_error: None,
    });

    let relay_http = settings
        .relay_endpoints
        .first()
        .cloned()
        .map(|endpoint| normalize_http_endpoint(&endpoint));
    let relay_enabled = settings.relay_sync_enabled && relay_http.is_some();

    let mut encounter_status = "message queued locally (LAN-only)".to_string();
    let mut pulled_messages_count = 0usize;

    if relay_enabled {
        let relay_ws = to_ws_endpoint(relay_http.as_deref().unwrap_or_default());
        let auth = std::env::var("AETHOS_RELAY_AUTH_TOKEN").ok();
        match run_relay_encounter_gossipv1(&relay_ws, &identity, auth.as_deref(), Some(&item_id)) {
            Ok(report) => {
                pulled_messages_count = report.pulled_messages.len();
                encounter_status = format!(
                    "encounter complete: {} transfer(s), {} pulled",
                    report.transferred_items, pulled_messages_count
                );
                mark_outgoing_message(&mut chat, wayfarer_id, &local_id, &item_id, None);
                merge_pulled_messages(&mut chat, &mut contacts, report.pulled_messages);
            }
            Err(err) => {
                encounter_status = format!("relay encounter failed: {err}");
                mark_outgoing_message(&mut chat, wayfarer_id, &local_id, &item_id, Some(err));
            }
        }
    } else {
        mark_outgoing_message(&mut chat, wayfarer_id, &local_id, &item_id, None);
    }

    save_chat_state(&chat)?;
    save_contact_aliases(&contacts)?;

    let message = latest_message_for_contact(&chat, wayfarer_id, &item_id, &local_id)
        .ok_or_else(|| "failed to load stored outbound message".to_string())?;

    Ok(SendMessageResponse {
        message,
        chat,
        contacts,
        encounter_status,
        pulled_messages: pulled_messages_count,
    })
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

    let relay_http = settings
        .relay_endpoints
        .first()
        .cloned()
        .ok_or_else(|| "no relay endpoints configured".to_string())
        .map(|endpoint| normalize_http_endpoint(&endpoint))?;
    let relay_ws = to_ws_endpoint(&relay_http);
    let identity = ensure_local_identity()?;
    let auth = std::env::var("AETHOS_RELAY_AUTH_TOKEN").ok();

    let report = run_relay_encounter_gossipv1(&relay_ws, &identity, auth.as_deref(), None)?;

    let mut chat = load_chat_state()?;
    let mut contacts = load_contact_aliases()?;
    let pulled = report.pulled_messages.len();
    merge_pulled_messages(&mut chat, &mut contacts, report.pulled_messages);
    save_chat_state(&chat)?;
    save_contact_aliases(&contacts)?;

    Ok(SyncInboxResponse {
        chat,
        contacts,
        pulled_messages: pulled,
        status: if pulled == 0 {
            "inbox sync complete (no new messages)".to_string()
        } else {
            format!("inbox sync complete ({} new message(s))", pulled)
        },
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

fn start_gossip_worker_if_needed(initial_enabled: bool) {
    let runtime = gossip_runtime(initial_enabled);
    if runtime.running.swap(true, Ordering::SeqCst) {
        return;
    }

    thread::spawn(|| {
        let runtime = gossip_runtime(false);
        let socket = match UdpSocket::bind(("0.0.0.0", GOSSIP_LAN_PORT)) {
            Ok(socket) => socket,
            Err(err) => {
                set_gossip_event(&format!("udp bind failed: {err}"));
                runtime.running.store(false, Ordering::SeqCst);
                return;
            }
        };

        if let Err(err) = socket.set_nonblocking(true) {
            set_gossip_event(&format!("set_nonblocking failed: {err}"));
            runtime.running.store(false, Ordering::SeqCst);
            return;
        }
        if let Err(err) = socket.set_broadcast(true) {
            set_gossip_event(&format!("set_broadcast failed: {err}"));
            runtime.running.store(false, Ordering::SeqCst);
            return;
        }

        set_gossip_event("listening");
        let mut peer_node_by_addr: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut last_inventory_broadcast = Instant::now() - Duration::from_secs(10);

        loop {
            if !runtime.enabled.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(120));
                continue;
            }

            let force_announce = runtime.force_announce.swap(false, Ordering::SeqCst);
            if force_announce || last_inventory_broadcast.elapsed() >= Duration::from_secs(3) {
                if gossip_broadcast_inventory(&socket).is_ok() {
                    runtime.last_activity_ms.store(now_unix_ms(), Ordering::SeqCst);
                    set_gossip_event("active");
                }
                last_inventory_broadcast = Instant::now();
            }

            let mut buf = [0u8; 65_535];
            match socket.recv_from(&mut buf) {
                Ok((len, source)) => {
                    runtime.last_activity_ms.store(now_unix_ms(), Ordering::SeqCst);
                    set_gossip_event("active");
                    let _ = handle_gossip_frame(
                        &socket,
                        &buf[..len],
                        source,
                        &mut peer_node_by_addr,
                        runtime,
                    );
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(30));
                }
                Err(err) => {
                    set_gossip_event(&format!("recv error: {err}"));
                    thread::sleep(Duration::from_millis(80));
                }
            }
        }
    });
}

fn gossip_broadcast_inventory(socket: &UdpSocket) -> Result<(), String> {
    let identity = ensure_local_identity()?;
    let node_pubkey_raw = base64::engine::general_purpose::STANDARD
        .decode(&identity.verifying_key_b64)
        .map_err(|err| format!("gossip pubkey decode failed: {err}"))?;
    let node_pubkey = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(node_pubkey_raw);

    if let Ok(hello) = build_gossip_hello_frame(&identity.wayfarer_id, &node_pubkey) {
        let _ = send_gossip_frame(socket, "255.255.255.255", GOSSIP_LAN_PORT, &hello);
    }
    if let Ok(summary) = build_gossip_summary_frame(now_unix_ms()) {
        let _ = send_gossip_frame(socket, "255.255.255.255", GOSSIP_LAN_PORT, &summary);
    }
    if let Ok(ingest) = build_gossip_relay_ingest_frame(now_unix_ms()) {
        let _ = send_gossip_frame(socket, "255.255.255.255", GOSSIP_LAN_PORT, &ingest);
    }
    Ok(())
}

fn handle_gossip_frame(
    socket: &UdpSocket,
    raw: &[u8],
    source: std::net::SocketAddr,
    peer_node_by_addr: &mut std::collections::HashMap<String, String>,
    runtime: &GossipRuntime,
) -> Result<(), String> {
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
            if let Ok(summary) = build_gossip_summary_frame(now_unix_ms()) {
                let _ = send_gossip_frame(socket, &source.ip().to_string(), source.port(), &summary);
            }
            if let Ok(ingest) = build_gossip_relay_ingest_frame(now_unix_ms()) {
                let _ = send_gossip_frame(socket, &source.ip().to_string(), source.port(), &ingest);
            }
        }
        GossipSyncFrame::Summary(summary) => {
            let want = gossip_select_request_item_ids_from_summary(&summary, 256)?;
            let request = build_gossip_request_frame(want, 256)?;
            let _ = send_gossip_frame(socket, &source.ip().to_string(), source.port(), &request);
        }
        GossipSyncFrame::RelayIngest(ingest) => {
            let mut missing_item_ids = ingest
                .item_ids
                .into_iter()
                .filter(|item_id| gossip_has_item(item_id).map(|have| !have).unwrap_or(false))
                .collect::<Vec<_>>();
            missing_item_ids.sort();
            let request = build_gossip_request_frame(missing_item_ids, 256)?;
            let _ = send_gossip_frame(socket, &source.ip().to_string(), source.port(), &request);
        }
        GossipSyncFrame::Request(req) => {
            let objects = gossip_transfer_items(&req.want, 32, MAX_TRANSFER_BYTES, now_unix_ms())
                .unwrap_or_default();
            let transfer = GossipSyncFrame::Transfer(crate::aethos_core::gossip_sync::TransferFrame {
                objects,
            });
            let _ = send_gossip_frame(socket, &source.ip().to_string(), source.port(), &transfer);
        }
        GossipSyncFrame::Transfer(transfer) => {
            let peer_node_id = peer_node_by_addr
                .get(&source_key)
                .or_else(|| peer_node_by_addr.get(&source_ip_key))
                .cloned()
                .unwrap_or_else(|| source.ip().to_string());
            let result = import_transfer_items(
                &peer_node_id,
                &local_wayfarer,
                &transfer.objects,
                now_unix_ms(),
            )?;

            if !result.new_messages.is_empty() {
                let mut chat = load_chat_state()?;
                let mut contacts = load_contact_aliases()?;
                let pulled = result
                    .new_messages
                    .into_iter()
                    .filter_map(|item| {
                        let sender = resolve_sender_wayfarer_from_import(
                            &item.from_wayfarer_id,
                            &item.text,
                            &peer_node_id,
                        )?;
                        Some(crate::relay::client::EncounterMessagePreview {
                            from_node_id: sender,
                            item_id: item.item_id,
                            text: item.text,
                            received_at_unix: item.received_at_unix,
                            manifest_id_hex: item.manifest_id_hex,
                        })
                    })
                    .collect::<Vec<_>>();
                merge_pulled_messages(&mut chat, &mut contacts, pulled);
                save_chat_state(&chat)?;
                save_contact_aliases(&contacts)?;
                runtime.last_activity_ms.store(now_unix_ms(), Ordering::SeqCst);
                set_gossip_event("received messages");
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

fn send_gossip_frame(socket: &UdpSocket, host: &str, port: u16, frame: &GossipSyncFrame) -> Result<(), String> {
    let raw = serialize_gossip_frame(frame)?;
    let addr = format!("{host}:{port}");
    socket
        .send_to(&raw, &addr)
        .map(|_| ())
        .map_err(|err| format!("gossip send failed ({addr}): {err}"))
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
                message.outbound_state = Some(OutboundState::Failed { error: err.clone() });
                message.last_sync_error = Some(err);
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
    for pulled in pulled_messages {
        if let Some(receipt_manifest) = extract_receipt_manifest_id(&pulled.text) {
            apply_delivery_receipt(chat, &pulled.from_node_id, &receipt_manifest, pulled.received_at_unix);
            continue;
        }

        let is_new_contact = !contacts.contains_key(&pulled.from_node_id);
        if is_new_contact {
            contacts.insert(pulled.from_node_id.clone(), pulled.from_node_id.clone());
            if !chat.new_contacts.iter().any(|value| value == &pulled.from_node_id) {
                chat.new_contacts.push(pulled.from_node_id.clone());
            }
        }

        let seen_on_insert = chat.selected_contact.as_deref() == Some(pulled.from_node_id.as_str());

        let thread = chat.threads.entry(pulled.from_node_id.clone()).or_default();
        let exists = thread.iter().any(|existing| {
            existing.msg_id == pulled.item_id
                || (pulled.manifest_id_hex.is_some()
                    && existing.manifest_id_hex == pulled.manifest_id_hex)
        });
        if exists {
            continue;
        }

        thread.push(ChatMessage {
            msg_id: pulled.item_id,
            text: extract_chat_text_if_json(&pulled.text),
            timestamp: format_timestamp_from_unix(pulled.received_at_unix),
            created_at_unix: pulled.received_at_unix,
            direction: ChatDirection::Incoming,
            seen: seen_on_insert,
            manifest_id_hex: pulled.manifest_id_hex,
            delivered_at: None,
            outbound_state: None,
            expires_at_unix_ms: None,
            last_sync_attempt_unix_ms: None,
            last_sync_error: None,
        });

        if chat.selected_contact.is_none() {
            chat.selected_contact = Some(pulled.from_node_id.clone());
            mark_contact_seen(chat, &pulled.from_node_id);
        }
    }

    normalize_chat_state(chat);
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
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
        return input.to_string();
    };

    value
        .get("text")
        .and_then(|v| v.as_str())
        .map(|text| text.to_string())
        .unwrap_or_else(|| input.to_string())
}

fn resolve_sender_wayfarer_from_import(
    import_sender: &str,
    text: &str,
    peer_sender_hint: &str,
) -> Option<String> {
    if let Some(from_json) = extract_sender_wayfarer_id_from_text(text) {
        return Some(from_json);
    }

    if is_valid_wayfarer_id(import_sender) {
        return Some(import_sender.to_string());
    }

    if is_valid_wayfarer_id(peer_sender_hint) {
        return Some(peer_sender_hint.to_string());
    }

    None
}

fn extract_sender_wayfarer_id_from_text(input: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(input).ok()?;
    let candidate = value
        .get("from_wayfarer_id")
        .and_then(|item| item.as_str())
        .or_else(|| value.get("fromWayfarerId").and_then(|item| item.as_str()))
        .or_else(|| value.get("sender_wayfarer_id").and_then(|item| item.as_str()))
        .or_else(|| value.get("senderWayfarerId").and_then(|item| item.as_str()))?;
    let normalized = candidate.trim().to_ascii_lowercase();
    if is_valid_wayfarer_id(&normalized) {
        Some(normalized)
    } else {
        None
    }
}

fn extract_receipt_manifest_id(input: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(input).ok()?;
    let candidate = value
        .get("receipt_manifest_id")
        .and_then(|item| item.as_str())
        .or_else(|| value.get("receiptManifestId").and_then(|item| item.as_str()))
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            app_diagnostics,
            bootstrap_state,
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
            run_relay_diagnostics
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn main() {
    run();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maybe_relay_target() -> Option<String> {
        std::env::var("AETHOS_RELAY_TEST_HTTP")
            .ok()
            .filter(|value| !value.trim().is_empty())
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
