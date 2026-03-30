use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::aethos_core::gossip_sync::record_local_payload as gossip_record_local_payload;
use crate::aethos_core::logging::log_verbose;
use crate::aethos_core::protocol::{
    build_envelope_payload_b64_from_utf8, bytes_to_hex_lower, is_valid_wayfarer_id,
};
use crate::app_state::{
    load_chat_state, now_unix_ms, save_chat_state, ChatAttachment, ChatDirection,
    ChatMediaTransfer, ChatMessage, MediaTransferStatus, PersistedChatState,
};
use crate::relay::client::EncounterMessagePreview;

const MEDIA_CAPABILITIES_TYPE: &str = "wayfarer.media.capabilities.v1";
const MEDIA_MANIFEST_TYPE: &str = "wayfarer.media_manifest.v1";
const MEDIA_CHUNK_TYPE: &str = "wayfarer.media.chunk.v1";
const MEDIA_MISSING_TYPE: &str = "wayfarer.media.missing.v1";

const MAX_ITEM_PAYLOAD_B64_BYTES_DEFAULT: usize = 64 * 1024;
const MAX_OBJECT_BYTES: u64 = 128 * 1024 * 1024;
#[allow(dead_code)] // Reserved for pending inbound spool quota enforcement wiring.
const SPOOL_GLOBAL_PARTIAL_BYTES: u64 = 1024 * 1024 * 1024;
#[allow(dead_code)] // Reserved for pending inbound spool quota enforcement wiring.
const SPOOL_PER_PEER_PARTIAL_BYTES: u64 = 256 * 1024 * 1024;
const ORPHAN_PER_PEER_BYTES: u64 = 16 * 1024 * 1024;
const ORPHAN_GLOBAL_BYTES: u64 = 128 * 1024 * 1024;
const ORPHAN_MAX_PEERS: usize = 32;
const ORPHAN_PER_PEER_META_FILES: u64 = 2048;
const ORPHAN_GLOBAL_META_FILES: u64 = 32 * 1024;
const ORPHAN_MAX_AGE_MS: u64 = 30 * 60 * 1000;
const ORPHAN_SWEEP_MIN_INTERVAL_MS: u64 = 15 * 1000;
const MISSING_RANGE_CAP: usize = 256;
const MISSING_MIN_INTERVAL_MS_DEFAULT: u64 = 1200;
const MISSING_REQUEST_CHUNK_TARGET: usize = 128;
const MISSING_FASTLANE_REDUNDANCY_DEFAULT: usize = 2;
const COMPLETE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const COMPLETE_MAX_AGE_MS: u64 = 90 * 24 * 60 * 60 * 1000;
const COMPLETE_INLINE_MAX_BYTES: u64 = 5 * 1024 * 1024;
const WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN: f64 = (8 * 1024 * 1024) as f64;
const WIRE_BUCKET_BURST_BYTES: f64 = (16 * 1024 * 1024) as f64;
const MANIFESTS_PER_HOUR_LIMIT: usize = 30;
const CHUNKS_PER_MINUTE_LIMIT: usize = 1200;
#[allow(dead_code)] // Reserved for pending corrupt-chunk abort handling wiring.
const CORRUPT_CHUNK_ABORT_STRIKES: u8 = 3;
#[allow(dead_code)] // Reserved for pending initial outbound media send burst wiring.
const INITIAL_OUTBOUND_CHUNK_CEILING: usize = 128;
#[allow(dead_code)] // Reserved for pending missing-response fastlane wiring.
const MISSING_RESPONSE_CHUNK_CEILING: usize = 64;
const MEDIA_CONTROL_FASTLANE_QUEUE_MAX: usize = 512;
#[allow(dead_code)] // Reserved for pending missing-response fastlane dedup wiring.
const MISSING_RESPONSE_CHUNK_DEDUP_WINDOW_MS: u64 = 8_000;
const E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN_DEFAULT: u64 = 64 * 1024 * 1024;
const E2E_WIRE_BUCKET_BURST_BYTES_DEFAULT: u64 = 64 * 1024 * 1024;
#[allow(dead_code)] // Reserved for pending E2E missing-response fastlane tuning.
const E2E_MISSING_RESPONSE_CHUNK_DEDUP_WINDOW_MS_DEFAULT: u64 = 1_500;
#[allow(dead_code)] // Reserved for pending E2E missing-response fastlane tuning.
const E2E_MISSING_RESPONSE_CHUNK_CEILING_DEFAULT: usize = 64;
const E2E_CHUNKS_PER_MINUTE_LIMIT_DEFAULT: usize = 4_000;

#[derive(Debug, Clone, Copy)]
pub enum PendingMediaControlKind {
    MissingRequest,
    ChunkResponse,
}

