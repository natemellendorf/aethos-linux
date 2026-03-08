use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::aethos_core::protocol::{
    decode_envelope_payload_b64, decode_envelope_payload_utf8_preview, is_valid_payload_b64,
    is_valid_wayfarer_id,
};

pub const GOSSIP_SYNC_VERSION: u8 = 1;
pub const GOSSIP_CHUNK_SIZE_BYTES: u64 = 32_768;
pub const GOSSIP_LAN_PORT: u16 = 47_655;

const GOSSIP_STORE_FILE_NAME: &str = "gossip-inventory.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GossipSyncFrame {
    #[serde(rename = "inventory_summary")]
    InventorySummary(InventorySummaryFrame),
    #[serde(rename = "missing_request")]
    MissingRequest(MissingRequestFrame),
    #[serde(rename = "transfer")]
    Transfer(TransferFrame),
    #[serde(rename = "receipt")]
    Receipt(ReceiptFrame),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventorySummaryFrame {
    pub sync_version: u8,
    pub session_id: String,
    pub sender_wayfarer_id: String,
    pub page: u32,
    pub has_more: bool,
    pub inventory: Vec<InventoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryEntry {
    pub item_id: String,
    pub manifest_id: String,
    pub to_wayfarer_id: String,
    pub expires_at_unix_ms: u64,
    pub total_size_bytes: u64,
    pub chunk_size_bytes: u64,
    pub chunk_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingRequestFrame {
    pub sync_version: u8,
    pub session_id: String,
    pub sender_wayfarer_id: String,
    pub page: u32,
    pub has_more: bool,
    pub request_id: String,
    pub in_response_to_page: u32,
    pub missing_item_ids: Vec<String>,
    pub max_transfer_items: u32,
    pub max_transfer_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferFrame {
    pub sync_version: u8,
    pub session_id: String,
    pub sender_wayfarer_id: String,
    pub page: u32,
    pub has_more: bool,
    pub transfer_id: String,
    pub in_response_to_request_id: String,
    pub items: Vec<TransferItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferItem {
    pub item_id: String,
    pub manifest_id: String,
    pub to_wayfarer_id: String,
    pub expires_at_unix_ms: u64,
    pub total_size_bytes: u64,
    pub chunk_size_bytes: u64,
    pub chunk_count: u32,
    pub envelope_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptFrame {
    pub sync_version: u8,
    pub session_id: String,
    pub sender_wayfarer_id: String,
    pub page: u32,
    pub has_more: bool,
    pub receipt_id: String,
    pub in_response_to_transfer_id: String,
    pub status: String,
    pub accepted_item_ids: Vec<String>,
    pub rejected_items: Vec<RejectedItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedItem {
    pub item_id: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ImportedEnvelope {
    pub item_id: String,
    pub from_wayfarer_id: String,
    pub text: String,
    pub received_at_unix: i64,
    pub manifest_id_hex: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ImportTransferResult {
    pub accepted_item_ids: Vec<String>,
    pub rejected_items: Vec<RejectedItem>,
    pub new_messages: Vec<ImportedEnvelope>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GossipStore {
    items: BTreeMap<String, StoredItem>,
}

impl Default for GossipStore {
    fn default() -> Self {
        Self {
            items: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredItem {
    item_id: String,
    manifest_id: String,
    to_wayfarer_id: String,
    expires_at_unix_ms: u64,
    total_size_bytes: u64,
    chunk_size_bytes: u64,
    chunk_count: u32,
    envelope_b64: String,
    recorded_at_unix_ms: u64,
}

pub fn serialize_frame(frame: &GossipSyncFrame) -> Result<Vec<u8>, String> {
    let text =
        serde_json::to_string(frame).map_err(|err| format!("serialize gossip frame: {err}"))?;
    Ok(text.into_bytes())
}

pub fn parse_frame(raw: &[u8]) -> Result<GossipSyncFrame, String> {
    let text =
        std::str::from_utf8(raw).map_err(|err| format!("gossip frame utf8 decode: {err}"))?;
    let frame: GossipSyncFrame =
        serde_json::from_str(text).map_err(|err| format!("gossip frame json parse: {err}"))?;
    validate_frame(&frame)?;
    Ok(frame)
}

pub fn validate_frame(frame: &GossipSyncFrame) -> Result<(), String> {
    match frame {
        GossipSyncFrame::InventorySummary(frame) => {
            validate_common(
                frame.sync_version,
                &frame.session_id,
                &frame.sender_wayfarer_id,
                frame.page,
                frame.has_more,
            )?;
            for entry in &frame.inventory {
                validate_inventory_entry(entry)?;
            }
        }
        GossipSyncFrame::MissingRequest(frame) => {
            validate_common(
                frame.sync_version,
                &frame.session_id,
                &frame.sender_wayfarer_id,
                frame.page,
                frame.has_more,
            )?;
            if !is_valid_ascii_id(&frame.request_id) {
                return Err("invalid missing_request.request_id format".to_string());
            }
            if frame.in_response_to_page != 1 {
                return Err("missing_request.in_response_to_page must be 1 for MVP0".to_string());
            }
            if frame.max_transfer_items == 0 {
                return Err("missing_request.max_transfer_items must be positive".to_string());
            }
            for item_id in &frame.missing_item_ids {
                if !is_valid_item_id(item_id) {
                    return Err("invalid missing_request.missing_item_ids item format".to_string());
                }
            }
        }
        GossipSyncFrame::Transfer(frame) => {
            validate_common(
                frame.sync_version,
                &frame.session_id,
                &frame.sender_wayfarer_id,
                frame.page,
                frame.has_more,
            )?;
            if !is_valid_ascii_id(&frame.transfer_id) {
                return Err("invalid transfer.transfer_id format".to_string());
            }
            if !is_valid_ascii_id(&frame.in_response_to_request_id) {
                return Err("invalid transfer.in_response_to_request_id format".to_string());
            }
            for item in &frame.items {
                validate_transfer_item(item)?;
            }
        }
        GossipSyncFrame::Receipt(frame) => {
            validate_common(
                frame.sync_version,
                &frame.session_id,
                &frame.sender_wayfarer_id,
                frame.page,
                frame.has_more,
            )?;
            if !is_valid_ascii_id(&frame.receipt_id) {
                return Err("invalid receipt.receipt_id format".to_string());
            }
            if !is_valid_ascii_id(&frame.in_response_to_transfer_id) {
                return Err("invalid receipt.in_response_to_transfer_id format".to_string());
            }
            if !(frame.status == "accepted"
                || frame.status == "partial"
                || frame.status == "rejected")
            {
                return Err("invalid receipt.status value".to_string());
            }
            let mut ids = BTreeSet::new();
            for item_id in &frame.accepted_item_ids {
                if !is_valid_item_id(item_id) {
                    return Err("invalid receipt.accepted_item_ids item format".to_string());
                }
                if !ids.insert(item_id.as_str()) {
                    return Err("receipt.accepted_item_ids contains duplicates".to_string());
                }
            }
        }
    }
    Ok(())
}

pub fn record_local_payload(payload_b64: &str, expires_at_unix_ms: u64) -> Result<String, String> {
    let now = now_unix_ms();
    if expires_at_unix_ms <= now {
        return Err("cannot store expired payload for gossip".to_string());
    }

    let mut store = load_store()?;
    let item = stored_item_from_payload(payload_b64, expires_at_unix_ms, now)?;
    let item_id = item.item_id.clone();
    store.items.insert(item.item_id.clone(), item);
    prune_expired(&mut store, now);
    save_store(&store)?;
    Ok(item_id)
}

pub fn has_item(item_id: &str) -> Result<bool, String> {
    let store = load_store()?;
    Ok(store.items.contains_key(item_id))
}

pub fn inventory_entries(now_ms: u64) -> Result<Vec<InventoryEntry>, String> {
    let mut store = load_store()?;
    prune_expired(&mut store, now_ms);
    save_store(&store)?;

    let mut entries = store
        .items
        .values()
        .filter(|item| item.expires_at_unix_ms > now_ms)
        .map(|item| InventoryEntry {
            item_id: item.item_id.clone(),
            manifest_id: item.manifest_id.clone(),
            to_wayfarer_id: item.to_wayfarer_id.clone(),
            expires_at_unix_ms: item.expires_at_unix_ms,
            total_size_bytes: item.total_size_bytes,
            chunk_size_bytes: item.chunk_size_bytes,
            chunk_count: item.chunk_count,
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.item_id.cmp(&b.item_id));
    Ok(entries)
}

pub fn transfer_items_for_request(
    requested_item_ids: &[String],
    max_items: u32,
    max_bytes: u64,
    now_ms: u64,
) -> Result<Vec<TransferItem>, String> {
    let store = load_store()?;
    let mut out = Vec::new();
    let mut used_bytes = 0u64;

    for item_id in requested_item_ids {
        if out.len() >= max_items as usize {
            break;
        }
        let Some(item) = store.items.get(item_id) else {
            continue;
        };
        if item.expires_at_unix_ms <= now_ms {
            continue;
        }
        let projected = used_bytes.saturating_add(item.total_size_bytes);
        if projected > max_bytes {
            break;
        }
        used_bytes = projected;
        out.push(TransferItem {
            item_id: item.item_id.clone(),
            manifest_id: item.manifest_id.clone(),
            to_wayfarer_id: item.to_wayfarer_id.clone(),
            expires_at_unix_ms: item.expires_at_unix_ms,
            total_size_bytes: item.total_size_bytes,
            chunk_size_bytes: item.chunk_size_bytes,
            chunk_count: item.chunk_count,
            envelope_b64: item.envelope_b64.clone(),
        });
    }

    Ok(out)
}

pub fn import_transfer_items(
    sender_wayfarer_id: &str,
    local_wayfarer_id: &str,
    items: &[TransferItem],
    now_ms: u64,
) -> Result<ImportTransferResult, String> {
    let mut store = load_store()?;
    let mut accepted_item_ids = Vec::new();
    let mut rejected_items = Vec::new();
    let mut new_messages = Vec::new();

    for item in items {
        let decoded = match validate_transfer_item(item)
            .and_then(|_| decode_envelope_payload_b64(&item.envelope_b64))
        {
            Ok(decoded) => decoded,
            Err(err) => {
                rejected_items.push(RejectedItem {
                    item_id: item.item_id.clone(),
                    code: "MALFORMED_ITEM".to_string(),
                    message: err,
                });
                continue;
            }
        };

        if decoded.to_wayfarer_id_hex != item.to_wayfarer_id {
            rejected_items.push(RejectedItem {
                item_id: item.item_id.clone(),
                code: "TO_WAYFARER_MISMATCH".to_string(),
                message: "transfer to_wayfarer_id does not match envelope".to_string(),
            });
            continue;
        }

        if item.to_wayfarer_id != local_wayfarer_id {
            rejected_items.push(RejectedItem {
                item_id: item.item_id.clone(),
                code: "NOT_FOR_LOCAL_ID".to_string(),
                message: "transfer item is not addressed to this wayfarer".to_string(),
            });
            continue;
        }

        if item.expires_at_unix_ms <= now_ms {
            rejected_items.push(RejectedItem {
                item_id: item.item_id.clone(),
                code: "ITEM_EXPIRED".to_string(),
                message: "transfer item is expired".to_string(),
            });
            continue;
        }

        let stored = StoredItem {
            item_id: item.item_id.clone(),
            manifest_id: item.manifest_id.clone(),
            to_wayfarer_id: item.to_wayfarer_id.clone(),
            expires_at_unix_ms: item.expires_at_unix_ms,
            total_size_bytes: item.total_size_bytes,
            chunk_size_bytes: item.chunk_size_bytes,
            chunk_count: item.chunk_count,
            envelope_b64: item.envelope_b64.clone(),
            recorded_at_unix_ms: now_ms,
        };

        match store.items.get(&item.item_id) {
            Some(existing) if existing.envelope_b64 != item.envelope_b64 => {
                rejected_items.push(RejectedItem {
                    item_id: item.item_id.clone(),
                    code: "ITEM_ID_MISMATCH".to_string(),
                    message: "existing item_id maps to different envelope bytes".to_string(),
                });
            }
            Some(_) => {
                accepted_item_ids.push(item.item_id.clone());
            }
            None => {
                store.items.insert(item.item_id.clone(), stored);
                accepted_item_ids.push(item.item_id.clone());
                let text = decode_envelope_payload_utf8_preview(&item.envelope_b64)
                    .unwrap_or_else(|_| "[binary payload]".to_string());
                new_messages.push(ImportedEnvelope {
                    item_id: item.item_id.clone(),
                    from_wayfarer_id: sender_wayfarer_id.to_string(),
                    text,
                    received_at_unix: (now_ms / 1000) as i64,
                    manifest_id_hex: Some(item.manifest_id.clone()),
                });
            }
        }
    }

    prune_expired(&mut store, now_ms);
    save_store(&store)?;

    Ok(ImportTransferResult {
        accepted_item_ids,
        rejected_items,
        new_messages,
    })
}

fn validate_common(
    sync_version: u8,
    session_id: &str,
    sender_wayfarer_id: &str,
    page: u32,
    has_more: bool,
) -> Result<(), String> {
    if sync_version != GOSSIP_SYNC_VERSION {
        return Err("unsupported sync_version".to_string());
    }
    if !is_valid_ascii_id(session_id) {
        return Err("invalid session_id format".to_string());
    }
    if !is_valid_wayfarer_id(sender_wayfarer_id) {
        return Err("invalid sender_wayfarer_id format".to_string());
    }
    if page != 1 || has_more {
        return Err("MVP0 gossip profile requires page=1 and has_more=false".to_string());
    }
    Ok(())
}

fn validate_inventory_entry(entry: &InventoryEntry) -> Result<(), String> {
    if !is_valid_item_id(&entry.item_id) {
        return Err("invalid inventory item_id format".to_string());
    }
    if !is_valid_item_id(&entry.manifest_id) {
        return Err("invalid inventory manifest_id format".to_string());
    }
    if !is_valid_wayfarer_id(&entry.to_wayfarer_id) {
        return Err("invalid inventory to_wayfarer_id format".to_string());
    }
    if entry.chunk_size_bytes != GOSSIP_CHUNK_SIZE_BYTES {
        return Err("inventory chunk_size_bytes must be 32768".to_string());
    }
    Ok(())
}

fn validate_transfer_item(item: &TransferItem) -> Result<(), String> {
    if !is_valid_item_id(&item.item_id) {
        return Err("invalid transfer item_id format".to_string());
    }
    if !is_valid_item_id(&item.manifest_id) {
        return Err("invalid transfer manifest_id format".to_string());
    }
    if !is_valid_wayfarer_id(&item.to_wayfarer_id) {
        return Err("invalid transfer to_wayfarer_id format".to_string());
    }
    if item.chunk_size_bytes != GOSSIP_CHUNK_SIZE_BYTES {
        return Err("transfer chunk_size_bytes must be 32768".to_string());
    }
    if !is_valid_payload_b64(&item.envelope_b64) {
        return Err("invalid transfer envelope_b64 format".to_string());
    }

    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&item.envelope_b64)
        .map_err(|err| format!("transfer envelope_b64 decode failed: {err}"))?;

    if item_id_from_envelope_bytes(&raw) != item.item_id {
        return Err("transfer item_id mismatch with envelope bytes".to_string());
    }

    let decoded = decode_envelope_payload_b64(&item.envelope_b64)?;
    if decoded.manifest_id_hex != item.manifest_id {
        return Err("transfer manifest_id mismatch with envelope bytes".to_string());
    }

    let total_size = decoded.body.len() as u64;
    if item.total_size_bytes != total_size {
        return Err("transfer total_size_bytes mismatch with envelope body size".to_string());
    }

    let expected_chunks = u32::max(1, total_size.div_ceil(GOSSIP_CHUNK_SIZE_BYTES) as u32);
    if item.chunk_count != expected_chunks {
        return Err("transfer chunk_count mismatch with envelope body size".to_string());
    }

    Ok(())
}

fn stored_item_from_payload(
    payload_b64: &str,
    expires_at_unix_ms: u64,
    recorded_at_unix_ms: u64,
) -> Result<StoredItem, String> {
    if !is_valid_payload_b64(payload_b64) {
        return Err("invalid payload_b64 format for gossip storage".to_string());
    }
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|err| format!("payload_b64 decode failed for gossip storage: {err}"))?;
    let decoded = decode_envelope_payload_b64(payload_b64)?;

    let total_size_bytes = decoded.body.len() as u64;
    let chunk_count = u32::max(1, total_size_bytes.div_ceil(GOSSIP_CHUNK_SIZE_BYTES) as u32);

    Ok(StoredItem {
        item_id: item_id_from_envelope_bytes(&raw),
        manifest_id: decoded.manifest_id_hex,
        to_wayfarer_id: decoded.to_wayfarer_id_hex,
        expires_at_unix_ms,
        total_size_bytes,
        chunk_size_bytes: GOSSIP_CHUNK_SIZE_BYTES,
        chunk_count,
        envelope_b64: payload_b64.to_string(),
        recorded_at_unix_ms,
    })
}

fn item_id_from_envelope_bytes(raw: &[u8]) -> String {
    let digest = Sha256::digest(raw);
    bytes_to_hex_lower(&digest)
}

fn prune_expired(store: &mut GossipStore, now_ms: u64) {
    store
        .items
        .retain(|_, item| item.expires_at_unix_ms > now_ms && !item.envelope_b64.is_empty());
}

fn load_store() -> Result<GossipStore, String> {
    let path = gossip_store_path();
    if !path.exists() {
        return Ok(GossipStore::default());
    }

    let content = fs::read_to_string(&path)
        .map_err(|err| format!("failed to read gossip store at {}: {err}", path.display()))?;
    serde_json::from_str(&content)
        .map_err(|err| format!("failed to parse gossip store at {}: {err}", path.display()))
}

fn save_store(store: &GossipStore) -> Result<(), String> {
    let path = gossip_store_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed creating gossip store directory: {err}"))?;
    }
    let serialized = serde_json::to_string_pretty(store)
        .map_err(|err| format!("failed serializing gossip store: {err}"))?;
    fs::write(&path, serialized)
        .map_err(|err| format!("failed writing gossip store at {}: {err}", path.display()))
}

fn gossip_store_path() -> PathBuf {
    if let Ok(xdg_state_home) = std::env::var("XDG_STATE_HOME") {
        if !xdg_state_home.trim().is_empty() {
            return Path::new(&xdg_state_home)
                .join("aethos-linux")
                .join(GOSSIP_STORE_FILE_NAME);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        return Path::new(&home)
            .join(".local")
            .join("state")
            .join("aethos-linux")
            .join(GOSSIP_STORE_FILE_NAME);
    }

    std::env::temp_dir().join(GOSSIP_STORE_FILE_NAME)
}

fn now_unix_ms() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(_) => 0,
    }
}

fn is_valid_ascii_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

fn is_valid_item_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn bytes_to_hex_lower(input: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(input.len() * 2);
    for byte in input {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        parse_frame, serialize_frame, validate_frame, GossipSyncFrame, InventoryEntry,
        InventorySummaryFrame, MissingRequestFrame, TransferFrame, TransferItem,
        GOSSIP_CHUNK_SIZE_BYTES, GOSSIP_SYNC_VERSION,
    };
    use crate::aethos_core::protocol::build_envelope_payload_b64_from_utf8;
    use base64::Engine;
    use sha2::{Digest, Sha256};

    #[test]
    fn parses_roundtrip_inventory_frame() {
        let frame = GossipSyncFrame::InventorySummary(InventorySummaryFrame {
            sync_version: GOSSIP_SYNC_VERSION,
            session_id: "sess-1".to_string(),
            sender_wayfarer_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            page: 1,
            has_more: false,
            inventory: Vec::new(),
        });

        let raw = serialize_frame(&frame).expect("serialize gossip frame");
        let parsed = parse_frame(&raw).expect("parse gossip frame");
        assert!(matches!(parsed, GossipSyncFrame::InventorySummary(_)));
    }

    #[test]
    fn rejects_multipage_frame_in_mvp0_profile() {
        let frame = GossipSyncFrame::InventorySummary(InventorySummaryFrame {
            sync_version: GOSSIP_SYNC_VERSION,
            session_id: "sess-2".to_string(),
            sender_wayfarer_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            page: 2,
            has_more: false,
            inventory: Vec::new(),
        });

        assert!(validate_frame(&frame).is_err());
    }

    #[test]
    fn validates_transfer_item_matches_envelope_hash() {
        let payload = build_envelope_payload_b64_from_utf8(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "hello gossip",
        )
        .expect("build payload");
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&payload)
            .expect("decode payload");
        let digest = Sha256::digest(raw);
        let item_id = digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let manifest = Sha256::digest("hello gossip".as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();

        let frame = GossipSyncFrame::Transfer(TransferFrame {
            sync_version: GOSSIP_SYNC_VERSION,
            session_id: "sess-3".to_string(),
            sender_wayfarer_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            page: 1,
            has_more: false,
            transfer_id: "tx-1".to_string(),
            in_response_to_request_id: "req-1".to_string(),
            items: vec![TransferItem {
                item_id,
                manifest_id: manifest,
                to_wayfarer_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    .to_string(),
                expires_at_unix_ms: 4_102_444_800_000,
                total_size_bytes: "hello gossip".len() as u64,
                chunk_size_bytes: GOSSIP_CHUNK_SIZE_BYTES,
                chunk_count: 1,
                envelope_b64: payload,
            }],
        });

        assert!(validate_frame(&frame).is_ok());
    }

    #[test]
    fn accepts_empty_missing_request_vector() {
        let frame = GossipSyncFrame::MissingRequest(MissingRequestFrame {
            sync_version: GOSSIP_SYNC_VERSION,
            session_id: "sess-4".to_string(),
            sender_wayfarer_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_string(),
            page: 1,
            has_more: false,
            request_id: "req-empty".to_string(),
            in_response_to_page: 1,
            missing_item_ids: Vec::new(),
            max_transfer_items: 64,
            max_transfer_bytes: 2_097_152,
        });

        assert!(validate_frame(&frame).is_ok());
    }

    #[test]
    fn validates_inventory_entry_shape() {
        let frame = GossipSyncFrame::InventorySummary(InventorySummaryFrame {
            sync_version: GOSSIP_SYNC_VERSION,
            session_id: "sess-5".to_string(),
            sender_wayfarer_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            page: 1,
            has_more: false,
            inventory: vec![InventoryEntry {
                item_id: "c".repeat(64),
                manifest_id: "d".repeat(64),
                to_wayfarer_id: "b".repeat(64),
                expires_at_unix_ms: 4_102_444_800_000,
                total_size_bytes: 12,
                chunk_size_bytes: GOSSIP_CHUNK_SIZE_BYTES,
                chunk_count: 1,
            }],
        });

        assert!(validate_frame(&frame).is_ok());
    }
}