#[derive(Debug, Clone)]
pub struct PendingMediaControlUnicast {
    pub peer_wayfarer_id: String,
    pub item_id: String,
    pub payload_b64: String,
    pub expiry_unix_ms: u64,
    pub kind: PendingMediaControlKind,
    pub transfer_id: Option<String>,
    pub tcp_mirror: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletedMediaPayload {
    pub object_sha256_hex: String,
    pub file_name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub content_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletedMediaPathPayload {
    pub path: String,
    pub mime: String,
    pub size_bytes: u64,
}

#[allow(dead_code)] // Reserved for pending outbound media send API wiring.
#[derive(Debug, Clone)]
pub struct OutboundMediaSendResult {
    pub item_id: String,
    pub transfer_id: String,
    pub object_sha256_hex: String,
    pub file_name: String,
    pub mime_type: String,
    pub total_bytes: u64,
    pub chunk_count: u32,
    pub expires_at_unix_ms: u64,
}

#[allow(dead_code)] // Reserved for pending incoming media message pipeline wiring.
#[derive(Debug, Clone)]
pub enum MediaMessageProcess {
    NotMedia,
    HandledSuppressed { chat_updated: bool },
    HandledManifest { chat_updated: bool },
}

#[allow(dead_code)] // Reserved for pending media capability handshake wiring.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaCapabilitiesMessage {
    #[serde(rename = "type")]
    msg_type: String,
    media_v1: bool,
    max_item_payload_b64_bytes: u64,
    sent_at_unix_ms: u64,
}

#[allow(dead_code)] // Reserved for pending media manifest ingest/send wiring.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaManifestMessage {
    #[serde(rename = "type")]
    msg_type: String,
    transfer_id: String,
    object_sha256_hex: String,
    file_name: String,
    mime_type: String,
    total_bytes: u64,
    chunk_payload_bytes: u32,
    chunk_count: u32,
    expires_at_unix_ms: u64,
    sent_at_unix_ms: u64,
    #[serde(default)]
    caption: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaChunkMessage {
    #[serde(rename = "type")]
    msg_type: String,
    transfer_id: String,
    object_sha256_hex: String,
    chunk_index: u32,
    chunk_sha256_hex: String,
    chunk_bytes: u32,
    payload_b64: String,
    expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChunkRange {
    start: u32,
    end: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaMissingMessage {
    #[serde(rename = "type")]
    msg_type: String,
    transfer_id: String,
    object_sha256_hex: String,
    ranges: Vec<ChunkRange>,
    sent_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CapabilitiesCache {
    peers: BTreeMap<String, PeerCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PeerCapability {
    media_v1: bool,
    max_item_payload_b64_bytes: u64,
    updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IncomingTransferState {
    transfer_id: String,
    sender_wayfarer_id: String,
    object_sha256_hex: String,
    file_name: String,
    mime_type: String,
    total_bytes: u64,
    chunk_payload_bytes: u32,
    chunk_count: u32,
    expires_at_unix_ms: u64,
    sent_at_unix_ms: u64,
    #[serde(default)]
    caption: Option<String>,
    received_chunks: u32,
    received_bytes: u64,
    #[serde(default)]
    strikes: BTreeMap<u32, u8>,
    #[serde(default)]
    last_missing_unix_ms: u64,
    #[serde(default)]
    failed_error: Option<String>,
    #[serde(default)]
    completed_unix_ms: Option<u64>,
}

#[allow(dead_code)] // Reserved for pending outbound media resend state wiring.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutboundTransferState {
    transfer_id: String,
    recipient_wayfarer_id: String,
    object_sha256_hex: String,
    file_name: String,
    mime_type: String,
    total_bytes: u64,
    chunk_payload_bytes: u32,
    chunk_count: u32,
    expires_at_unix_ms: u64,
    sent_at_unix_ms: u64,
    #[serde(default)]
    caption: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompletedTransferMeta {
    object_sha256_hex: String,
    file_name: String,
    mime_type: String,
    size_bytes: u64,
    completed_at_unix_ms: u64,
    #[serde(default)]
    last_access_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SpoolUsageCache {
    global_partial_bytes: u64,
    #[serde(default)]
    per_peer_partial_bytes: BTreeMap<String, u64>,
    #[serde(default)]
    updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrphanChunkMeta {
    sender_wayfarer_id: String,
    transfer_id: String,
    object_sha256_hex: String,
    chunk_index: u32,
    chunk_bytes: u32,
    payload_b64_len: u64,
    expires_at_unix_ms: u64,
    stored_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct OrphanUsageCache {
    global_declared_bytes: u64,
    global_meta_files: u64,
    #[serde(default)]
    per_peer: BTreeMap<String, OrphanPeerUsage>,
    #[serde(default)]
    updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct OrphanPeerUsage {
    declared_bytes: u64,
    meta_files: u64,
}

#[allow(dead_code)] // Reserved for pending media wire message dispatch wiring.
#[derive(Debug, Clone)]
enum ParsedMediaMessage {
    Capabilities(MediaCapabilitiesMessage),
    Manifest(MediaManifestMessage),
    Chunk(MediaChunkMessage),
    Missing(MediaMissingMessage),
}

#[derive(Debug, Clone)]
struct MediaLimits {
    max_item_payload_b64_bytes: usize,
    missing_min_interval_ms: u64,
}

#[derive(Debug)]
struct OutboundLimiter {
    tokens: f64,
    last_refill_unix_ms: u64,
    manifest_sends: VecDeque<u64>,
    chunk_sends: VecDeque<u64>,
}

#[derive(Debug, Clone)]
struct MissingRequestPlan {
    ranges: Vec<ChunkRange>,
    selected_chunks: Vec<u32>,
    missing_total: usize,
    cursor_start: u32,
    cursor_next: u32,
}

#[derive(Debug, Clone)]
struct MissingRequestYieldState {
    requested_at_unix_ms: u64,
    requested_chunks: BTreeMap<u32, bool>,
    delivered_chunks: usize,
}

impl Default for OutboundLimiter {
    fn default() -> Self {
        Self {
            tokens: wire_bucket_burst_bytes(),
            last_refill_unix_ms: now_unix_ms(),
            manifest_sends: VecDeque::new(),
            chunk_sends: VecDeque::new(),
        }
    }
}

fn outbound_limiter() -> &'static Mutex<OutboundLimiter> {
    static LIMITER: OnceLock<Mutex<OutboundLimiter>> = OnceLock::new();
    LIMITER.get_or_init(|| Mutex::new(OutboundLimiter::default()))
}

fn pending_media_control_unicast_queue() -> &'static Mutex<VecDeque<PendingMediaControlUnicast>> {
    static QUEUE: OnceLock<Mutex<VecDeque<PendingMediaControlUnicast>>> = OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn media_control_fastlane_queue_max() -> usize {
    let default_limit = std::env::var("AETHOS_E2E")
        .ok()
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .map(|enabled| {
            if enabled {
                8_192
            } else {
                MEDIA_CONTROL_FASTLANE_QUEUE_MAX
            }
        })
        .unwrap_or(MEDIA_CONTROL_FASTLANE_QUEUE_MAX);
    std::env::var("AETHOS_MEDIA_CONTROL_FASTLANE_QUEUE_MAX")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(128, 65_536))
        .unwrap_or(default_limit)
}

fn queue_pending_media_control_unicast(envelope: PendingMediaControlUnicast) {
    let Ok(mut queue) = pending_media_control_unicast_queue().lock() else {
        return;
    };
    if queue.len() >= media_control_fastlane_queue_max() {
        let _ = queue.pop_front();
    }
    queue.push_back(envelope);
}

fn missing_fastlane_redundancy() -> usize {
    std::env::var("AETHOS_MEDIA_MISSING_FASTLANE_REDUNDANCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, 4))
        .unwrap_or(MISSING_FASTLANE_REDUNDANCY_DEFAULT)
}

fn missing_request_tcp_mirror_stall_ms() -> u64 {
    std::env::var("AETHOS_MEDIA_MISSING_TCP_MIRROR_STALL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.clamp(300, 10_000))
        .unwrap_or(900)
}

pub fn drain_pending_media_control_unicast(max_items: usize) -> Vec<PendingMediaControlUnicast> {
    let limit = max_items.max(1);
    let Ok(mut queue) = pending_media_control_unicast_queue().lock() else {
        return Vec::new();
    };
    let mut drained = Vec::new();
    while drained.len() < limit {
        let Some(item) = queue.pop_front() else {
            break;
        };
        drained.push(item);
    }
    drained
}

fn max_base64url_nopad_len(decoded_len: usize) -> usize {
    let full_groups = decoded_len / 3;
    let remainder = decoded_len % 3;
    full_groups
        .saturating_mul(4)
        .saturating_add(match remainder {
            0 => 0,
            1 => 2,
            _ => 3,
        })
}

fn max_decoded_len_from_base64url_len(encoded_len: usize) -> Option<usize> {
    let full_groups = encoded_len / 4;
    let remainder = encoded_len % 4;
    if remainder == 1 {
        return None;
    }
    let remainder_bytes = match remainder {
        0 => 0,
        2 => 1,
        3 => 2,
        _ => 0,
    };
    Some(
        full_groups
            .saturating_mul(3)
            .saturating_add(remainder_bytes),
    )
}

fn validate_chunk_predecode_bounds(
    payload_b64_len: usize,
    declared_chunk_bytes: usize,
    expected_chunk_bytes: usize,
    chunk_payload_bytes: usize,
    max_item_payload_b64_bytes: usize,
) -> Result<(), &'static str> {
    if payload_b64_len == 0 {
        return Err("chunk_payload_empty");
    }
    if payload_b64_len > max_item_payload_b64_bytes {
        return Err("chunk_payload_b64_too_large");
    }
    if declared_chunk_bytes == 0 {
        return Err("chunk_declared_length_zero");
    }
    if expected_chunk_bytes > chunk_payload_bytes || declared_chunk_bytes > chunk_payload_bytes {
        return Err("chunk_declared_exceeds_chunk_payload_bytes");
    }
    if declared_chunk_bytes != expected_chunk_bytes {
        return Err("chunk_expected_length_mismatch");
    }
    let strict_ceiling = max_base64url_nopad_len(expected_chunk_bytes);
    if payload_b64_len > strict_ceiling {
        return Err("chunk_payload_b64_exceeds_expected_length");
    }
    let decoded_cap = max_decoded_len_from_base64url_len(payload_b64_len)
        .ok_or("chunk_payload_b64_invalid_length")?;
    if decoded_cap < expected_chunk_bytes {
        return Err("chunk_payload_b64_too_short");
    }
    Ok(())
}

fn validate_orphan_chunk_predecode_bounds(
    chunk: &MediaChunkMessage,
    max_item_payload_b64_bytes: usize,
) -> Result<(), &'static str> {
    if chunk.payload_b64.is_empty() {
        return Err("orphan_chunk_payload_empty");
    }
    if chunk.payload_b64.len() > max_item_payload_b64_bytes {
        return Err("orphan_chunk_payload_b64_too_large");
    }
    if chunk.chunk_bytes == 0 {
        return Err("orphan_chunk_declared_length_zero");
    }
    let declared = chunk.chunk_bytes as usize;
    let strict_ceiling = max_base64url_nopad_len(declared);
    if chunk.payload_b64.len() > strict_ceiling {
        return Err("orphan_chunk_payload_b64_exceeds_declared_length");
    }
    let decoded_cap = max_decoded_len_from_base64url_len(chunk.payload_b64.len())
        .ok_or("orphan_chunk_payload_b64_invalid_length")?;
    if decoded_cap < declared {
        return Err("orphan_chunk_payload_b64_too_short");
    }
    Ok(())
}

fn transfer_status_and_error(
    transfer: &IncomingTransferState,
) -> (MediaTransferStatus, Option<String>) {
    if let Some(error) = transfer.failed_error.clone() {
        return (MediaTransferStatus::Failed, Some(error));
    }
    if transfer.completed_unix_ms.is_some() {
        return (MediaTransferStatus::Complete, None);
    }
    (MediaTransferStatus::Receiving, None)
}

fn transfer_is_terminal(transfer: &IncomingTransferState) -> bool {
    transfer.failed_error.is_some() || transfer.completed_unix_ms.is_some()
}

fn negotiated_payload_b64_limit_for_sender(sender_wayfarer_id: &str) -> usize {
    let local = media_limits().max_item_payload_b64_bytes;
    let Ok(cache) = load_capabilities_cache() else {
        return local;
    };
    let Some(peer) = cache.peers.get(sender_wayfarer_id) else {
        return local;
    };
    local.min(peer.max_item_payload_b64_bytes as usize)
}

#[allow(dead_code)] // Reserved for pending missing-request interval gate wiring.
fn missing_request_tracker() -> &'static Mutex<HashMap<String, u64>> {
    static TRACKER: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    TRACKER.get_or_init(|| Mutex::new(HashMap::new()))
}

fn missing_request_cursor_tracker() -> &'static Mutex<HashMap<String, u32>> {
    static TRACKER: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    TRACKER.get_or_init(|| Mutex::new(HashMap::new()))
}

fn missing_request_yield_tracker() -> &'static Mutex<HashMap<String, MissingRequestYieldState>> {
    static TRACKER: OnceLock<Mutex<HashMap<String, MissingRequestYieldState>>> = OnceLock::new();
    TRACKER.get_or_init(|| Mutex::new(HashMap::new()))
}

fn note_missing_request_plan(
    sender_wayfarer_id: &str,
    transfer_id: &str,
    selected_chunks: &[u32],
    now_ms: u64,
) -> Option<(u64, usize, usize, usize)> {
    let key = format!("{sender_wayfarer_id}:{transfer_id}");
    let Ok(mut tracker) = missing_request_yield_tracker().lock() else {
        return None;
    };
    let previous = tracker.remove(&key).map(|state| {
        let requested_chunks = state
            .requested_chunks
            .len()
            .saturating_add(state.delivered_chunks);
        let pending_chunks = state.requested_chunks.len();
        (
            now_ms.saturating_sub(state.requested_at_unix_ms),
            requested_chunks,
            state.delivered_chunks,
            pending_chunks,
        )
    });

    let mut requested = BTreeMap::new();
    for chunk_index in selected_chunks {
        requested.insert(*chunk_index, true);
    }
    tracker.insert(
        key,
        MissingRequestYieldState {
            requested_at_unix_ms: now_ms,
            requested_chunks: requested,
            delivered_chunks: 0,
        },
    );
    previous
}

fn note_missing_request_chunk_delivered(
    sender_wayfarer_id: &str,
    transfer_id: &str,
    chunk_index: u32,
    now_ms: u64,
) -> Option<(u64, usize, usize, usize)> {
    let key = format!("{sender_wayfarer_id}:{transfer_id}");
    let Ok(mut tracker) = missing_request_yield_tracker().lock() else {
        return None;
    };
    let state = tracker.get_mut(&key)?;
    if state.requested_chunks.remove(&chunk_index).is_none() {
        return None;
    }
    state.delivered_chunks = state.delivered_chunks.saturating_add(1);
    let requested_chunks = state
        .requested_chunks
        .len()
        .saturating_add(state.delivered_chunks);
    let pending_chunks = state.requested_chunks.len();
    Some((
        now_ms.saturating_sub(state.requested_at_unix_ms),
        requested_chunks,
        state.delivered_chunks,
        pending_chunks,
    ))
}

fn clear_missing_request_yield(sender_wayfarer_id: &str, transfer_id: &str) {
    let key = format!("{sender_wayfarer_id}:{transfer_id}");
    if let Ok(mut tracker) = missing_request_yield_tracker().lock() {
        tracker.remove(&key);
    }
}

#[allow(dead_code)] // Reserved for pending missing-response dedup tracker wiring.
fn missing_response_chunk_tracker() -> &'static Mutex<HashMap<String, HashMap<u32, u64>>> {
    static TRACKER: OnceLock<Mutex<HashMap<String, HashMap<u32, u64>>>> = OnceLock::new();
    TRACKER.get_or_init(|| Mutex::new(HashMap::new()))
}

#[allow(dead_code)] // Reserved for pending missing-response fastlane dedup wiring.
fn missing_response_chunk_recently_sent(key: &str, chunk_index: u32, now_ms: u64) -> bool {
    let Ok(mut tracker) = missing_response_chunk_tracker().lock() else {
        return false;
    };
    let dedup_window_ms = missing_response_chunk_dedup_window_ms();
    for chunks in tracker.values_mut() {
        chunks.retain(|_, sent_at| now_ms.saturating_sub(*sent_at) <= dedup_window_ms);
    }
    tracker.retain(|_, chunks| !chunks.is_empty());
    tracker
        .get(key)
        .and_then(|chunks| chunks.get(&chunk_index).copied())
        .map(|sent_at| now_ms.saturating_sub(sent_at) <= dedup_window_ms)
        .unwrap_or(false)
}

#[allow(dead_code)] // Reserved for pending missing-response fastlane dedup wiring.
fn note_missing_response_chunk_sent(key: &str, chunk_index: u32, now_ms: u64) {
    let Ok(mut tracker) = missing_response_chunk_tracker().lock() else {
        return;
    };
    tracker
        .entry(key.to_string())
        .or_insert_with(HashMap::new)
        .insert(chunk_index, now_ms);
}

#[allow(dead_code)] // Reserved for pending composer-side media candidate filtering.
pub fn is_media_candidate_attachment(attachment: &ChatAttachment) -> bool {
    let mime = attachment.mime_type.trim().to_ascii_lowercase();
    mime.starts_with("image/")
}

pub fn max_object_bytes() -> u64 {
    MAX_OBJECT_BYTES
}

#[allow(dead_code)] // Reserved for pending composer-side media wire payload guards.
pub fn is_media_wire_message(input: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
        return false;
    };
    let Some(msg_type) = value.get("type").and_then(|item| item.as_str()) else {
        return false;
    };
    matches!(
        msg_type,
        MEDIA_CAPABILITIES_TYPE | MEDIA_MANIFEST_TYPE | MEDIA_CHUNK_TYPE | MEDIA_MISSING_TYPE
    )
}

#[allow(dead_code)] // Reserved for pending outbound media capability handshake wiring.
pub fn maybe_queue_capabilities_for_peer(
    peer_wayfarer_id: &str,
    author_signing_seed: &[u8; 32],
    ttl_seconds_max: u64,
) -> Result<(), String> {
    if !is_valid_wayfarer_id(peer_wayfarer_id) {
        return Ok(());
    }

    let now_ms = now_unix_ms();
    let limits = media_limits();
    let body = serde_json::to_string(&MediaCapabilitiesMessage {
        msg_type: MEDIA_CAPABILITIES_TYPE.to_string(),
        media_v1: true,
        max_item_payload_b64_bytes: limits.max_item_payload_b64_bytes as u64,
        sent_at_unix_ms: now_ms,
    })
    .map_err(|err| format!("failed serializing media capabilities message: {err}"))?;
    let payload =
        build_envelope_payload_b64_from_utf8(peer_wayfarer_id, &body, author_signing_seed)?;
    let expiry = now_ms.saturating_add(ttl_seconds_max.saturating_mul(1000));
    let expiry_unix_ms = apply_expiry_authority(expiry, now_ms, ttl_seconds_max);
    rate_limit_outbound(payload.len(), now_ms, false, false)?;
    let _ = gossip_record_local_payload(&payload, expiry_unix_ms)?;
    Ok(())
}

#[allow(dead_code)] // Reserved for pending outbound media capability gate wiring.
pub fn peer_supports_media_v1(peer_wayfarer_id: &str) -> Result<bool, String> {
    let cache = load_capabilities_cache()?;
    Ok(cache
        .peers
        .get(peer_wayfarer_id)
        .map(|entry| entry.media_v1)
        .unwrap_or(false))
}

#[allow(dead_code)] // Reserved for pending outbound media manifest/chunk publish wiring.
pub fn send_media_manifest_and_chunks(
    to_wayfarer_id: &str,
    caption: &str,
    attachment: &ChatAttachment,
    author_signing_seed: &[u8; 32],
    ttl_seconds_max: u64,
) -> Result<OutboundMediaSendResult, String> {
    if !is_valid_wayfarer_id(to_wayfarer_id) {
        return Err("invalid wayfarer_id; expected 64 lowercase hex chars".to_string());
    }

    if !peer_supports_media_v1(to_wayfarer_id)? {
        let _ =
            maybe_queue_capabilities_for_peer(to_wayfarer_id, author_signing_seed, ttl_seconds_max);
        return Err("peer media capability unavailable; waiting for mediaV1 handshake".to_string());
    }

    let content_b64 = attachment
        .content_b64
        .as_ref()
        .ok_or_else(|| "media attachment requires base64 content".to_string())?;
    let object_bytes = base64::engine::general_purpose::STANDARD
        .decode(content_b64.trim())
        .map_err(|_| "attachment base64 content is invalid".to_string())?;

    if object_bytes.is_empty() {
        return Err("media attachment cannot be empty".to_string());
    }
    if object_bytes.len() as u64 > MAX_OBJECT_BYTES {
        return Err(format!(
            "media attachment exceeds maxObjectBytes ({MAX_OBJECT_BYTES} bytes)"
        ));
    }

    let object_sha256_hex = bytes_to_hex_lower(&Sha256::digest(&object_bytes));
    let transfer_id = uuid_v4_string();
    let now_ms = now_unix_ms();
    let limits = media_limits();
    let peer_cap = load_capabilities_cache()?
        .peers
        .get(to_wayfarer_id)
        .cloned()
        .ok_or_else(|| "peer capability cache missing for media send".to_string())?;
    if !peer_cap.media_v1 {
        return Err("peer capability cache disallows mediaV1".to_string());
    }

    let max_item_payload_b64_bytes = limits
        .max_item_payload_b64_bytes
        .min(peer_cap.max_item_payload_b64_bytes as usize);
    let ttl_expiry = now_ms.saturating_add(ttl_seconds_max.saturating_mul(1000));
    let expires_at_unix_ms = e2e_ttl_override_ms(ttl_expiry, now_ms);
    let chunk_payload_bytes = choose_chunk_payload_bytes(
        to_wayfarer_id,
        &transfer_id,
        &object_sha256_hex,
        expires_at_unix_ms,
        object_bytes.len() as u64,
        max_item_payload_b64_bytes,
        author_signing_seed,
    )?;
    let chunk_count = ceil_div_u64(object_bytes.len() as u64, chunk_payload_bytes as u64) as u32;

    let manifest = MediaManifestMessage {
        msg_type: MEDIA_MANIFEST_TYPE.to_string(),
        transfer_id: transfer_id.clone(),
        object_sha256_hex: object_sha256_hex.clone(),
        file_name: attachment.file_name.clone(),
        mime_type: attachment.mime_type.clone(),
        total_bytes: object_bytes.len() as u64,
        chunk_payload_bytes,
        chunk_count,
        expires_at_unix_ms,
        sent_at_unix_ms: now_ms,
        caption: (!caption.trim().is_empty()).then_some(caption.trim().to_string()),
    };
    validate_manifest(&manifest)?;

    persist_outbound_state(&manifest, to_wayfarer_id, &object_bytes, now_ms)?;

    let manifest_body = serde_json::to_string(&manifest)
        .map_err(|err| format!("failed serializing media manifest: {err}"))?;
    let manifest_payload =
        build_envelope_payload_b64_from_utf8(to_wayfarer_id, &manifest_body, author_signing_seed)?;
    if manifest_payload.len() > max_item_payload_b64_bytes {
        return Err(format!(
            "media manifest exceeds maxItemPayloadB64Bytes ({} > {})",
            manifest_payload.len(),
            max_item_payload_b64_bytes
        ));
    }
    rate_limit_outbound(manifest_payload.len(), now_ms, true, false)?;
    let manifest_expiry = apply_expiry_authority(expires_at_unix_ms, now_ms, ttl_seconds_max);
    let item_id = gossip_record_local_payload(&manifest_payload, manifest_expiry)?;

    let drop_chunk_index = e2e_drop_chunk_index();
    let initial_ceiling = INITIAL_OUTBOUND_CHUNK_CEILING.min(chunk_count as usize);
    let mut initial_chunks_queued = 0usize;
    let mut initial_chunks_dropped = 0usize;
    for chunk_index in 0..initial_ceiling {
        if Some(chunk_index as u32) == drop_chunk_index {
            initial_chunks_dropped = initial_chunks_dropped.saturating_add(1);
            log_verbose(&format!(
                "media_e2e_drop_chunk: transfer_id={} chunk_index={}",
                transfer_id, chunk_index
            ));
            continue;
        }
        let _ = record_chunk_by_index(
            &manifest,
            &object_bytes,
            chunk_index as u32,
            to_wayfarer_id,
            author_signing_seed,
            ttl_seconds_max,
            now_ms,
            max_item_payload_b64_bytes,
        )?;
        initial_chunks_queued = initial_chunks_queued.saturating_add(1);
    }
    log_verbose(&format!(
        "media_outbound_publish_summary: transfer_id={} recipient={} object_bytes={} chunk_count={} initial_ceiling={} initial_chunks_queued={} initial_chunks_dropped={} expiry_ms={}",
        transfer_id,
        to_wayfarer_id,
        object_bytes.len(),
        chunk_count,
        initial_ceiling,
        initial_chunks_queued,
        initial_chunks_dropped,
        expires_at_unix_ms
    ));

    Ok(OutboundMediaSendResult {
        item_id,
        transfer_id,
        object_sha256_hex,
        file_name: attachment.file_name.clone(),
        mime_type: attachment.mime_type.clone(),
        total_bytes: object_bytes.len() as u64,
        chunk_count,
        expires_at_unix_ms,
    })
}

#[allow(dead_code)] // Reserved for pending inbound media message dispatch wiring.
pub fn process_incoming_media_message(
    pulled: &EncounterMessagePreview,
    chat: &mut PersistedChatState,
    _contacts: &mut BTreeMap<String, String>,
    ttl_seconds_max: u64,
    local_wayfarer_id: &str,
    author_signing_seed: &[u8; 32],
) -> Result<MediaMessageProcess, String> {
    let media_text = String::from_utf8(pulled.body_bytes.clone())
        .map_err(|_| "invalid media message body encoding; expected utf8".to_string())?;
    let Some(parsed) = parse_media_message(&media_text)? else {
        return Ok(MediaMessageProcess::NotMedia);
    };

    match parsed {
        ParsedMediaMessage::Capabilities(msg) => {
            if let Some(sender) = pulled.author_wayfarer_id.as_deref() {
                if is_valid_wayfarer_id(sender) {
                    let mut cache = load_capabilities_cache()?;
                    cache.peers.insert(
                        sender.to_string(),
                        PeerCapability {
                            media_v1: msg.media_v1,
                            max_item_payload_b64_bytes: msg.max_item_payload_b64_bytes,
                            updated_at_unix_ms: msg.sent_at_unix_ms,
                        },
                    );
                    save_capabilities_cache(&cache)?;
                }
            }
            Ok(MediaMessageProcess::HandledSuppressed {
                chat_updated: false,
            })
        }
        ParsedMediaMessage::Manifest(msg) => {
            validate_manifest(&msg)?;
            let sender = pulled
                .author_wayfarer_id
                .clone()
                .filter(|value| is_valid_wayfarer_id(value))
                .unwrap_or_else(|| "unknown-peer".to_string());

            if let Some(mut existing) = load_incoming_state(Some(&sender), &msg.transfer_id)? {
                if existing.object_sha256_hex != msg.object_sha256_hex {
                    return Ok(MediaMessageProcess::HandledSuppressed {
                        chat_updated: false,
                    });
                }

                recover_transfer_progress_from_disk(&mut existing, true)?;
                let (status, error) = transfer_status_and_error(&existing);
                let mut chat_updated =
                    upsert_manifest_chat_message(chat, pulled, &msg, status.clone(), error.clone());
                chat_updated |= update_chat_media_progress(
                    chat,
                    &existing.sender_wayfarer_id,
                    &existing.transfer_id,
                    existing.received_chunks,
                    existing.received_bytes,
                    status.clone(),
                    error.clone(),
                );

                if transfer_is_terminal(&existing) {
                    return Ok(MediaMessageProcess::HandledManifest { chat_updated });
                }

                existing.file_name = msg.file_name.clone();
                existing.mime_type = msg.mime_type.clone();
                existing.total_bytes = msg.total_bytes;
                existing.chunk_payload_bytes = msg.chunk_payload_bytes;
                existing.chunk_count = msg.chunk_count;
                existing.expires_at_unix_ms = msg.expires_at_unix_ms;
                existing.sent_at_unix_ms = msg.sent_at_unix_ms;
                existing.caption = msg.caption.clone();

                persist_incoming_state(&existing)?;
                let _ = send_missing_request_if_due(
                    &existing,
                    local_wayfarer_id,
                    author_signing_seed,
                    ttl_seconds_max,
                );
                return Ok(MediaMessageProcess::HandledManifest { chat_updated });
            }

            if msg.expires_at_unix_ms <= now_unix_ms() {
                let chat_updated = upsert_manifest_chat_message(
                    chat,
                    pulled,
                    &msg,
                    MediaTransferStatus::Failed,
                    Some("expired_manifest".to_string()),
                );
                mark_transfer_failed(
                    &msg.transfer_id,
                    pulled.author_wayfarer_id.as_deref(),
                    "expired_manifest",
                )?;
                return Ok(MediaMessageProcess::HandledManifest { chat_updated });
            }

            let incoming = IncomingTransferState {
                transfer_id: msg.transfer_id.clone(),
                sender_wayfarer_id: sender.clone(),
                object_sha256_hex: msg.object_sha256_hex.clone(),
                file_name: msg.file_name.clone(),
                mime_type: msg.mime_type.clone(),
                total_bytes: msg.total_bytes,
                chunk_payload_bytes: msg.chunk_payload_bytes,
                chunk_count: msg.chunk_count,
                expires_at_unix_ms: msg.expires_at_unix_ms,
                sent_at_unix_ms: msg.sent_at_unix_ms,
                caption: msg.caption.clone(),
                received_chunks: 0,
                received_bytes: 0,
                strikes: BTreeMap::new(),
                last_missing_unix_ms: 0,
                failed_error: None,
                completed_unix_ms: None,
            };
            persist_incoming_state(&incoming)?;
            let chat_updated = upsert_manifest_chat_message(
                chat,
                pulled,
                &msg,
                MediaTransferStatus::Receiving,
                None,
            );
            log_verbose(&format!(
                "media_manifest_upserted: sender={} transfer_id={} object_sha={} thread_messages={}",
                sender,
                msg.transfer_id,
                msg.object_sha256_hex,
                chat.threads
                    .get(&sender)
                    .map(|thread| thread.len())
                    .unwrap_or(0)
            ));
            let _ = send_missing_request_if_due(
                &incoming,
                local_wayfarer_id,
                author_signing_seed,
                ttl_seconds_max,
            );
            Ok(MediaMessageProcess::HandledManifest { chat_updated })
        }
        ParsedMediaMessage::Chunk(msg) => {
            let mut chat_updated = false;
            let sender = pulled
                .author_wayfarer_id
                .clone()
                .filter(|value| is_valid_wayfarer_id(value))
                .unwrap_or_else(|| "unknown-peer".to_string());
            let max_item_payload_b64_bytes = negotiated_payload_b64_limit_for_sender(&sender);
            if msg.expires_at_unix_ms <= now_unix_ms() {
                mark_transfer_failed(&msg.transfer_id, Some(&sender), "chunk_expired")?;
                chat_updated |= update_chat_media_state(
                    chat,
                    &sender,
                    &msg.transfer_id,
                    MediaTransferStatus::Failed,
                    Some("chunk_expired".to_string()),
                );
                return Ok(MediaMessageProcess::HandledSuppressed { chat_updated });
            }

            let mut transfer = match load_incoming_state(Some(&sender), &msg.transfer_id)? {
                Some(state) => state,
                None => {
                    let _ = store_orphan_chunk(&sender, &msg);
                    return Ok(MediaMessageProcess::HandledSuppressed { chat_updated });
                }
            };

            recover_transfer_progress_from_disk(&mut transfer, true)?;
            if transfer_is_terminal(&transfer) {
                return Ok(MediaMessageProcess::HandledSuppressed { chat_updated });
            }

            if transfer.expires_at_unix_ms <= now_unix_ms() {
                transfer.failed_error = Some("expired_transfer".to_string());
                persist_incoming_state(&transfer)?;
                chat_updated |= update_chat_media_state(
                    chat,
                    &transfer.sender_wayfarer_id,
                    &transfer.transfer_id,
                    MediaTransferStatus::Failed,
                    Some("expired_transfer".to_string()),
                );
                return Ok(MediaMessageProcess::HandledSuppressed { chat_updated });
            }

            if msg.object_sha256_hex != transfer.object_sha256_hex {
                return Ok(MediaMessageProcess::HandledSuppressed { chat_updated });
            }
            if msg.chunk_index >= transfer.chunk_count {
                return Ok(MediaMessageProcess::HandledSuppressed { chat_updated });
            }
            if msg.payload_b64.contains('=') {
                strike_or_abort_corrupt_chunk(
                    &mut transfer,
                    msg.chunk_index,
                    "chunk_payload_not_base64url_nopad",
                    chat,
                )?;
                chat_updated = true;
                return Ok(MediaMessageProcess::HandledSuppressed { chat_updated });
            }

            let expected_len = expected_chunk_len(&transfer, msg.chunk_index)?;
            if let Err(reason) = validate_chunk_predecode_bounds(
                msg.payload_b64.len(),
                msg.chunk_bytes as usize,
                expected_len,
                transfer.chunk_payload_bytes as usize,
                max_item_payload_b64_bytes,
            ) {
                strike_or_abort_corrupt_chunk(&mut transfer, msg.chunk_index, reason, chat)?;
                chat_updated = true;
                return Ok(MediaMessageProcess::HandledSuppressed { chat_updated });
            }

            let bytes =
                match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&msg.payload_b64) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        strike_or_abort_corrupt_chunk(
                            &mut transfer,
                            msg.chunk_index,
                            "chunk_payload_decode_failed",
                            chat,
                        )?;
                        chat_updated = true;
                        return Ok(MediaMessageProcess::HandledSuppressed { chat_updated });
                    }
                };
            if bytes.len() != msg.chunk_bytes as usize {
                strike_or_abort_corrupt_chunk(
                    &mut transfer,
                    msg.chunk_index,
                    "chunk_length_mismatch",
                    chat,
                )?;
                chat_updated = true;
                return Ok(MediaMessageProcess::HandledSuppressed { chat_updated });
            }
            if bytes.len() != expected_len {
                strike_or_abort_corrupt_chunk(
                    &mut transfer,
                    msg.chunk_index,
                    "chunk_expected_length_mismatch",
                    chat,
                )?;
                chat_updated = true;
                return Ok(MediaMessageProcess::HandledSuppressed { chat_updated });
            }

            let actual_chunk_sha = bytes_to_hex_lower(&Sha256::digest(&bytes));
            if actual_chunk_sha != msg.chunk_sha256_hex {
                strike_or_abort_corrupt_chunk(
                    &mut transfer,
                    msg.chunk_index,
                    "chunk_hash_mismatch",
                    chat,
                )?;
                chat_updated = true;
                return Ok(MediaMessageProcess::HandledSuppressed { chat_updated });
            }

            ensure_spool_quota(&transfer.sender_wayfarer_id, bytes.len() as u64)?;
            let chunk_path = incoming_chunk_path(
                &transfer.sender_wayfarer_id,
                &transfer.transfer_id,
                msg.chunk_index,
            );
            if !chunk_path.exists() {
                write_chunk_file(&chunk_path, &bytes)?;
                apply_spool_usage_increase(&transfer.sender_wayfarer_id, bytes.len() as u64)?;
                transfer.received_chunks = transfer.received_chunks.saturating_add(1);
                transfer.received_bytes =
                    transfer.received_bytes.saturating_add(bytes.len() as u64);
                let now_ms = now_unix_ms();
                log_verbose(&format!(
                    "media_chunk_ingested: sender={} transfer_id={} chunk_index={} received_chunks={} chunk_count={}",
                    transfer.sender_wayfarer_id,
                    transfer.transfer_id,
                    msg.chunk_index,
                    transfer.received_chunks,
                    transfer.chunk_count
                ));
                if let Some((age_ms, requested_chunks, delivered_chunks, pending_chunks)) =
                    note_missing_request_chunk_delivered(
                        &transfer.sender_wayfarer_id,
                        &transfer.transfer_id,
                        msg.chunk_index,
                        now_ms,
                    )
                {
                    log_verbose(&format!(
                        "media_missing_yield_progress: sender={} transfer_id={} chunk_index={} age_ms={} requested_chunks={} delivered_chunks={} pending_chunks={}",
                        transfer.sender_wayfarer_id,
                        transfer.transfer_id,
                        msg.chunk_index,
                        age_ms,
                        requested_chunks,
                        delivered_chunks,
                        pending_chunks
                    ));
                }
            } else {
                log_verbose(&format!(
                    "media_chunk_duplicate: sender={} transfer_id={} chunk_index={}",
                    transfer.sender_wayfarer_id, transfer.transfer_id, msg.chunk_index
                ));
            }
            persist_incoming_state(&transfer)?;

            recover_transfer_progress_from_disk(&mut transfer, true)?;
            let (status, error) = transfer_status_and_error(&transfer);

            chat_updated |= update_chat_media_progress(
                chat,
                &transfer.sender_wayfarer_id,
                &transfer.transfer_id,
                transfer.received_chunks,
                transfer.received_bytes,
                status.clone(),
                error,
            );

            if status != MediaTransferStatus::Receiving {
                clear_missing_request_yield(&transfer.sender_wayfarer_id, &transfer.transfer_id);
            }

            if status == MediaTransferStatus::Receiving {
                let _ = send_missing_request_if_due(
                    &transfer,
                    local_wayfarer_id,
                    author_signing_seed,
                    ttl_seconds_max,
                );
            }

            Ok(MediaMessageProcess::HandledSuppressed { chat_updated })
        }
        ParsedMediaMessage::Missing(msg) => {
            let sender = pulled
                .author_wayfarer_id
                .clone()
                .filter(|value| is_valid_wayfarer_id(value))
                .unwrap_or_else(|| "unknown-peer".to_string());

            let requested_ranges = msg.ranges.len();
            log_verbose(&format!(
                "media_missing_received: requester={} transfer_id={} object_sha={} ranges={} sent_at_ms={}",
                sender, msg.transfer_id, msg.object_sha256_hex, requested_ranges, msg.sent_at_unix_ms
            ));

            if msg.ranges.len() > MISSING_RANGE_CAP {
                log_verbose(&format!(
                    "media_missing_ignored: requester={} transfer_id={} reason=range_cap_exceeded ranges={} cap={}",
                    sender,
                    msg.transfer_id,
                    msg.ranges.len(),
                    MISSING_RANGE_CAP
                ));
                return Ok(MediaMessageProcess::HandledSuppressed {
                    chat_updated: false,
                });
            }
            if !peer_supports_media_v1(&sender)? {
                log_verbose(&format!(
                    "media_missing_ignored: requester={} transfer_id={} reason=requester_capability_missing",
                    sender, msg.transfer_id
                ));
                return Ok(MediaMessageProcess::HandledSuppressed {
                    chat_updated: false,
                });
            }

            let arrival_now_ms = now_unix_ms();
            if !missing_request_interval_ok(&sender, &msg.transfer_id, arrival_now_ms) {
                log_verbose(&format!(
                    "media_missing_ignored: requester={} transfer_id={} reason=interval_gate sent_at_ms={} arrival_ms={}",
                    sender, msg.transfer_id, msg.sent_at_unix_ms, arrival_now_ms
                ));
                return Ok(MediaMessageProcess::HandledSuppressed {
                    chat_updated: false,
                });
            }

            if let Some(outbound) = load_outbound_state(&msg.transfer_id)? {
                if outbound.object_sha256_hex != msg.object_sha256_hex {
                    log_verbose(&format!(
                        "media_missing_ignored: requester={} transfer_id={} reason=object_sha_mismatch outbound_sha={} requested_sha={}",
                        sender,
                        msg.transfer_id,
                        outbound.object_sha256_hex,
                        msg.object_sha256_hex
                    ));
                    return Ok(MediaMessageProcess::HandledSuppressed {
                        chat_updated: false,
                    });
                }
                if outbound.expires_at_unix_ms <= now_unix_ms() {
                    log_verbose(&format!(
                        "media_missing_ignored: requester={} transfer_id={} reason=outbound_expired expires_at_ms={}",
                        sender, msg.transfer_id, outbound.expires_at_unix_ms
                    ));
                    return Ok(MediaMessageProcess::HandledSuppressed {
                        chat_updated: false,
                    });
                }

                let source_path = outbound_source_path(&outbound.transfer_id);
                let mut source_file = fs::File::open(&source_path).map_err(|err| {
                    format!(
                        "failed opening outbound source for transfer {} at {}: {err}",
                        outbound.transfer_id,
                        source_path.display()
                    )
                })?;
                let max_payload = media_limits().max_item_payload_b64_bytes;
                let manifest = MediaManifestMessage {
                    msg_type: MEDIA_MANIFEST_TYPE.to_string(),
                    transfer_id: outbound.transfer_id.clone(),
                    object_sha256_hex: outbound.object_sha256_hex.clone(),
                    file_name: outbound.file_name.clone(),
                    mime_type: outbound.mime_type.clone(),
                    total_bytes: outbound.total_bytes,
                    chunk_payload_bytes: outbound.chunk_payload_bytes,
                    chunk_count: outbound.chunk_count,
                    expires_at_unix_ms: outbound.expires_at_unix_ms,
                    sent_at_unix_ms: outbound.sent_at_unix_ms,
                    caption: outbound.caption.clone(),
                };
                let mut sent_chunks = 0usize;
                let chunk_ceiling = missing_response_chunk_ceiling();
                let dedup_key = format!("{}:{}", sender, msg.transfer_id);
                let mut invalid_range_skips = 0usize;
                let mut dedup_skips = 0usize;
                let mut out_of_bounds_skips = 0usize;
                let mut e2e_drop_skips = 0usize;
                let mut first_queue_error = None::<String>;
                for range in msg.ranges {
                    if range.start > range.end {
                        invalid_range_skips = invalid_range_skips.saturating_add(1);
                        log_verbose(&format!(
                            "media_missing_range_ignored: requester={} transfer_id={} reason=invalid_range start={} end={}",
                            sender, msg.transfer_id, range.start, range.end
                        ));
                        continue;
                    }
                    let mut index = range.start;
                    while index <= range.end {
                        if sent_chunks >= chunk_ceiling {
                            break;
                        }
                        if index >= outbound.chunk_count {
                            out_of_bounds_skips = out_of_bounds_skips.saturating_add(1);
                            break;
                        }
                        if Some(index) == e2e_drop_chunk_index() {
                            e2e_drop_skips = e2e_drop_skips.saturating_add(1);
                            index = index.saturating_add(1);
                            continue;
                        }
                        let send_now_ms = now_unix_ms();
                        if missing_response_chunk_recently_sent(&dedup_key, index, send_now_ms) {
                            dedup_skips = dedup_skips.saturating_add(1);
                            index = index.saturating_add(1);
                            continue;
                        }

                        let chunk_bytes =
                            read_chunk_bytes_from_file(&mut source_file, &manifest, index)?;
                        if let Err(err) = queue_chunk_message_fastlane(
                            &manifest,
                            index,
                            &chunk_bytes,
                            &sender,
                            author_signing_seed,
                            ttl_seconds_max,
                            send_now_ms,
                            max_payload,
                        ) {
                            if first_queue_error.is_none() {
                                first_queue_error = Some(err.clone());
                            }
                            log_verbose(&format!(
                                "media_missing_chunk_queue_failed: requester={} transfer_id={} chunk_index={} error={}",
                                sender, msg.transfer_id, index, err
                            ));
                            break;
                        }
                        note_missing_response_chunk_sent(&dedup_key, index, send_now_ms);
                        sent_chunks = sent_chunks.saturating_add(1);
                        index = index.saturating_add(1);
                    }
                }
                log_verbose(&format!(
                    "media_missing_served: requester={} transfer_id={} ranges={} sent_chunks={} chunk_ceiling={} invalid_range_skips={} dedup_skips={} out_of_bounds_skips={} e2e_drop_skips={} queue_error={}",
                    sender,
                    msg.transfer_id,
                    requested_ranges,
                    sent_chunks,
                    chunk_ceiling,
                    invalid_range_skips,
                    dedup_skips,
                    out_of_bounds_skips,
                    e2e_drop_skips,
                    first_queue_error.unwrap_or_else(|| "none".to_string())
                ));
            } else {
                log_verbose(&format!(
                    "media_missing_ignored: requester={} transfer_id={} reason=outbound_not_found",
                    sender, msg.transfer_id
                ));
            }

            Ok(MediaMessageProcess::HandledSuppressed {
                chat_updated: false,
            })
        }
    }
}

pub fn load_completed_media(object_sha256_hex: &str) -> Result<CompletedMediaPayload, String> {
    if !is_valid_sha256_hex(object_sha256_hex) {
        return Err("invalid objectSha256Hex".to_string());
    }
    sweep_complete_store()?;
    let meta_path = complete_meta_path(object_sha256_hex);
    let bin_path = complete_bin_path(object_sha256_hex);
    let mut meta = read_json_file::<CompletedTransferMeta>(&meta_path)?;

    if meta.size_bytes > COMPLETE_INLINE_MAX_BYTES {
        return Err(format!(
            "completed media exceeds inline limit ({COMPLETE_INLINE_MAX_BYTES} bytes)"
        ));
    }

    let bytes = fs::read(&bin_path).map_err(|err| {
        format!(
            "failed reading completed media object {} at {}: {err}",
            object_sha256_hex,
            bin_path.display()
        )
    })?;
    let actual = bytes_to_hex_lower(&Sha256::digest(&bytes));
    if actual != object_sha256_hex {
        return Err("completed media hash mismatch".to_string());
    }
    meta.last_access_unix_ms = now_unix_ms();
    write_json_file_atomic(&meta_path, &meta)?;
    Ok(CompletedMediaPayload {
        object_sha256_hex: object_sha256_hex.to_string(),
        file_name: meta.file_name,
        mime_type: meta.mime_type,
        size_bytes: bytes.len() as u64,
        content_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

pub fn get_completed_media_path(
    object_sha256_hex: &str,
) -> Result<CompletedMediaPathPayload, String> {
    if !is_valid_sha256_hex(object_sha256_hex) {
        return Err("invalid objectSha256Hex".to_string());
    }
    sweep_complete_store()?;
    let meta_path = complete_meta_path(object_sha256_hex);
    let bin_path = complete_bin_path(object_sha256_hex);
    let mut meta = read_json_file::<CompletedTransferMeta>(&meta_path)?;

    if !bin_path.exists() {
        return Err("completed media object not found".to_string());
    }
    let canonical_complete_dir = canonicalize_existing_dir(&media_complete_dir())?;
    let canonical_bin_path = fs::canonicalize(&bin_path).map_err(|err| {
        format!(
            "failed resolving completed media path {}: {err}",
            bin_path.display()
        )
    })?;
    if !canonical_bin_path.starts_with(&canonical_complete_dir) {
        return Err("completed media path escaped managed directory".to_string());
    }

    let size_bytes = fs::metadata(&canonical_bin_path)
        .map_err(|err| {
            format!(
                "failed stat completed media path {}: {err}",
                canonical_bin_path.display()
            )
        })?
        .len();
    if size_bytes != meta.size_bytes {
        return Err("completed media size mismatch".to_string());
    }

    meta.last_access_unix_ms = now_unix_ms();
    write_json_file_atomic(&meta_path, &meta)?;

    Ok(CompletedMediaPathPayload {
        path: canonical_bin_path.display().to_string(),
        mime: meta.mime_type,
        size_bytes,
    })
}

pub fn run_housekeeping_tick(ttl_seconds_max: u64) -> Result<bool, String> {
    sweep_orphans()?;
    let _ = reconcile_spool_usage_cache()?;
    sweep_complete_store()?;
    sweep_expired_incoming_transfers()?;
    expire_chat_media_messages(ttl_seconds_max)
}

pub fn pulse_missing_requests_for_receiving_transfers(
    local_wayfarer_id: &str,
    author_signing_seed: &[u8; 32],
    ttl_seconds_max: u64,
) -> Result<usize, String> {
    if !is_valid_wayfarer_id(local_wayfarer_id) {
        return Ok(0);
    }
    let spool = media_spool_dir();
    if !spool.exists() {
        return Ok(0);
    }

    let now = now_unix_ms();
    let mut sent = 0usize;
    for peer in fs::read_dir(&spool)
        .map_err(|err| format!("failed reading spool root {}: {err}", spool.display()))?
    {
        let peer = peer.map_err(|err| format!("failed reading spool peer entry: {err}"))?;
        if !peer.path().is_dir() {
            continue;
        }
        for transfer in fs::read_dir(peer.path()).map_err(|err| {
            format!(
                "failed reading transfer dir {}: {err}",
                peer.path().display()
            )
        })? {
            let transfer =
                transfer.map_err(|err| format!("failed reading transfer entry: {err}"))?;
            let dir = transfer.path();
            if !dir.is_dir() || dir.join(".active.lock").exists() {
                continue;
            }

            let state_path = dir.join("state.json");
            if !state_path.exists() {
                continue;
            }

            let mut state = read_json_file::<IncomingTransferState>(&state_path)?;
            if transfer_is_terminal(&state) || state.expires_at_unix_ms <= now {
                continue;
            }
            recover_transfer_progress_from_disk(&mut state, true)?;
            if transfer_is_terminal(&state) {
                continue;
            }
            if send_missing_request_if_due(
                &state,
                local_wayfarer_id,
                author_signing_seed,
                ttl_seconds_max,
            )? {
                sent = sent.saturating_add(1);
            }
        }
    }
    Ok(sent)
}

#[allow(dead_code)] // Reserved for pending inbound media wire message dispatch wiring.
fn parse_media_message(input: &str) -> Result<Option<ParsedMediaMessage>, String> {
    let value = match serde_json::from_str::<serde_json::Value>(input) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(msg_type) = value
        .get("type")
        .and_then(|item| item.as_str())
        .map(|item| item.trim())
    else {
        return Ok(None);
    };
    let parsed = match msg_type {
        MEDIA_CAPABILITIES_TYPE => ParsedMediaMessage::Capabilities(
            serde_json::from_value(value)
                .map_err(|err| format!("invalid media capabilities message: {err}"))?,
        ),
        MEDIA_MANIFEST_TYPE => ParsedMediaMessage::Manifest(
            serde_json::from_value(value)
                .map_err(|err| format!("invalid media manifest message: {err}"))?,
        ),
        MEDIA_CHUNK_TYPE => ParsedMediaMessage::Chunk(
            serde_json::from_value(value)
                .map_err(|err| format!("invalid media chunk message: {err}"))?,
        ),
        MEDIA_MISSING_TYPE => ParsedMediaMessage::Missing(
            serde_json::from_value(value)
                .map_err(|err| format!("invalid media missing message: {err}"))?,
        ),
        _ => return Ok(None),
    };
    Ok(Some(parsed))
}

#[allow(dead_code)] // Reserved for pending media manifest ingest/send validation wiring.
fn validate_manifest(manifest: &MediaManifestMessage) -> Result<(), String> {
    if !is_valid_sha256_hex(&manifest.object_sha256_hex) {
        return Err("manifest objectSha256Hex must be lowercase hex64".to_string());
    }
    if !is_uuid_v4(&manifest.transfer_id) {
        return Err("manifest transferId must be UUIDv4".to_string());
    }
    if manifest.total_bytes == 0 || manifest.total_bytes > MAX_OBJECT_BYTES {
        return Err("manifest totalBytes out of range".to_string());
    }
    if manifest.chunk_payload_bytes == 0 {
        return Err("manifest chunkPayloadBytes must be > 0".to_string());
    }
    let expected_chunk_count =
        ceil_div_u64(manifest.total_bytes, manifest.chunk_payload_bytes as u64) as u32;
    if expected_chunk_count != manifest.chunk_count {
        return Err("manifest chunkCount invariant failed".to_string());
    }
    Ok(())
}

fn choose_chunk_payload_bytes(
    to_wayfarer_id: &str,
    transfer_id: &str,
    object_sha256_hex: &str,
    expires_at_unix_ms: u64,
    total_bytes: u64,
    max_item_payload_b64_bytes: usize,
    author_signing_seed: &[u8; 32],
) -> Result<u32, String> {
    let mut candidate = 64 * 1024usize;
    if total_bytes < candidate as u64 {
        candidate = total_bytes.max(1) as usize;
    }

    while candidate >= 1024 {
        let sample_bytes = vec![0u8; candidate.min(total_bytes as usize)];
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&sample_bytes);
        let chunk_sha = bytes_to_hex_lower(&Sha256::digest(&sample_bytes));
        let body = serde_json::to_string(&MediaChunkMessage {
            msg_type: MEDIA_CHUNK_TYPE.to_string(),
            transfer_id: transfer_id.to_string(),
            object_sha256_hex: object_sha256_hex.to_string(),
            chunk_index: 0,
            chunk_sha256_hex: chunk_sha,
            chunk_bytes: sample_bytes.len() as u32,
            payload_b64,
            expires_at_unix_ms,
        })
        .map_err(|err| format!("failed serializing sample media chunk: {err}"))?;
        let envelope_payload =
            build_envelope_payload_b64_from_utf8(to_wayfarer_id, &body, author_signing_seed)?;
        if envelope_payload.len() <= max_item_payload_b64_bytes {
            return Ok(candidate as u32);
        }
        candidate = candidate.saturating_sub(1024);
    }

    Err("unable to fit media chunk within maxItemPayloadB64Bytes".to_string())
}

#[allow(dead_code)] // Reserved for pending initial outbound chunk publish wiring.
#[allow(clippy::too_many_arguments)]
fn record_chunk_by_index(
    manifest: &MediaManifestMessage,
    object_bytes: &[u8],
    chunk_index: u32,
    to_wayfarer_id: &str,
    author_signing_seed: &[u8; 32],
    ttl_seconds_max: u64,
    now_ms: u64,
    max_item_payload_b64_bytes: usize,
) -> Result<String, String> {
    let (start, end) = chunk_bounds(
        manifest.total_bytes,
        manifest.chunk_payload_bytes,
        chunk_index,
    )?;
    if end > object_bytes.len() {
        return Err("chunk bounds exceed object bytes".to_string());
    }
    let chunk = &object_bytes[start..end];
    record_chunk_message(
        manifest,
        chunk_index,
        chunk,
        to_wayfarer_id,
        author_signing_seed,
        ttl_seconds_max,
        now_ms,
        max_item_payload_b64_bytes,
    )
}

#[allow(dead_code)] // Reserved for pending initial outbound chunk publish wiring.
#[allow(clippy::too_many_arguments)]
fn record_chunk_message(
    manifest: &MediaManifestMessage,
    chunk_index: u32,
    chunk: &[u8],
    to_wayfarer_id: &str,
    author_signing_seed: &[u8; 32],
    ttl_seconds_max: u64,
    now_ms: u64,
    max_item_payload_b64_bytes: usize,
) -> Result<String, String> {
    let payload = build_chunk_envelope_payload(
        manifest,
        chunk_index,
        chunk,
        to_wayfarer_id,
        author_signing_seed,
        max_item_payload_b64_bytes,
    )?;
    rate_limit_outbound(payload.len(), now_ms, false, true)?;
    let expiry = apply_expiry_authority(manifest.expires_at_unix_ms, now_ms, ttl_seconds_max);
    let item_id = gossip_record_local_payload(&payload, expiry)?;
    log_verbose(&format!(
        "media_chunk_published: stage=initial transfer_id={} recipient={} chunk_index={} item_id={}",
        manifest.transfer_id, to_wayfarer_id, chunk_index, item_id
    ));
    Ok(item_id)
}

#[allow(dead_code)] // Reserved for pending missing-response fastlane wiring.
#[allow(clippy::too_many_arguments)]
fn queue_chunk_message_fastlane(
    manifest: &MediaManifestMessage,
    chunk_index: u32,
    chunk: &[u8],
    to_wayfarer_id: &str,
    author_signing_seed: &[u8; 32],
    ttl_seconds_max: u64,
    now_ms: u64,
    max_item_payload_b64_bytes: usize,
) -> Result<String, String> {
    let payload = build_chunk_envelope_payload(
        manifest,
        chunk_index,
        chunk,
        to_wayfarer_id,
        author_signing_seed,
        max_item_payload_b64_bytes,
    )?;
    let expiry = apply_expiry_authority(manifest.expires_at_unix_ms, now_ms, ttl_seconds_max);
    let item_id = gossip_record_local_payload(&payload, expiry)?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&payload)
        .map_err(|err| format!("failed decoding chunk envelope payload for fastlane: {err}"))?;
    let derived_item_id = bytes_to_hex_lower(&Sha256::digest(&raw));
    if derived_item_id != item_id {
        return Err("fastlane chunk item_id mismatch with stored payload".to_string());
    }
    queue_pending_media_control_unicast(PendingMediaControlUnicast {
        peer_wayfarer_id: to_wayfarer_id.to_string(),
        item_id: item_id.clone(),
        payload_b64: payload,
        expiry_unix_ms: expiry,
        kind: PendingMediaControlKind::ChunkResponse,
        transfer_id: Some(manifest.transfer_id.clone()),
        tcp_mirror: false,
    });
    Ok(item_id)
}

#[allow(dead_code)] // Reserved for pending outbound chunk envelope publish wiring.
fn build_chunk_envelope_payload(
    manifest: &MediaManifestMessage,
    chunk_index: u32,
    chunk: &[u8],
    to_wayfarer_id: &str,
    author_signing_seed: &[u8; 32],
    max_item_payload_b64_bytes: usize,
) -> Result<String, String> {
    if chunk_index >= manifest.chunk_count {
        return Err("chunk index out of range".to_string());
    }
    let expected_len = chunk_bounds(
        manifest.total_bytes,
        manifest.chunk_payload_bytes,
        chunk_index,
    )?;
    let expected_size = expected_len.1.saturating_sub(expected_len.0);
    if chunk.len() != expected_size {
        return Err("chunk bytes length mismatch".to_string());
    }
    let chunk_sha256_hex = bytes_to_hex_lower(&Sha256::digest(chunk));
    let body = serde_json::to_string(&MediaChunkMessage {
        msg_type: MEDIA_CHUNK_TYPE.to_string(),
        transfer_id: manifest.transfer_id.clone(),
        object_sha256_hex: manifest.object_sha256_hex.clone(),
        chunk_index,
        chunk_sha256_hex,
        chunk_bytes: chunk.len() as u32,
        payload_b64: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(chunk),
        expires_at_unix_ms: manifest.expires_at_unix_ms,
    })
    .map_err(|err| format!("failed serializing media chunk: {err}"))?;

    let payload = build_envelope_payload_b64_from_utf8(to_wayfarer_id, &body, author_signing_seed)?;
    if payload.len() > max_item_payload_b64_bytes {
        return Err(format!(
            "chunk envelope exceeds maxItemPayloadB64Bytes ({} > {})",
            payload.len(),
            max_item_payload_b64_bytes
        ));
    }
    Ok(payload)
}

#[allow(dead_code)] // Reserved for pending missing-response chunk replay wiring.
fn read_chunk_bytes_from_file(
    file: &mut fs::File,
    manifest: &MediaManifestMessage,
    chunk_index: u32,
) -> Result<Vec<u8>, String> {
    let (start, end) = chunk_bounds(
        manifest.total_bytes,
        manifest.chunk_payload_bytes,
        chunk_index,
    )?;
    let len = end.saturating_sub(start);
    file.seek(SeekFrom::Start(start as u64)).map_err(|err| {
        format!(
            "failed seeking outbound source to chunk {} offset {}: {err}",
            chunk_index, start
        )
    })?;
    let mut chunk = vec![0u8; len];
    file.read_exact(&mut chunk).map_err(|err| {
        format!(
            "failed reading outbound source bytes for chunk {} ({} bytes): {err}",
            chunk_index, len
        )
    })?;
    Ok(chunk)
}

#[allow(dead_code)] // Reserved for pending outbound transfer persistence wiring.
fn persist_outbound_state(
    manifest: &MediaManifestMessage,
    recipient_wayfarer_id: &str,
    source_bytes: &[u8],
    now_ms: u64,
) -> Result<(), String> {
    let dir = outbound_transfer_dir(&manifest.transfer_id);
    fs::create_dir_all(&dir).map_err(|err| {
        format!(
            "failed creating outbound transfer dir {}: {err}",
            dir.display()
        )
    })?;
    let state = OutboundTransferState {
        transfer_id: manifest.transfer_id.clone(),
        recipient_wayfarer_id: recipient_wayfarer_id.to_string(),
        object_sha256_hex: manifest.object_sha256_hex.clone(),
        file_name: manifest.file_name.clone(),
        mime_type: manifest.mime_type.clone(),
        total_bytes: manifest.total_bytes,
        chunk_payload_bytes: manifest.chunk_payload_bytes,
        chunk_count: manifest.chunk_count,
        expires_at_unix_ms: manifest.expires_at_unix_ms,
        sent_at_unix_ms: now_ms,
        caption: manifest.caption.clone(),
    };
    write_json_file_atomic(&outbound_state_path(&manifest.transfer_id), &state)?;
    fs::write(outbound_source_path(&manifest.transfer_id), source_bytes).map_err(|err| {
        format!(
            "failed writing outbound source bytes for transfer {}: {err}",
            manifest.transfer_id
        )
    })
}

fn persist_incoming_state(state: &IncomingTransferState) -> Result<(), String> {
    let dir = incoming_transfer_dir(&state.sender_wayfarer_id, &state.transfer_id);
    fs::create_dir_all(dir.join("chunks")).map_err(|err| {
        format!(
            "failed creating incoming transfer dir {}: {err}",
            dir.display()
        )
    })?;
    write_json_file_atomic(
        &incoming_state_path(&state.sender_wayfarer_id, &state.transfer_id),
        state,
    )
}

fn load_incoming_state(
    sender_wayfarer_id: Option<&str>,
    transfer_id: &str,
) -> Result<Option<IncomingTransferState>, String> {
    if let Some(sender) = sender_wayfarer_id {
        let path = incoming_state_path(sender, transfer_id);
        if !path.exists() {
            return Ok(None);
        }
        return read_json_file(&path).map(Some);
    }

    let root = media_spool_dir();
    if !root.exists() {
        return Ok(None);
    }
    for entry in fs::read_dir(&root)
        .map_err(|err| format!("failed reading media spool dir {}: {err}", root.display()))?
    {
        let entry = entry.map_err(|err| format!("failed reading media spool entry: {err}"))?;
        let candidate = entry.path().join(transfer_id).join("state.json");
        if candidate.exists() {
            return read_json_file(&candidate).map(Some);
        }
    }
    Ok(None)
}

#[allow(dead_code)] // Reserved for pending outbound transfer replay wiring.
fn load_outbound_state(transfer_id: &str) -> Result<Option<OutboundTransferState>, String> {
    let path = outbound_state_path(transfer_id);
    if !path.exists() {
        return Ok(None);
    }
    read_json_file(&path).map(Some)
}

fn recover_transfer_progress_from_disk(
    transfer: &mut IncomingTransferState,
    allow_finalize: bool,
) -> Result<(), String> {
    if transfer.completed_unix_ms.is_none() {
        if let Some(completed_at) = completed_object_completion_time(transfer)? {
            transfer.completed_unix_ms = Some(completed_at);
            transfer.received_chunks = transfer.chunk_count;
            transfer.received_bytes = transfer.total_bytes;
            persist_incoming_state(transfer)?;
            return Ok(());
        }
    }

    if transfer.completed_unix_ms.is_some() {
        let mut changed = false;
        if transfer.received_chunks != transfer.chunk_count {
            transfer.received_chunks = transfer.chunk_count;
            changed = true;
        }
        if transfer.received_bytes != transfer.total_bytes {
            transfer.received_bytes = transfer.total_bytes;
            changed = true;
        }
        if changed {
            persist_incoming_state(transfer)?;
        }
        return Ok(());
    }

    let mut observed_chunks = 0u32;
    let mut observed_bytes = 0u64;
    for chunk_index in 0..transfer.chunk_count {
        let path = incoming_chunk_path(
            &transfer.sender_wayfarer_id,
            &transfer.transfer_id,
            chunk_index,
        );
        if !path.exists() {
            continue;
        }
        let expected = expected_chunk_len(transfer, chunk_index)?;
        let actual = fs::metadata(&path)
            .map_err(|err| {
                format!(
                    "failed stat chunk {} during recovery: {err}",
                    path.display()
                )
            })?
            .len() as usize;
        if actual != expected {
            remove_transfer_chunks_and_adjust_usage(transfer)?;
            transfer.received_chunks = 0;
            transfer.received_bytes = 0;
            transfer.failed_error = Some("chunk_expected_length_mismatch".to_string());
            persist_incoming_state(transfer)?;
            return Ok(());
        }
        observed_chunks = observed_chunks.saturating_add(1);
        observed_bytes = observed_bytes.saturating_add(actual as u64);
    }

    let mut should_persist = false;
    if transfer.received_chunks != observed_chunks {
        transfer.received_chunks = observed_chunks;
        should_persist = true;
    }
    if transfer.received_bytes != observed_bytes {
        transfer.received_bytes = observed_bytes;
        should_persist = true;
    }

    if allow_finalize
        && transfer.failed_error.is_none()
        && transfer.completed_unix_ms.is_none()
        && observed_chunks == transfer.chunk_count
    {
        match finalize_completed_transfer(transfer) {
            Ok(()) => return Ok(()),
            Err(err) => {
                remove_transfer_chunks_and_adjust_usage(transfer)?;
                transfer.received_chunks = 0;
                transfer.received_bytes = 0;
                transfer.failed_error = Some(err);
                should_persist = true;
            }
        }
    }

    if should_persist {
        persist_incoming_state(transfer)?;
    }

    Ok(())
}

fn completed_object_completion_time(
    transfer: &IncomingTransferState,
) -> Result<Option<u64>, String> {
    let bin_path = complete_bin_path(&transfer.object_sha256_hex);
    if !bin_path.exists() {
        return Ok(None);
    }

    let meta_path = complete_meta_path(&transfer.object_sha256_hex);
    if meta_path.exists() {
        let meta = read_json_file::<CompletedTransferMeta>(&meta_path)?;
        if meta.size_bytes != transfer.total_bytes {
            return Ok(None);
        }
        return Ok(Some(meta.completed_at_unix_ms));
    }

    let size = fs::metadata(&bin_path)
        .map_err(|err| {
            format!(
                "failed stat completed media object {}: {err}",
                bin_path.display()
            )
        })?
        .len();
    if size != transfer.total_bytes {
        return Ok(None);
    }
    let bytes = fs::read(&bin_path).map_err(|err| {
        format!(
            "failed reading completed media object {}: {err}",
            bin_path.display()
        )
    })?;
    let hash = bytes_to_hex_lower(&Sha256::digest(&bytes));
    if hash != transfer.object_sha256_hex {
        return Ok(None);
    }
    Ok(Some(now_unix_ms()))
}

fn remove_transfer_chunks_and_adjust_usage(transfer: &IncomingTransferState) -> Result<(), String> {
    let chunks_dir =
        incoming_transfer_dir(&transfer.sender_wayfarer_id, &transfer.transfer_id).join("chunks");
    let existing_bytes = dir_size_bytes(&chunks_dir)?;
    if existing_bytes == 0 {
        return Ok(());
    }
    if chunks_dir.exists() {
        fs::remove_dir_all(&chunks_dir).map_err(|err| {
            format!(
                "failed removing incoming chunk dir {}: {err}",
                chunks_dir.display()
            )
        })?;
    }
    apply_spool_usage_decrease(&transfer.sender_wayfarer_id, existing_bytes)
}

fn load_or_reconcile_spool_usage_cache() -> Result<SpoolUsageCache, String> {
    let path = spool_usage_cache_path();
    if path.exists() {
        return read_json_file(&path);
    }
    reconcile_spool_usage_cache()
}

fn orphan_usage_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn orphan_last_sweep_ms() -> &'static Mutex<u64> {
    static LAST: OnceLock<Mutex<u64>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(0))
}

fn maybe_sweep_orphans() -> Result<(), String> {
    let now = now_unix_ms();
    let mut gate = orphan_last_sweep_ms()
        .lock()
        .map_err(|_| "orphan sweep gate lock poisoned".to_string())?;
    if now.saturating_sub(*gate) < ORPHAN_SWEEP_MIN_INTERVAL_MS {
        return Ok(());
    }
    *gate = now;
    drop(gate);
    sweep_orphans()
}

fn load_or_reconcile_orphan_usage_cache() -> Result<OrphanUsageCache, String> {
    let path = orphan_usage_cache_path();
    if path.exists() {
        match read_json_file(&path) {
            Ok(cache) => return Ok(cache),
            Err(_) => {
                // fall through to reconcile
            }
        }
    }
    reconcile_orphan_usage_cache()
}

fn save_orphan_usage_cache(cache: &OrphanUsageCache) -> Result<(), String> {
    write_json_file_atomic(&orphan_usage_cache_path(), cache)
}

fn reconcile_orphan_usage_cache() -> Result<OrphanUsageCache, String> {
    let root = media_orphans_dir();
    if !root.exists() {
        let cache = OrphanUsageCache::default();
        save_orphan_usage_cache(&cache)?;
        return Ok(cache);
    }

    let mut usage = OrphanUsageCache::default();
    for peer in fs::read_dir(&root)
        .map_err(|err| format!("failed reading orphan root {}: {err}", root.display()))?
    {
        let peer = peer.map_err(|err| format!("failed reading orphan peer entry: {err}"))?;
        if !peer.path().is_dir() {
            continue;
        }
        let peer_name = peer.file_name().to_string_lossy().to_string();
        if !is_valid_wayfarer_id(&peer_name) {
            continue;
        }

        let mut peer_usage = OrphanPeerUsage::default();
        for file in fs::read_dir(peer.path()).map_err(|err| {
            format!(
                "failed reading orphan peer dir {}: {err}",
                peer.path().display()
            )
        })? {
            let file = file.map_err(|err| format!("failed reading orphan file entry: {err}"))?;
            let path = file.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(meta) = read_json_file::<OrphanChunkMeta>(&path) else {
                continue;
            };
            if !is_valid_wayfarer_id(&meta.sender_wayfarer_id)
                || !is_uuid_v4(&meta.transfer_id)
                || !is_valid_sha256_hex(&meta.object_sha256_hex)
            {
                continue;
            }
            peer_usage.declared_bytes = peer_usage
                .declared_bytes
                .saturating_add(meta.chunk_bytes as u64);
            peer_usage.meta_files = peer_usage.meta_files.saturating_add(1);
        }
        if peer_usage.meta_files == 0 {
            continue;
        }
        usage.global_declared_bytes = usage
            .global_declared_bytes
            .saturating_add(peer_usage.declared_bytes);
        usage.global_meta_files = usage
            .global_meta_files
            .saturating_add(peer_usage.meta_files);
        usage.per_peer.insert(peer_name, peer_usage);
    }

    usage.updated_at_unix_ms = now_unix_ms();
    save_orphan_usage_cache(&usage)?;
    Ok(usage)
}

fn save_spool_usage_cache(cache: &SpoolUsageCache) -> Result<(), String> {
    write_json_file_atomic(&spool_usage_cache_path(), cache)
}

#[allow(dead_code)] // Reserved for pending inbound spool accounting on chunk ingest.
fn apply_spool_usage_increase(sender_wayfarer_id: &str, delta_bytes: u64) -> Result<(), String> {
    if delta_bytes == 0 {
        return Ok(());
    }
    let mut cache = load_or_reconcile_spool_usage_cache()?;
    cache.global_partial_bytes = cache.global_partial_bytes.saturating_add(delta_bytes);
    let peer = cache
        .per_peer_partial_bytes
        .entry(sender_wayfarer_id.to_string())
        .or_insert(0);
    *peer = peer.saturating_add(delta_bytes);
    cache.updated_at_unix_ms = now_unix_ms();
    save_spool_usage_cache(&cache)
}

fn apply_spool_usage_decrease(sender_wayfarer_id: &str, delta_bytes: u64) -> Result<(), String> {
    if delta_bytes == 0 {
        return Ok(());
    }
    let mut cache = load_or_reconcile_spool_usage_cache()?;
    cache.global_partial_bytes = cache.global_partial_bytes.saturating_sub(delta_bytes);
    let next_peer = cache
        .per_peer_partial_bytes
        .get(sender_wayfarer_id)
        .copied()
        .unwrap_or(0)
        .saturating_sub(delta_bytes);
    if next_peer == 0 {
        cache.per_peer_partial_bytes.remove(sender_wayfarer_id);
    } else {
        cache
            .per_peer_partial_bytes
            .insert(sender_wayfarer_id.to_string(), next_peer);
    }
    cache.updated_at_unix_ms = now_unix_ms();
    save_spool_usage_cache(&cache)
}

fn reconcile_spool_usage_cache() -> Result<SpoolUsageCache, String> {
    let spool = media_spool_dir();
    let mut cache = SpoolUsageCache::default();
    if !spool.exists() {
        save_spool_usage_cache(&cache)?;
        return Ok(cache);
    }

    for peer in fs::read_dir(&spool)
        .map_err(|err| format!("failed reading spool root {}: {err}", spool.display()))?
    {
        let peer = peer.map_err(|err| format!("failed reading spool peer entry: {err}"))?;
        if !peer.path().is_dir() {
            continue;
        }
        let peer_name = peer.file_name().to_string_lossy().to_string();
        if peer_name == "orphans" {
            continue;
        }
        let mut peer_partial = 0u64;
        for transfer in fs::read_dir(peer.path())
            .map_err(|err| format!("failed reading spool peer {}: {err}", peer.path().display()))?
        {
            let transfer =
                transfer.map_err(|err| format!("failed reading transfer entry: {err}"))?;
            if !transfer.path().is_dir() {
                continue;
            }
            let chunks = transfer.path().join("chunks");
            if !chunks.exists() {
                continue;
            }

            let should_count = match read_json_file::<IncomingTransferState>(
                &transfer.path().join("state.json"),
            ) {
                Ok(state) => state.completed_unix_ms.is_none() && state.failed_error.is_none(),
                Err(_) => true,
            };
            if !should_count {
                continue;
            }

            let size = dir_size_bytes(&chunks)?;
            peer_partial = peer_partial.saturating_add(size);
        }

        if peer_partial > 0 {
            cache.per_peer_partial_bytes.insert(peer_name, peer_partial);
            cache.global_partial_bytes = cache.global_partial_bytes.saturating_add(peer_partial);
        }
    }

    cache.updated_at_unix_ms = now_unix_ms();
    save_spool_usage_cache(&cache)?;
    Ok(cache)
}

fn finalize_completed_transfer(transfer: &mut IncomingTransferState) -> Result<(), String> {
    if transfer.completed_unix_ms.is_some() {
        return Ok(());
    }

    let transfer_dir = incoming_transfer_dir(&transfer.sender_wayfarer_id, &transfer.transfer_id);
    let _lock = ActiveTransferLock::acquire(&transfer_dir)?;

    for chunk_index in 0..transfer.chunk_count {
        let path = incoming_chunk_path(
            &transfer.sender_wayfarer_id,
            &transfer.transfer_id,
            chunk_index,
        );
        if !path.exists() {
            return Err("missing_chunk_at_finalize".to_string());
        }
        let bytes = fs::read(&path)
            .map_err(|err| format!("failed reading chunk {} for finalize: {err}", chunk_index))?;
        let expected = expected_chunk_len(transfer, chunk_index)?;
        if bytes.len() != expected {
            return Err("chunk_expected_length_mismatch".to_string());
        }
    }

    let complete_dir = media_complete_dir();
    fs::create_dir_all(&complete_dir).map_err(|err| {
        format!(
            "failed creating complete media dir {}: {err}",
            complete_dir.display()
        )
    })?;
    let tmp_path = complete_bin_path(&transfer.object_sha256_hex)
        .with_extension(format!("bin.tmp-{}", random_u64_hex()));
    let final_path = complete_bin_path(&transfer.object_sha256_hex);
    let mut writer = fs::File::create(&tmp_path).map_err(|err| {
        format!(
            "failed creating temp completed media file {}: {err}",
            tmp_path.display()
        )
    })?;
    let mut hasher = Sha256::new();

    for chunk_index in 0..transfer.chunk_count {
        let path = incoming_chunk_path(
            &transfer.sender_wayfarer_id,
            &transfer.transfer_id,
            chunk_index,
        );
        let bytes = fs::read(&path)
            .map_err(|err| format!("failed reading chunk {} for reassembly: {err}", chunk_index))?;
        writer.write_all(&bytes).map_err(|err| {
            format!(
                "failed writing chunk {} to completed object: {err}",
                chunk_index
            )
        })?;
        hasher.update(&bytes);
    }
    writer.flush().map_err(|err| {
        format!(
            "failed flushing completed object {}: {err}",
            tmp_path.display()
        )
    })?;

    let object_hash = bytes_to_hex_lower(&hasher.finalize());
    if object_hash != transfer.object_sha256_hex {
        let _ = fs::remove_file(&tmp_path);
        return Err("object_hash_mismatch".to_string());
    }
    fs::rename(&tmp_path, &final_path).map_err(|err| {
        format!(
            "failed moving completed media object {} -> {}: {err}",
            tmp_path.display(),
            final_path.display()
        )
    })?;

    let completed_at = now_unix_ms();
    let meta = CompletedTransferMeta {
        object_sha256_hex: transfer.object_sha256_hex.clone(),
        file_name: transfer.file_name.clone(),
        mime_type: transfer.mime_type.clone(),
        size_bytes: transfer.total_bytes,
        completed_at_unix_ms: completed_at,
        last_access_unix_ms: completed_at,
    };
    write_json_file_atomic(&complete_meta_path(&transfer.object_sha256_hex), &meta)?;

    remove_transfer_chunks_and_adjust_usage(transfer)?;

    transfer.completed_unix_ms = Some(completed_at);
    transfer.received_chunks = transfer.chunk_count;
    transfer.received_bytes = transfer.total_bytes;
    persist_incoming_state(transfer)?;
    sweep_complete_store()?;
    Ok(())
}

#[allow(dead_code)] // Reserved for pending corrupt-chunk strike threshold wiring.
fn strike_or_abort_corrupt_chunk(
    transfer: &mut IncomingTransferState,
    chunk_index: u32,
    reason: &str,
    chat: &mut PersistedChatState,
) -> Result<(), String> {
    let next_strikes = {
        let strikes = transfer.strikes.entry(chunk_index).or_insert(0);
        *strikes = strikes.saturating_add(1);
        *strikes
    };

    if next_strikes >= CORRUPT_CHUNK_ABORT_STRIKES {
        remove_transfer_chunks_and_adjust_usage(transfer)?;
        transfer.received_chunks = 0;
        transfer.received_bytes = 0;
        transfer.failed_error = Some("corrupt_chunk_threshold_exceeded".to_string());
        persist_incoming_state(transfer)?;
        update_chat_media_progress(
            chat,
            &transfer.sender_wayfarer_id,
            &transfer.transfer_id,
            transfer.received_chunks,
            transfer.received_bytes,
            MediaTransferStatus::Failed,
            Some("corrupt_chunk_threshold_exceeded".to_string()),
        );
    } else {
        persist_incoming_state(transfer)?;
    }
    log_verbose(&format!(
        "media_chunk_corrupt: transfer_id={} chunk_index={} strikes={} reason={}",
        transfer.transfer_id, chunk_index, next_strikes, reason
    ));
    Ok(())
}

fn mark_transfer_failed(
    transfer_id: &str,
    sender_wayfarer_id: Option<&str>,
    error: &str,
) -> Result<(), String> {
    let Some(mut state) = load_incoming_state(sender_wayfarer_id, transfer_id)? else {
        return Ok(());
    };
    if state.completed_unix_ms.is_some() {
        return Ok(());
    }
    if state.failed_error.is_some() {
        return Ok(());
    }
    remove_transfer_chunks_and_adjust_usage(&state)?;
    state.received_chunks = 0;
    state.received_bytes = 0;
    state.failed_error = Some(error.to_string());
    persist_incoming_state(&state)
}

#[allow(dead_code)] // Reserved for pending compact chat media status updates.
fn update_chat_media_state(
    chat: &mut PersistedChatState,
    sender_wayfarer_id: &str,
    transfer_id: &str,
    status: MediaTransferStatus,
    error: Option<String>,
) -> bool {
    update_chat_media_progress(chat, sender_wayfarer_id, transfer_id, 0, 0, status, error)
}

#[allow(dead_code)] // Reserved for pending inbound transfer progress updates.
fn update_chat_media_progress(
    chat: &mut PersistedChatState,
    sender_wayfarer_id: &str,
    transfer_id: &str,
    received_chunks: u32,
    received_bytes: u64,
    status: MediaTransferStatus,
    error: Option<String>,
) -> bool {
    let Some(thread) = chat.threads.get_mut(sender_wayfarer_id) else {
        return false;
    };
    for message in thread.iter_mut().rev() {
        let Some(media) = message.media.as_mut() else {
            continue;
        };
        if media.transfer_id != transfer_id {
            continue;
        }
        let next_chunks = received_chunks.min(media.chunk_count);
        let next_bytes = received_bytes.min(media.total_bytes);
        let changed = media.received_chunks != next_chunks
            || media.received_bytes != next_bytes
            || media.status != status
            || media.error != error;
        media.received_chunks = next_chunks;
        media.received_bytes = next_bytes;
        media.status = status.clone();
        media.error = error.clone();
        return changed;
    }
    false
}

#[allow(dead_code)] // Reserved for pending manifest-to-chat projection wiring.
fn upsert_manifest_chat_message(
    chat: &mut PersistedChatState,
    pulled: &EncounterMessagePreview,
    manifest: &MediaManifestMessage,
    status: MediaTransferStatus,
    error: Option<String>,
) -> bool {
    let sender = pulled
        .author_wayfarer_id
        .clone()
        .filter(|value| is_valid_wayfarer_id(value))
        .unwrap_or_else(|| "unknown-peer".to_string());
    let thread = chat.threads.entry(sender.clone()).or_default();

    if let Some(existing) = thread.iter_mut().find(|message| {
        message
            .media
            .as_ref()
            .map(|media| media.transfer_id.as_str() == manifest.transfer_id.as_str())
            .unwrap_or(false)
    }) {
        if let Some(media) = existing.media.as_mut() {
            let next_chunks = media.received_chunks.max(0);
            let next_bytes = media.received_bytes.max(0);
            let changed = media.received_chunks != next_chunks
                || media.received_bytes != next_bytes
                || media.status != status
                || media.error != error;
            media.received_chunks = next_chunks;
            media.received_bytes = next_bytes;
            media.status = status;
            media.error = error;
            return changed;
        }
        return false;
    }

    let caption = manifest
        .caption
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("[Image] {}", manifest.file_name));
    let created_at_unix_ms = manifest
        .sent_at_unix_ms
        .max((pulled.received_at_unix.max(0) as u64) * 1000);
    let created_at_unix = (created_at_unix_ms / 1000) as i64;

    thread.push(ChatMessage {
        msg_id: pulled.item_id.clone(),
        text: caption,
        timestamp: (created_at_unix).to_string(),
        created_at_unix,
        created_at_unix_ms,
        direction: ChatDirection::Incoming,
        seen: chat.selected_contact.as_deref() == Some(sender.as_str()),
        manifest_id_hex: pulled.manifest_id_hex.clone(),
        delivered_at: None,
        outbound_state: None,
        expires_at_unix_ms: Some(manifest.expires_at_unix_ms),
        last_sync_attempt_unix_ms: None,
        last_sync_error: None,
        attachment: Some(ChatAttachment {
            file_name: manifest.file_name.clone(),
            mime_type: manifest.mime_type.clone(),
            size_bytes: manifest.total_bytes,
            content_b64: None,
        }),
        media: Some(ChatMediaTransfer {
            transfer_id: manifest.transfer_id.clone(),
            object_sha256_hex: manifest.object_sha256_hex.clone(),
            file_name: manifest.file_name.clone(),
            mime_type: manifest.mime_type.clone(),
            total_bytes: manifest.total_bytes,
            chunk_count: manifest.chunk_count,
            received_chunks: 0,
            received_bytes: 0,
            status,
            error,
            expires_at_unix_ms: Some(manifest.expires_at_unix_ms),
        }),
    });
    true
}

fn send_missing_request_if_due(
    transfer: &IncomingTransferState,
    local_wayfarer_id: &str,
    author_signing_seed: &[u8; 32],
    ttl_seconds_max: u64,
) -> Result<bool, String> {
    if transfer.chunk_count == 0 {
        return Ok(false);
    }
    if transfer.failed_error.is_some() || transfer.completed_unix_ms.is_some() {
        return Ok(false);
    }
    if transfer.expires_at_unix_ms <= now_unix_ms() {
        return Ok(false);
    }
    if !is_valid_wayfarer_id(&transfer.sender_wayfarer_id) {
        return Ok(false);
    }

    let mut state = transfer.clone();
    let now_ms = now_unix_ms();
    let min_interval = media_limits().missing_min_interval_ms;
    if now_ms.saturating_sub(state.last_missing_unix_ms) < min_interval {
        return Ok(false);
    }

    let plan = compute_missing_plan(&state)?;
    if plan.ranges.is_empty() {
        return Ok(false);
    }
    let mut mirror_tcp = false;
    if let Some((age_ms, requested_chunks, delivered_chunks, pending_chunks)) =
        note_missing_request_plan(
            &state.sender_wayfarer_id,
            &state.transfer_id,
            &plan.selected_chunks,
            now_ms,
        )
    {
        if delivered_chunks == 0 && age_ms >= missing_request_tcp_mirror_stall_ms() {
            mirror_tcp = true;
        }
        log_verbose(&format!(
            "media_missing_yield_rollover: local={} sender={} transfer_id={} age_ms={} requested_chunks={} delivered_chunks={} pending_chunks={}",
            local_wayfarer_id,
            state.sender_wayfarer_id,
            state.transfer_id,
            age_ms,
            requested_chunks,
            delivered_chunks,
            pending_chunks
        ));
    }
    let request = MediaMissingMessage {
        msg_type: MEDIA_MISSING_TYPE.to_string(),
        transfer_id: state.transfer_id.clone(),
        object_sha256_hex: state.object_sha256_hex.clone(),
        ranges: plan.ranges.clone(),
        sent_at_unix_ms: now_ms,
    };

    let body = serde_json::to_string(&request)
        .map_err(|err| format!("failed serializing media missing request: {err}"))?;
    let payload = build_envelope_payload_b64_from_utf8(
        &state.sender_wayfarer_id,
        &body,
        author_signing_seed,
    )?;
    if payload.len() > media_limits().max_item_payload_b64_bytes {
        return Ok(false);
    }
    rate_limit_outbound(payload.len(), now_ms, false, false)?;
    let expiry = apply_expiry_authority(state.expires_at_unix_ms, now_ms, ttl_seconds_max);
    let item_id = gossip_record_local_payload(&payload, expiry)?;
    let redundancy = missing_fastlane_redundancy();
    for _ in 0..redundancy {
        queue_pending_media_control_unicast(PendingMediaControlUnicast {
            peer_wayfarer_id: state.sender_wayfarer_id.clone(),
            item_id: item_id.clone(),
            payload_b64: payload.clone(),
            expiry_unix_ms: expiry,
            kind: PendingMediaControlKind::MissingRequest,
            transfer_id: Some(state.transfer_id.clone()),
            tcp_mirror: mirror_tcp,
        });
    }
    state.last_missing_unix_ms = now_ms;
    persist_incoming_state(&state)?;
    if is_valid_wayfarer_id(local_wayfarer_id) {
        let first_chunk = plan.selected_chunks.first().copied().unwrap_or(0);
        let last_chunk = plan.selected_chunks.last().copied().unwrap_or(0);
        log_verbose(&format!(
            "media_missing_plan: local={} sender={} transfer_id={} missing_total={} selected_chunks={} ranges={} cursor_start={} cursor_next={} first_chunk={} last_chunk={}",
            local_wayfarer_id,
            state.sender_wayfarer_id,
            state.transfer_id,
            plan.missing_total,
            plan.selected_chunks.len(),
            request.ranges.len(),
            plan.cursor_start,
            plan.cursor_next,
            first_chunk,
            last_chunk
        ));
        log_verbose(&format!(
            "media_missing_requested: local={} sender={} transfer_id={} ranges={}",
            local_wayfarer_id,
            state.sender_wayfarer_id,
            state.transfer_id,
            request.ranges.len()
        ));
    }
    Ok(true)
}

#[allow(dead_code)] // Reserved for pending direct missing-range planner tests.
fn compute_missing_ranges(transfer: &IncomingTransferState) -> Result<Vec<ChunkRange>, String> {
    Ok(compute_missing_plan(transfer)?.ranges)
}

fn compute_missing_plan(transfer: &IncomingTransferState) -> Result<MissingRequestPlan, String> {
    let mut missing = Vec::new();
    for chunk_index in 0..transfer.chunk_count {
        let path = incoming_chunk_path(
            &transfer.sender_wayfarer_id,
            &transfer.transfer_id,
            chunk_index,
        );
        if !path.exists() {
            missing.push(chunk_index);
        }
    }
    if missing.is_empty() {
        return Ok(MissingRequestPlan {
            ranges: Vec::new(),
            selected_chunks: Vec::new(),
            missing_total: 0,
            cursor_start: 0,
            cursor_next: 0,
        });
    }

    let request_key = format!("{}:{}", transfer.sender_wayfarer_id, transfer.transfer_id);
    let cursor = missing_request_cursor_tracker()
        .lock()
        .ok()
        .and_then(|tracker| tracker.get(&request_key).copied())
        .unwrap_or(0);
    let missing_total = missing.len();
    let start_pos = missing
        .iter()
        .position(|chunk_index| *chunk_index >= cursor)
        .unwrap_or(0);

    let target_chunks = MISSING_REQUEST_CHUNK_TARGET.min(missing.len());
    let mut selected = Vec::with_capacity(target_chunks);
    for offset in 0..target_chunks {
        let idx = (start_pos + offset) % missing.len();
        selected.push(missing[idx]);
    }

    let next_cursor = selected
        .last()
        .map(|last| last.saturating_add(1) % transfer.chunk_count.max(1))
        .unwrap_or(cursor);
    if let Ok(mut tracker) = missing_request_cursor_tracker().lock() {
        tracker.insert(request_key, next_cursor);
    }

    let mut ranges = Vec::new();
    let mut start = selected[0];
    let mut prev = selected[0];
    for chunk in selected.iter().copied().skip(1) {
        if chunk == prev.saturating_add(1) {
            prev = chunk;
            continue;
        }
        ranges.push(ChunkRange { start, end: prev });
        if ranges.len() >= MISSING_RANGE_CAP {
            return Ok(MissingRequestPlan {
                ranges,
                selected_chunks: selected,
                missing_total,
                cursor_start: cursor,
                cursor_next: next_cursor,
            });
        }
        start = chunk;
        prev = chunk;
    }
    if ranges.len() < MISSING_RANGE_CAP {
        ranges.push(ChunkRange { start, end: prev });
    }
    Ok(MissingRequestPlan {
        ranges,
        selected_chunks: selected,
        missing_total,
        cursor_start: cursor,
        cursor_next: next_cursor,
    })
}

#[allow(dead_code)] // Reserved for pending missing-request interval suppression wiring.
fn missing_request_interval_ok(sender_wayfarer_id: &str, transfer_id: &str, now_ms: u64) -> bool {
    let key = format!("{sender_wayfarer_id}:{transfer_id}");
    let min_interval = media_limits().missing_min_interval_ms;
    let Ok(mut tracker) = missing_request_tracker().lock() else {
        return false;
    };
    let previous = tracker.get(&key).copied().unwrap_or(0);
    if now_ms.saturating_sub(previous) < min_interval {
        return false;
    }
    tracker.insert(key, now_ms);
    true
}

#[allow(dead_code)] // Reserved for pending inbound spool quota enforcement wiring.
fn ensure_spool_quota(sender_wayfarer_id: &str, incoming_chunk_bytes: u64) -> Result<(), String> {
    let cache = load_or_reconcile_spool_usage_cache()?;
    let peer_partial = cache
        .per_peer_partial_bytes
        .get(sender_wayfarer_id)
        .copied()
        .unwrap_or(0);

    if cache
        .global_partial_bytes
        .saturating_add(incoming_chunk_bytes)
        > SPOOL_GLOBAL_PARTIAL_BYTES
    {
        return Err("media spool global quota exceeded".to_string());
    }
    if peer_partial.saturating_add(incoming_chunk_bytes) > SPOOL_PER_PEER_PARTIAL_BYTES {
        return Err("media spool per-peer quota exceeded".to_string());
    }
    Ok(())
}

fn store_orphan_chunk(sender_wayfarer_id: &str, chunk: &MediaChunkMessage) -> Result<(), String> {
    let max_item_payload_b64_bytes = negotiated_payload_b64_limit_for_sender(sender_wayfarer_id);
    if validate_orphan_chunk_predecode_bounds(chunk, max_item_payload_b64_bytes).is_err() {
        return Ok(());
    }

    if !is_valid_wayfarer_id(sender_wayfarer_id) {
        return Err("orphan senderWayfarerId must be lowercase hex64".to_string());
    }
    if !is_valid_sha256_hex(&chunk.object_sha256_hex) {
        return Err("orphan objectSha256Hex must be lowercase hex64".to_string());
    }
    if !is_uuid_v4(&chunk.transfer_id) {
        return Err("orphan transferId must be UUIDv4".to_string());
    }

    maybe_sweep_orphans()?;
    let _orphan_lock = orphan_usage_lock()
        .lock()
        .map_err(|_| "orphan usage lock poisoned".to_string())?;

    let root = media_orphans_dir();
    fs::create_dir_all(&root)
        .map_err(|err| format!("failed creating media orphan dir {}: {err}", root.display()))?;

    let mut usage = load_or_reconcile_orphan_usage_cache()?;
    let has_peer = usage.per_peer.contains_key(sender_wayfarer_id);
    if usage.per_peer.len() >= ORPHAN_MAX_PEERS && !has_peer {
        return Ok(());
    }

    let peer_dir = root.join(sender_wayfarer_id);
    fs::create_dir_all(&peer_dir).map_err(|err| {
        format!(
            "failed creating orphan peer dir {}: {err}",
            peer_dir.display()
        )
    })?;

    let canonical_root = canonicalize_existing_dir(&root)?;
    let canonical_peer_dir = canonicalize_existing_dir(&peer_dir)?;
    if !canonical_peer_dir.starts_with(&canonical_root) {
        return Err("orphan peer directory escaped orphan root".to_string());
    }

    let incoming_bytes = chunk.chunk_bytes as u64;
    let (peer_declared_bytes, peer_meta_files) = usage
        .per_peer
        .get(sender_wayfarer_id)
        .map(|entry| (entry.declared_bytes, entry.meta_files))
        .unwrap_or((0, 0));
    if peer_declared_bytes.saturating_add(incoming_bytes) > ORPHAN_PER_PEER_BYTES {
        return Ok(());
    }
    if usage.global_declared_bytes.saturating_add(incoming_bytes) > ORPHAN_GLOBAL_BYTES {
        return Ok(());
    }
    if peer_meta_files >= ORPHAN_PER_PEER_META_FILES {
        return Ok(());
    }
    if usage.global_meta_files >= ORPHAN_GLOBAL_META_FILES {
        return Ok(());
    }

    let orphan_meta = OrphanChunkMeta {
        sender_wayfarer_id: sender_wayfarer_id.to_string(),
        transfer_id: chunk.transfer_id.clone(),
        object_sha256_hex: chunk.object_sha256_hex.clone(),
        chunk_index: chunk.chunk_index,
        chunk_bytes: chunk.chunk_bytes,
        payload_b64_len: chunk.payload_b64.len() as u64,
        expires_at_unix_ms: chunk.expires_at_unix_ms,
        stored_at_unix_ms: now_unix_ms(),
    };
    let meta_path = orphan_meta_path(&canonical_peer_dir, sender_wayfarer_id, chunk)?;
    write_json_file_atomic(&meta_path, &orphan_meta)?;

    let peer_usage = usage
        .per_peer
        .entry(sender_wayfarer_id.to_string())
        .or_default();
    peer_usage.declared_bytes = peer_usage.declared_bytes.saturating_add(incoming_bytes);
    peer_usage.meta_files = peer_usage.meta_files.saturating_add(1);
    usage.global_declared_bytes = usage.global_declared_bytes.saturating_add(incoming_bytes);
    usage.global_meta_files = usage.global_meta_files.saturating_add(1);
    usage.updated_at_unix_ms = now_unix_ms();
    save_orphan_usage_cache(&usage)
}

fn sweep_orphans() -> Result<(), String> {
    let _orphan_lock = orphan_usage_lock()
        .lock()
        .map_err(|_| "orphan usage lock poisoned".to_string())?;
    let root = media_orphans_dir();
    if !root.exists() {
        let cache_path = orphan_usage_cache_path();
        if cache_path.exists() {
            let _ = fs::remove_file(cache_path);
        }
        return Ok(());
    }
    let now = now_unix_ms();

    let mut usage = OrphanUsageCache::default();
    for peer in fs::read_dir(&root)
        .map_err(|err| format!("failed reading orphan root {}: {err}", root.display()))?
    {
        let peer = peer.map_err(|err| format!("failed reading orphan peer entry: {err}"))?;
        if !peer.path().is_dir() {
            continue;
        }
        let peer_name = peer.file_name().to_string_lossy().to_string();
        if !is_valid_wayfarer_id(&peer_name) {
            let _ = fs::remove_dir_all(peer.path());
            continue;
        }

        let mut peer_usage = OrphanPeerUsage::default();
        for file in fs::read_dir(peer.path()).map_err(|err| {
            format!(
                "failed reading orphan peer dir {}: {err}",
                peer.path().display()
            )
        })? {
            let file = file.map_err(|err| format!("failed reading orphan file entry: {err}"))?;
            let path = file.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                let _ = fs::remove_file(path);
                continue;
            }
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let Ok(age) = modified.elapsed() else {
                continue;
            };
            let expired_by_age = age.as_millis() as u64 > ORPHAN_MAX_AGE_MS || now == 0;
            let meta = read_json_file::<OrphanChunkMeta>(&path);
            let expired_by_message = meta
                .as_ref()
                .map(|meta| {
                    meta.expires_at_unix_ms <= now
                        || !is_valid_wayfarer_id(&meta.sender_wayfarer_id)
                        || !is_uuid_v4(&meta.transfer_id)
                        || !is_valid_sha256_hex(&meta.object_sha256_hex)
                })
                .unwrap_or(true);
            if expired_by_age || expired_by_message {
                let _ = fs::remove_file(path);
            } else if let Ok(meta) = meta {
                peer_usage.declared_bytes = peer_usage
                    .declared_bytes
                    .saturating_add(meta.chunk_bytes as u64);
                peer_usage.meta_files = peer_usage.meta_files.saturating_add(1);
            }
        }
        if peer_usage.meta_files == 0 {
            let _ = fs::remove_dir(peer.path());
            continue;
        }
        usage.global_declared_bytes = usage
            .global_declared_bytes
            .saturating_add(peer_usage.declared_bytes);
        usage.global_meta_files = usage
            .global_meta_files
            .saturating_add(peer_usage.meta_files);
        usage.per_peer.insert(peer_name, peer_usage);
    }

    usage.updated_at_unix_ms = now;
    save_orphan_usage_cache(&usage)?;

    Ok(())
}

fn orphan_meta_path(
    peer_dir: &Path,
    sender_wayfarer_id: &str,
    chunk: &MediaChunkMessage,
) -> Result<PathBuf, String> {
    let digest = bytes_to_hex_lower(&Sha256::digest(format!(
        "orphan|{}|{}|{}|{}",
        sender_wayfarer_id, chunk.transfer_id, chunk.object_sha256_hex, chunk.chunk_index
    )));
    let candidate = peer_dir.join(format!("{digest}.json"));
    if !candidate.starts_with(peer_dir) {
        return Err("orphan meta path escaped peer directory".to_string());
    }
    Ok(candidate)
}

fn sweep_expired_incoming_transfers() -> Result<(), String> {
    let spool = media_spool_dir();
    if !spool.exists() {
        return Ok(());
    }
    let now = now_unix_ms();
    for peer in fs::read_dir(&spool)
        .map_err(|err| format!("failed reading spool root {}: {err}", spool.display()))?
    {
        let peer = peer.map_err(|err| format!("failed reading spool peer entry: {err}"))?;
        if !peer.path().is_dir() {
            continue;
        }
        for transfer in fs::read_dir(peer.path()).map_err(|err| {
            format!(
                "failed reading transfer dir {}: {err}",
                peer.path().display()
            )
        })? {
            let transfer =
                transfer.map_err(|err| format!("failed reading transfer entry: {err}"))?;
            let dir = transfer.path();
            if !dir.is_dir() {
                continue;
            }
            if dir.join(".active.lock").exists() {
                continue;
            }
            let state_path = dir.join("state.json");
            if !state_path.exists() {
                continue;
            }
            let mut state = read_json_file::<IncomingTransferState>(&state_path)?;
            if state.completed_unix_ms.is_some() {
                continue;
            }
            recover_transfer_progress_from_disk(&mut state, true)?;
            if state.completed_unix_ms.is_some() {
                continue;
            }
            if state.expires_at_unix_ms <= now {
                mark_transfer_failed(
                    &state.transfer_id,
                    Some(&state.sender_wayfarer_id),
                    "transfer_expired",
                )?;
            }
        }
    }
    Ok(())
}

fn expire_chat_media_messages(ttl_seconds_max: u64) -> Result<bool, String> {
    let mut chat = load_chat_state()?;
    let mut changed = false;
    let now = now_unix_ms();
    let ttl_cap = now.saturating_add(ttl_seconds_max.saturating_mul(1000));

    for (sender_wayfarer_id, thread) in chat.threads.iter_mut() {
        for message in thread.iter_mut() {
            let Some(media) = message.media.as_mut() else {
                continue;
            };

            if matches!(message.direction, ChatDirection::Incoming)
                && is_valid_wayfarer_id(sender_wayfarer_id)
            {
                if let Some(mut state) =
                    load_incoming_state(Some(sender_wayfarer_id), &media.transfer_id)?
                {
                    recover_transfer_progress_from_disk(&mut state, true)?;

                    let next_chunks = state.received_chunks.min(media.chunk_count);
                    let next_bytes = state.received_bytes.min(media.total_bytes);
                    if media.received_chunks != next_chunks {
                        media.received_chunks = next_chunks;
                        changed = true;
                    }
                    if media.received_bytes != next_bytes {
                        media.received_bytes = next_bytes;
                        changed = true;
                    }

                    let (status, error) = transfer_status_and_error(&state);
                    if media.status != status {
                        media.status = status;
                        changed = true;
                    }
                    if media.error != error {
                        media.error = error;
                        changed = true;
                    }
                }
            }

            let expires = media.expires_at_unix_ms.unwrap_or(ttl_cap).min(ttl_cap);
            if media.expires_at_unix_ms != Some(expires) {
                media.expires_at_unix_ms = Some(expires);
                changed = true;
            }
            if media.status != MediaTransferStatus::Complete
                && media.status != MediaTransferStatus::Failed
                && expires <= now
            {
                media.status = MediaTransferStatus::Failed;
                media.error = Some("transfer_expired".to_string());
                changed = true;
            }
        }
    }

    if changed {
        save_chat_state(&chat)?;
    }
    Ok(changed)
}

fn sweep_complete_store() -> Result<(), String> {
    let dir = media_complete_dir();
    if !dir.exists() {
        return Ok(());
    }

    let now = now_unix_ms();
    let mut entries = Vec::new();
    for entry in fs::read_dir(&dir)
        .map_err(|err| format!("failed reading complete media dir {}: {err}", dir.display()))?
    {
        let entry = entry.map_err(|err| format!("failed reading complete media entry: {err}"))?;
        let path = entry.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let meta = read_json_file::<CompletedTransferMeta>(&path)?;
        let bin_path = complete_bin_path(&meta.object_sha256_hex);
        if !bin_path.exists() {
            let _ = fs::remove_file(path);
            continue;
        }
        if now.saturating_sub(meta.completed_at_unix_ms) > COMPLETE_MAX_AGE_MS {
            let _ = fs::remove_file(&bin_path);
            let _ = fs::remove_file(path);
            continue;
        }
        let size = fs::metadata(&bin_path)
            .map_err(|err| {
                format!(
                    "failed stat complete media object {}: {err}",
                    bin_path.display()
                )
            })?
            .len();
        entries.push((
            meta.last_access_unix_ms.max(meta.completed_at_unix_ms),
            size,
            bin_path,
            path,
        ));
    }

    let mut total: u64 = entries.iter().map(|(_, size, _, _)| *size).sum();
    if total <= COMPLETE_MAX_BYTES {
        return Ok(());
    }
    entries.sort_by_key(|(last_access, _, _, _)| *last_access);
    for (_, size, bin_path, meta_path) in entries {
        if total <= COMPLETE_MAX_BYTES {
            break;
        }
        let _ = fs::remove_file(&bin_path);
        let _ = fs::remove_file(&meta_path);
        total = total.saturating_sub(size);
    }
    Ok(())
}

fn rate_limit_outbound(
    wire_bytes: usize,
    now_ms: u64,
    is_manifest: bool,
    is_chunk: bool,
) -> Result<(), String> {
    let Ok(mut limiter) = outbound_limiter().lock() else {
        return Err("media outbound limiter poisoned".to_string());
    };

    let sustained_per_min = wire_bucket_sustained_bytes_per_min();
    let burst_bytes = wire_bucket_burst_bytes();

    let elapsed_ms = now_ms.saturating_sub(limiter.last_refill_unix_ms);
    let refill = (sustained_per_min / 60_000.0) * elapsed_ms as f64;
    limiter.tokens = (limiter.tokens + refill).min(burst_bytes);
    limiter.last_refill_unix_ms = now_ms;

    while let Some(front) = limiter.manifest_sends.front().copied() {
        if now_ms.saturating_sub(front) <= 60 * 60 * 1000 {
            break;
        }
        limiter.manifest_sends.pop_front();
    }
    while let Some(front) = limiter.chunk_sends.front().copied() {
        if now_ms.saturating_sub(front) <= 60 * 1000 {
            break;
        }
        limiter.chunk_sends.pop_front();
    }

    if is_manifest && limiter.manifest_sends.len() >= MANIFESTS_PER_HOUR_LIMIT {
        return Err("media manifest rate limit exceeded".to_string());
    }
    if is_chunk && limiter.chunk_sends.len() >= chunks_per_minute_limit() {
        return Err("media chunk rate limit exceeded".to_string());
    }

    let required = wire_bytes as f64;
    if required > burst_bytes {
        return Err("media wire byte budget exceeded burst".to_string());
    }
    if limiter.tokens < required {
        return Err("media wire byte token bucket exhausted".to_string());
    }
    limiter.tokens -= required;
    if is_manifest {
        limiter.manifest_sends.push_back(now_ms);
    }
    if is_chunk {
        limiter.chunk_sends.push_back(now_ms);
    }
    Ok(())
}

fn wire_bucket_sustained_bytes_per_min() -> f64 {
    if std::env::var("AETHOS_MEDIA_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN").is_ok() {
        return env_u64_clamped(
            "AETHOS_MEDIA_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN",
            WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN as u64,
            1 * 1024 * 1024,
            512 * 1024 * 1024,
        ) as f64;
    }
    if !e2e_enabled() {
        return WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN;
    }
    env_u64_clamped(
        "AETHOS_MEDIA_E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN",
        E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN_DEFAULT,
        8 * 1024 * 1024,
        512 * 1024 * 1024,
    ) as f64
}

fn wire_bucket_burst_bytes() -> f64 {
    if std::env::var("AETHOS_MEDIA_WIRE_BUCKET_BURST_BYTES").is_ok() {
        return env_u64_clamped(
            "AETHOS_MEDIA_WIRE_BUCKET_BURST_BYTES",
            WIRE_BUCKET_BURST_BYTES as u64,
            2 * 1024 * 1024,
            512 * 1024 * 1024,
        ) as f64;
    }
    if !e2e_enabled() {
        return WIRE_BUCKET_BURST_BYTES;
    }
    env_u64_clamped(
        "AETHOS_MEDIA_E2E_WIRE_BUCKET_BURST_BYTES",
        E2E_WIRE_BUCKET_BURST_BYTES_DEFAULT,
        16 * 1024 * 1024,
        512 * 1024 * 1024,
    ) as f64
}

#[allow(dead_code)] // Reserved for pending missing-response dedup e2e tuning wiring.
fn missing_response_chunk_dedup_window_ms() -> u64 {
    if !e2e_enabled() {
        return MISSING_RESPONSE_CHUNK_DEDUP_WINDOW_MS;
    }
    env_u64_clamped(
        "AETHOS_MEDIA_E2E_MISSING_RESPONSE_DEDUP_WINDOW_MS",
        E2E_MISSING_RESPONSE_CHUNK_DEDUP_WINDOW_MS_DEFAULT,
        200,
        10_000,
    )
}

#[allow(dead_code)] // Reserved for pending missing-response chunk ceiling e2e tuning wiring.
fn missing_response_chunk_ceiling() -> usize {
    if !e2e_enabled() {
        return MISSING_RESPONSE_CHUNK_CEILING;
    }
    env_u64_clamped(
        "AETHOS_MEDIA_E2E_MISSING_RESPONSE_CHUNK_CEILING",
        E2E_MISSING_RESPONSE_CHUNK_CEILING_DEFAULT as u64,
        16,
        256,
    ) as usize
}

fn chunks_per_minute_limit() -> usize {
    if !e2e_enabled() {
        return CHUNKS_PER_MINUTE_LIMIT;
    }
    env_u64_clamped(
        "AETHOS_MEDIA_E2E_CHUNKS_PER_MINUTE_LIMIT",
        E2E_CHUNKS_PER_MINUTE_LIMIT_DEFAULT as u64,
        CHUNKS_PER_MINUTE_LIMIT as u64,
        20_000,
    ) as usize
}

fn media_limits() -> MediaLimits {
    let mut max_item_payload_b64_bytes = MAX_ITEM_PAYLOAD_B64_BYTES_DEFAULT;
    let mut missing_min_interval_ms = MISSING_MIN_INTERVAL_MS_DEFAULT;

    if e2e_enabled() {
        max_item_payload_b64_bytes = env_u64_clamped(
            "AETHOS_MEDIA_E2E_MAX_ITEM_PAYLOAD_B64_BYTES",
            max_item_payload_b64_bytes as u64,
            4 * 1024,
            128 * 1024,
        ) as usize;
        missing_min_interval_ms = env_u64_clamped(
            "AETHOS_MEDIA_E2E_MISSING_MIN_INTERVAL_MS",
            missing_min_interval_ms,
            250,
            10_000,
        );
    }

    MediaLimits {
        max_item_payload_b64_bytes,
        missing_min_interval_ms,
    }
}

#[allow(dead_code)] // Reserved for pending media e2e TTL override send wiring.
fn e2e_ttl_override_ms(default_expiry_ms: u64, now_ms: u64) -> u64 {
    if !e2e_enabled() {
        return default_expiry_ms;
    }
    let ttl_seconds = env_u64_clamped(
        "AETHOS_MEDIA_E2E_TTL_SECONDS",
        default_expiry_ms.saturating_sub(now_ms) / 1000,
        5,
        24 * 60 * 60,
    );
    now_ms.saturating_add(ttl_seconds.saturating_mul(1000))
}

#[allow(dead_code)] // Reserved for pending media e2e drop-chunk fault injection wiring.
fn e2e_drop_chunk_index() -> Option<u32> {
    if !e2e_enabled() {
        return None;
    }
    let raw = std::env::var("AETHOS_MEDIA_E2E_DROP_CHUNK_INDEX").ok()?;
    let parsed = raw.trim().parse::<u32>().ok()?;
    Some(parsed)
}

fn apply_expiry_authority(
    manifest_expires_at_unix_ms: u64,
    now_ms: u64,
    ttl_seconds_max: u64,
) -> u64 {
    let ttl_cap = now_ms.saturating_add(ttl_seconds_max.saturating_mul(1000));
    manifest_expires_at_unix_ms.min(ttl_cap)
}

fn expected_chunk_len(transfer: &IncomingTransferState, chunk_index: u32) -> Result<usize, String> {
    if chunk_index >= transfer.chunk_count {
        return Err("chunk index out of range".to_string());
    }
    let chunk_payload = transfer.chunk_payload_bytes as usize;
    let is_last = chunk_index == transfer.chunk_count.saturating_sub(1);
    if !is_last {
        return Ok(chunk_payload);
    }
    let consumed = chunk_payload.saturating_mul(transfer.chunk_count.saturating_sub(1) as usize);
    let total = transfer.total_bytes as usize;
    Ok(total.saturating_sub(consumed))
}

#[allow(dead_code)] // Reserved for pending outbound chunk slicing wiring.
fn chunk_bounds(
    total_bytes: u64,
    chunk_payload_bytes: u32,
    chunk_index: u32,
) -> Result<(usize, usize), String> {
    let chunk_payload = chunk_payload_bytes as usize;
    let start = (chunk_index as usize).saturating_mul(chunk_payload);
    let total = total_bytes as usize;
    if start >= total {
        return Err("chunk start exceeds total bytes".to_string());
    }
    let end = (start + chunk_payload).min(total);
    Ok((start, end))
}

#[allow(dead_code)] // Reserved for pending chunk-count invariant calculations.
fn ceil_div_u64(n: u64, d: u64) -> u64 {
    if d == 0 {
        return 0;
    }
    n.saturating_add(d.saturating_sub(1)) / d
}

fn e2e_enabled() -> bool {
    std::env::var("AETHOS_E2E")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn env_u64_clamped(key: &str, fallback: u64, min: u64, max: u64) -> u64 {
    let parsed = std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(fallback);
    parsed.clamp(min, max)
}

fn uuid_v4_string() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn is_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (idx, byte) in bytes.iter().enumerate() {
        let is_dash = idx == 8 || idx == 13 || idx == 18 || idx == 23;
        if is_dash {
            if *byte != b'-' {
                return false;
            }
            continue;
        }
        if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    bytes[14] == b'4' && matches!(bytes[19], b'8' | b'9' | b'a' | b'b' | b'A' | b'B')
}

fn is_valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[allow(dead_code)] // Reserved for pending inbound chunk persistence wiring.
fn write_chunk_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed creating chunk parent {}: {err}", parent.display()))?;
    }
    let mut file = fs::File::create(path)
        .map_err(|err| format!("failed creating chunk file {}: {err}", path.display()))?;
    file.write_all(bytes)
        .map_err(|err| format!("failed writing chunk file {}: {err}", path.display()))?;
    file.flush()
        .map_err(|err| format!("failed flushing chunk file {}: {err}", path.display()))
}

fn load_capabilities_cache() -> Result<CapabilitiesCache, String> {
    let path = capabilities_cache_path();
    if !path.exists() {
        return Ok(CapabilitiesCache::default());
    }
    read_json_file(&path)
}

#[allow(dead_code)] // Reserved for pending media capabilities cache write-through wiring.
fn save_capabilities_cache(cache: &CapabilitiesCache) -> Result<(), String> {
    write_json_file_atomic(&capabilities_cache_path(), cache)
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let raw = fs::read_to_string(path)
        .map_err(|err| format!("failed reading json file {}: {err}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|err| format!("failed parsing json file {}: {err}", path.display()))
}

fn write_json_file_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed creating directory {}: {err}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("tmp-{}-{}", now_unix_ms(), random_u64_hex()));
    let raw = serde_json::to_string_pretty(value)
        .map_err(|err| format!("failed serializing {}: {err}", path.display()))?;
    fs::write(&tmp, raw)
        .map_err(|err| format!("failed writing temp json file {}: {err}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|err| {
        format!(
            "failed atomically replacing {} with {}: {err}",
            path.display(),
            tmp.display()
        )
    })
}

fn random_u64_hex() -> String {
    format!("{:016x}", rand::thread_rng().next_u64())
}

fn canonicalize_existing_dir(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path)
        .map_err(|err| format!("failed resolving directory {}: {err}", path.display()))
}

fn dir_size_bytes(path: &Path) -> Result<u64, String> {
    if !path.exists() {
        return Ok(0);
    }
    if path.is_file() {
        return fs::metadata(path)
            .map(|meta| meta.len())
            .map_err(|err| format!("failed stat file {}: {err}", path.display()));
    }
    let mut total = 0u64;
    for entry in
        fs::read_dir(path).map_err(|err| format!("failed reading dir {}: {err}", path.display()))?
    {
        let entry = entry.map_err(|err| format!("failed reading dir entry: {err}"))?;
        total = total.saturating_add(dir_size_bytes(&entry.path())?);
    }
    Ok(total)
}

struct ActiveTransferLock {
    path: PathBuf,
}

impl ActiveTransferLock {
    fn acquire(transfer_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(transfer_dir).map_err(|err| {
            format!(
                "failed creating transfer dir {}: {err}",
                transfer_dir.display()
            )
        })?;
        let lock_path = transfer_dir.join(".active.lock");
        fs::write(&lock_path, format!("{}", now_unix_ms())).map_err(|err| {
            format!(
                "failed writing transfer lock {}: {err}",
                lock_path.display()
            )
        })?;
        Ok(Self { path: lock_path })
    }
}

impl Drop for ActiveTransferLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn media_base_dir() -> PathBuf {
    if let Ok(state_dir) = std::env::var("AETHOS_STATE_DIR") {
        if !state_dir.trim().is_empty() {
            return Path::new(&state_dir).join("media");
        }
    }

    if let Ok(xdg_state_home) = std::env::var("XDG_STATE_HOME") {
        if !xdg_state_home.trim().is_empty() {
            return Path::new(&xdg_state_home)
                .join("aethos-linux")
                .join("media");
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return Path::new(&home)
                .join(".local")
                .join("state")
                .join("aethos-linux")
                .join("media");
        }
    }

    std::env::temp_dir().join("aethos-linux").join("media")
}

fn media_spool_dir() -> PathBuf {
    media_base_dir().join("spool")
}

fn media_orphans_dir() -> PathBuf {
    media_spool_dir().join("orphans")
}

fn media_complete_dir() -> PathBuf {
    media_base_dir().join("complete")
}

fn capabilities_cache_path() -> PathBuf {
    media_base_dir().join("capabilities-cache.json")
}

fn orphan_usage_cache_path() -> PathBuf {
    media_orphans_dir().join("usage-cache.json")
}

fn incoming_transfer_dir(sender_wayfarer_id: &str, transfer_id: &str) -> PathBuf {
    media_spool_dir().join(sender_wayfarer_id).join(transfer_id)
}

fn incoming_state_path(sender_wayfarer_id: &str, transfer_id: &str) -> PathBuf {
    incoming_transfer_dir(sender_wayfarer_id, transfer_id).join("state.json")
}

fn incoming_chunk_path(sender_wayfarer_id: &str, transfer_id: &str, chunk_index: u32) -> PathBuf {
    incoming_transfer_dir(sender_wayfarer_id, transfer_id)
        .join("chunks")
        .join(format!("{chunk_index}.bin"))
}

#[allow(dead_code)] // Reserved for pending outbound transfer persistence wiring.
fn outbound_transfer_dir(transfer_id: &str) -> PathBuf {
    media_base_dir().join("outbound").join(transfer_id)
}

#[allow(dead_code)] // Reserved for pending outbound transfer persistence wiring.
fn outbound_state_path(transfer_id: &str) -> PathBuf {
    outbound_transfer_dir(transfer_id).join("state.json")
}

#[allow(dead_code)] // Reserved for pending outbound transfer persistence wiring.
fn outbound_source_path(transfer_id: &str) -> PathBuf {
    outbound_transfer_dir(transfer_id).join("source.bin")
}

fn complete_meta_path(object_sha256_hex: &str) -> PathBuf {
    media_complete_dir().join(format!("{object_sha256_hex}.json"))
}

fn complete_bin_path(object_sha256_hex: &str) -> PathBuf {
    media_complete_dir().join(format!("{object_sha256_hex}.bin"))
}

fn spool_usage_cache_path() -> PathBuf {
    media_spool_dir().join("usage-cache.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::shared_test_env_lock;
    use crate::relay::client::EncounterMessagePreview;

    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &Path) -> Self {
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

    fn unique_state_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", rand::random::<u64>()))
    }

    fn sample_orphan_chunk(object_sha256_hex: &str, transfer_id: &str) -> MediaChunkMessage {
        MediaChunkMessage {
            msg_type: MEDIA_CHUNK_TYPE.to_string(),
            transfer_id: transfer_id.to_string(),
            object_sha256_hex: object_sha256_hex.to_string(),
            chunk_index: 0,
            chunk_sha256_hex: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            chunk_bytes: 1,
            payload_b64: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([1u8]),
            expires_at_unix_ms: now_unix_ms() + 60_000,
        }
    }

    #[test]
    fn uuid_v4_builder_emits_valid_uuid_v4() {
        let value = uuid_v4_string();
        assert!(is_uuid_v4(&value));
    }

    #[test]
    fn missing_ranges_are_capped() {
        let transfer = IncomingTransferState {
            transfer_id: "00000000-0000-4000-8000-000000000000".to_string(),
            sender_wayfarer_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            object_sha256_hex: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_string(),
            file_name: "img.png".to_string(),
            mime_type: "image/png".to_string(),
            total_bytes: 1024,
            chunk_payload_bytes: 1,
            chunk_count: 500,
            expires_at_unix_ms: now_unix_ms() + 10_000,
            sent_at_unix_ms: now_unix_ms(),
            caption: None,
            received_chunks: 0,
            received_bytes: 0,
            strikes: BTreeMap::new(),
            last_missing_unix_ms: 0,
            failed_error: None,
            completed_unix_ms: None,
        };
        let ranges = compute_missing_ranges(&transfer).expect("ranges");
        assert!(ranges.len() <= MISSING_RANGE_CAP);
    }

    #[test]
    fn media_wire_detection_matches_spec_types() {
        let msg = serde_json::json!({"type":"wayfarer.media.chunk.v1","x":1}).to_string();
        assert!(is_media_wire_message(&msg));
        assert!(!is_media_wire_message("{\"type\":\"chat\"}"));
        assert!(!is_media_wire_message("not-json"));
    }

    #[test]
    fn choose_chunk_payload_respects_budget() {
        let to = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let seed = [7u8; 32];
        let payload = choose_chunk_payload_bytes(
            to,
            "00000000-0000-4000-8000-000000000000",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            now_unix_ms() + 30_000,
            1_000_000,
            32 * 1024,
            &seed,
        )
        .expect("choose chunk payload");
        assert!(payload > 0);
    }

    #[test]
    fn predecode_caps_reject_oversized_payload_b64() {
        let result = validate_chunk_predecode_bounds(1000, 32, 32, 32, 256);
        assert_eq!(result, Err("chunk_payload_b64_too_large"));
    }

    #[test]
    fn orphan_store_rejects_invalid_object_sha_without_writing_files() {
        let _lock = shared_test_env_lock().lock().expect("lock");
        let state_dir = unique_state_dir("aethos-media-orphan-invalid-object");
        let _guard = EnvVarGuard::set("AETHOS_STATE_DIR", &state_dir);
        let sender = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let chunk = sample_orphan_chunk(
            "../../../../etc/passwd",
            "00000000-0000-4000-8000-000000000000",
        );

        let result = store_orphan_chunk(sender, &chunk);
        assert_eq!(
            result,
            Err("orphan objectSha256Hex must be lowercase hex64".to_string())
        );
        assert!(!media_orphans_dir().exists());
    }

    #[test]
    fn orphan_store_rejects_invalid_transfer_id_without_writing_files() {
        let _lock = shared_test_env_lock().lock().expect("lock");
        let state_dir = unique_state_dir("aethos-media-orphan-invalid-transfer");
        let _guard = EnvVarGuard::set("AETHOS_STATE_DIR", &state_dir);
        let sender = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let chunk = sample_orphan_chunk(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "../../../../tmp/evil",
        );

        let result = store_orphan_chunk(sender, &chunk);
        assert_eq!(result, Err("orphan transferId must be UUIDv4".to_string()));
        assert!(!media_orphans_dir().exists());
    }

    #[test]
    fn crash_consistency_recovers_progress_and_finalizes() {
        let _lock = shared_test_env_lock().lock().expect("lock");
        let state_dir = unique_state_dir("aethos-media-crash-recover");
        let _guard = EnvVarGuard::set("AETHOS_STATE_DIR", &state_dir);

        let sender = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let transfer_id = "00000000-0000-4000-8000-000000000000";
        let object_bytes = b"hello-media-crash-recovery".to_vec();
        let object_sha256_hex = bytes_to_hex_lower(&Sha256::digest(&object_bytes));
        let mut state = IncomingTransferState {
            transfer_id: transfer_id.to_string(),
            sender_wayfarer_id: sender.to_string(),
            object_sha256_hex: object_sha256_hex.clone(),
            file_name: "image.png".to_string(),
            mime_type: "image/png".to_string(),
            total_bytes: object_bytes.len() as u64,
            chunk_payload_bytes: object_bytes.len() as u32,
            chunk_count: 1,
            expires_at_unix_ms: now_unix_ms() + 60_000,
            sent_at_unix_ms: now_unix_ms(),
            caption: None,
            received_chunks: 0,
            received_bytes: 0,
            strikes: BTreeMap::new(),
            last_missing_unix_ms: 0,
            failed_error: None,
            completed_unix_ms: None,
        };

        let chunk_path = incoming_chunk_path(sender, transfer_id, 0);
        if let Some(parent) = chunk_path.parent() {
            fs::create_dir_all(parent).expect("mkdir chunk parent");
        }
        fs::write(&chunk_path, &object_bytes).expect("write chunk");

        recover_transfer_progress_from_disk(&mut state, true).expect("recover progress");

        assert_eq!(state.received_chunks, 1);
        assert_eq!(state.received_bytes, object_bytes.len() as u64);
        assert!(state.completed_unix_ms.is_some());
        assert!(complete_bin_path(&object_sha256_hex).exists());
        assert!(!chunk_path.exists());
    }

    #[test]
    fn failed_transfer_ignores_subsequent_chunks() {
        let _lock = shared_test_env_lock().lock().expect("lock");
        let state_dir = unique_state_dir("aethos-media-failed-ignore");
        let _guard = EnvVarGuard::set("AETHOS_STATE_DIR", &state_dir);

        let mut transfer = IncomingTransferState {
            transfer_id: "00000000-0000-4000-8000-000000000001".to_string(),
            sender_wayfarer_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            object_sha256_hex: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_string(),
            file_name: "image.png".to_string(),
            mime_type: "image/png".to_string(),
            total_bytes: 3,
            chunk_payload_bytes: 3,
            chunk_count: 1,
            expires_at_unix_ms: now_unix_ms() + 60_000,
            sent_at_unix_ms: now_unix_ms(),
            caption: None,
            received_chunks: 0,
            received_bytes: 0,
            strikes: BTreeMap::new(),
            last_missing_unix_ms: 0,
            failed_error: Some("already_failed".to_string()),
            completed_unix_ms: None,
        };

        assert!(transfer_is_terminal(&transfer));
        recover_transfer_progress_from_disk(&mut transfer, true).expect("recover failed transfer");
        assert_eq!(transfer.failed_error.as_deref(), Some("already_failed"));
        assert_eq!(transfer.received_chunks, 0);
        assert_eq!(transfer.received_bytes, 0);

        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([1u8, 2u8, 3u8]);
        let expected_len = expected_chunk_len(&transfer, 0).expect("expected len");
        let predecode = validate_chunk_predecode_bounds(
            payload_b64.len(),
            3,
            expected_len,
            transfer.chunk_payload_bytes as usize,
            1024,
        );
        assert!(predecode.is_ok());

        let _ = EncounterMessagePreview {
            author_wayfarer_id: Some(transfer.sender_wayfarer_id.clone()),
            session_peer: None,
            transport_peer: None,
            item_id: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_string(),
            body_bytes: b"{}".to_vec(),
            text: "{}".to_string(),
            received_at_unix: now_unix_ms() as i64,
            manifest_id_hex: None,
        };
    }
}
