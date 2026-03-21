use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use ciborium::value::Value;
use ciborium::{de::from_reader, ser::into_writer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::aethos_core::logging::log_verbose;
use crate::aethos_core::protocol::{
    bytes_to_hex_lower, decode_cbor_value_exact, decode_envelope_payload_b64,
    decode_envelope_payload_utf8_preview, encode_cbor_value_deterministic, is_valid_payload_b64,
    to_cbor_value,
};

pub const GOSSIP_VERSION: u64 = 1;
pub const GOSSIP_LAN_PORT: u16 = 47_655;
pub const MAX_FRAME_BYTES: usize = 1_048_576;
pub const MAX_WANT_ITEMS: usize = 256;
pub const MAX_TRANSFER_ITEMS: usize = 32;
pub const MAX_TRANSFER_BYTES: u64 = 524_288;
pub const BLOOM_FILTER_BYTES: usize = 2048;
pub const BLOOM_HASH_COUNT: u8 = 4;
pub const CLOCK_SKEW_TOLERANCE_MS: u64 = 30_000;
pub const MAX_SUMMARY_PREVIEW_ITEMS: usize = 64;

const GOSSIP_STORE_FILE_NAME: &str = "gossip-object-store.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum GossipSyncFrame {
    #[serde(rename = "HELLO")]
    Hello(HelloFrame),
    #[serde(rename = "SUMMARY")]
    Summary(SummaryFrame),
    #[serde(rename = "REQUEST")]
    Request(RequestFrame),
    #[serde(rename = "TRANSFER")]
    Transfer(TransferFrame),
    #[serde(rename = "RECEIPT")]
    Receipt(ReceiptFrame),
    #[serde(rename = "RELAY_INGEST")]
    RelayIngest(RelayIngestFrame),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloFrame {
    pub version: u64,
    pub node_id: String,
    pub node_pubkey: String,
    pub capabilities: Vec<String>,
    pub propagation_class: String,
    pub max_want: u64,
    pub max_transfer: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryFrame {
    #[serde(with = "serde_bytes")]
    pub bloom_filter: Vec<u8>,
    pub item_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_item_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestFrame {
    pub want: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferFrame {
    pub objects: Vec<TransferObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferObject {
    pub item_id: String,
    pub envelope_b64: String,
    pub expiry_unix_ms: u64,
    pub hop_count: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptFrame {
    pub received: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayIngestFrame {
    pub item_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ImportedEnvelope {
    pub item_id: String,
    pub author_wayfarer_id: Option<String>,
    pub transport_peer: Option<String>,
    pub session_peer: Option<String>,
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
#[serde(deny_unknown_fields)]
pub struct RejectedItem {
    pub item_id: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GossipStore {
    items: BTreeMap<String, StoredItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredItem {
    item_id: String,
    envelope_b64: String,
    expiry_unix_ms: u64,
    hop_count: u16,
    recorded_at_unix_ms: u64,
}

pub fn serialize_frame(frame: &GossipSyncFrame) -> Result<Vec<u8>, String> {
    let (frame_type, payload) = frame_type_and_payload(frame)?;
    let envelope = Value::Map(vec![
        (
            Value::Text("type".to_string()),
            Value::Text(frame_type.to_string()),
        ),
        (Value::Text("payload".to_string()), payload),
    ]);
    let raw = encode_cbor_value_deterministic(&envelope)
        .map_err(|err| format!("serialize gossip frame: {err}"))?;
    if raw.len() > MAX_FRAME_BYTES {
        return Err("frame exceeds MAX_FRAME_BYTES".to_string());
    }
    Ok(raw)
}

pub fn parse_frame(raw: &[u8]) -> Result<GossipSyncFrame, String> {
    if raw.len() > MAX_FRAME_BYTES {
        return Err("frame exceeds MAX_FRAME_BYTES".to_string());
    }

    let envelope = decode_cbor_value_exact(raw, "gossip frame")
        .map_err(|err| classify_frame_parse_error(&err))?;
    let canonical = encode_cbor_value_deterministic(&envelope).map_err(|err| {
        classify_frame_parse_error(&format!("canonical frame encode failed: {err}"))
    })?;
    if canonical.as_slice() != raw {
        return Err(
            "parse gossip frame cbor: frame is not deterministic canonical CBOR".to_string(),
        );
    }

    let frame =
        frame_from_envelope_value(envelope).map_err(|err| classify_frame_parse_error(&err))?;
    validate_frame(&frame)?;
    Ok(frame)
}

fn classify_frame_parse_error(parse_error: &str) -> String {
    if parse_error.contains("unknown field `accepted`") && parse_error.contains("received") {
        return "protocol violation: RECEIPT payload used non-spec field `accepted`; expected `received`"
            .to_string();
    }

    format!("parse gossip frame cbor: {parse_error}")
}

pub fn validate_frame(frame: &GossipSyncFrame) -> Result<(), String> {
    match frame {
        GossipSyncFrame::Hello(hello) => validate_hello(hello),
        GossipSyncFrame::Summary(summary) => validate_summary(summary),
        GossipSyncFrame::Request(request) => {
            if request.want.len() > MAX_WANT_ITEMS {
                return Err("REQUEST want exceeds MAX_WANT_ITEMS".to_string());
            }
            validate_sorted_unique_item_ids(&request.want, "REQUEST.want")
        }
        GossipSyncFrame::Transfer(transfer) => validate_transfer(transfer),
        GossipSyncFrame::Receipt(receipt) => {
            validate_unique_item_ids(&receipt.received, "RECEIPT.received")
        }
        GossipSyncFrame::RelayIngest(ingest) => {
            validate_unique_item_ids(&ingest.item_ids, "RELAY_INGEST.item_ids")
        }
    }
}

pub fn build_hello_frame(
    node_id: &str,
    node_pubkey_b64url: &str,
) -> Result<GossipSyncFrame, String> {
    let frame = GossipSyncFrame::Hello(HelloFrame {
        version: GOSSIP_VERSION,
        node_id: node_id.to_string(),
        node_pubkey: node_pubkey_b64url.to_string(),
        capabilities: vec!["relay_ingest".to_string()],
        propagation_class: "interactive".to_string(),
        max_want: MAX_WANT_ITEMS as u64,
        max_transfer: MAX_TRANSFER_ITEMS as u64,
    });
    validate_frame(&frame)?;
    Ok(frame)
}

pub fn build_summary_frame(now_ms: u64) -> Result<GossipSyncFrame, String> {
    let item_ids = eligible_item_ids(now_ms)?;
    let bloom_filter = build_bloom_filter(&item_ids)?;
    let preview_item_ids = build_summary_preview_item_ids(now_ms)?;
    let preview_cursor = preview_item_ids.last().cloned();
    let frame = GossipSyncFrame::Summary(SummaryFrame {
        bloom_filter,
        item_count: item_ids.len() as u64,
        preview_item_ids: (!preview_item_ids.is_empty()).then_some(preview_item_ids),
        preview_cursor,
    });
    validate_frame(&frame)?;
    Ok(frame)
}

#[allow(dead_code)]
pub fn select_request_item_ids_from_summary(
    summary: &SummaryFrame,
    max_want: usize,
) -> Result<Vec<String>, String> {
    select_request_item_ids_from_summary_with_candidates(summary, max_want, &[])
}

pub fn select_request_item_ids_from_summary_with_candidates(
    summary: &SummaryFrame,
    max_want: usize,
    candidate_item_ids: &[String],
) -> Result<Vec<String>, String> {
    let local_have = eligible_item_ids(now_unix_ms())?;
    select_request_item_ids_from_summary_with_context(
        summary,
        max_want,
        candidate_item_ids,
        &local_have,
    )
}

fn select_request_item_ids_from_summary_with_context(
    summary: &SummaryFrame,
    max_want: usize,
    candidate_item_ids: &[String],
    local_have_item_ids: &[String],
) -> Result<Vec<String>, String> {
    let request_cap = max_want.min(MAX_WANT_ITEMS);
    if request_cap == 0 {
        return Ok(Vec::new());
    }

    let local_have = local_have_item_ids.iter().cloned().collect::<BTreeSet<_>>();
    let mut preview_eligible = Vec::new();
    let mut preview_seen = BTreeSet::new();
    for item_id in summary.preview_item_ids.as_deref().unwrap_or(&[]) {
        if local_have.contains(item_id) {
            continue;
        }
        if !preview_seen.insert(item_id.clone()) {
            continue;
        }
        if bloom_might_contain(&summary.bloom_filter, item_id)? {
            preview_eligible.push(item_id.clone());
        }
    }

    let mut candidate_eligible = Vec::new();
    let mut candidate_seen = BTreeSet::new();
    for item_id in candidate_item_ids {
        if local_have.contains(item_id) {
            continue;
        }
        if !candidate_seen.insert(item_id.clone()) {
            continue;
        }
        if bloom_might_contain(&summary.bloom_filter, item_id)? {
            candidate_eligible.push(item_id.clone());
        }
    }

    let mut selected = Vec::new();
    let mut selected_seen = BTreeSet::new();
    for item_id in preview_eligible
        .into_iter()
        .chain(candidate_eligible.into_iter())
    {
        if selected.len() >= request_cap {
            break;
        }
        if selected_seen.insert(item_id.clone()) {
            selected.push(item_id);
        }
    }

    selected.sort_by_key(|item_id| decode_item_id(item_id).unwrap_or_default());
    Ok(selected)
}

pub fn build_relay_ingest_frame(now_ms: u64) -> Result<GossipSyncFrame, String> {
    let frame = GossipSyncFrame::RelayIngest(RelayIngestFrame {
        item_ids: eligible_item_ids(now_ms)?,
    });
    validate_frame(&frame)?;
    Ok(frame)
}

pub fn build_request_frame(
    mut want: Vec<String>,
    max_want: usize,
) -> Result<GossipSyncFrame, String> {
    want.retain(|item_id| is_valid_item_id(item_id));
    want.sort_by_key(|item_id| decode_item_id(item_id).unwrap_or_default());
    want.dedup();
    want.truncate(max_want.min(MAX_WANT_ITEMS));
    let frame = GossipSyncFrame::Request(RequestFrame { want });
    validate_frame(&frame)?;
    Ok(frame)
}

pub fn has_item(item_id: &str) -> Result<bool, String> {
    let store = load_store()?;
    Ok(store.items.contains_key(item_id))
}

pub fn eligible_item_ids(now_ms: u64) -> Result<Vec<String>, String> {
    let mut store = load_store()?;
    prune_expired(&mut store, now_ms);
    save_store(&store)?;
    Ok(store.items.keys().cloned().collect())
}

pub fn record_local_payload(payload_b64: &str, expiry_unix_ms: u64) -> Result<String, String> {
    if !is_valid_payload_b64(payload_b64) {
        return Err("invalid payload_b64 format for gossip storage".to_string());
    }
    let now = now_unix_ms();
    if now + CLOCK_SKEW_TOLERANCE_MS >= expiry_unix_ms {
        return Err("cannot store already-expired object".to_string());
    }

    let mut store = load_store()?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|err| format!("payload decode failed: {err}"))?;
    let item_id = item_id_from_envelope_bytes(&raw);
    log_verbose(&format!(
        "object_store_put: item_id={} expiry_unix_ms={} payload_bytes={}",
        item_id,
        expiry_unix_ms,
        raw.len()
    ));

    let item = StoredItem {
        item_id: item_id.clone(),
        envelope_b64: payload_b64.to_string(),
        expiry_unix_ms,
        hop_count: 0,
        recorded_at_unix_ms: now,
    };
    store.items.insert(item_id.clone(), item);
    prune_expired(&mut store, now);
    save_store(&store)?;
    Ok(item_id)
}

pub fn transfer_items_for_request(
    requested_item_ids: &[String],
    max_items: u32,
    max_bytes: u64,
    now_ms: u64,
) -> Result<Vec<TransferObject>, String> {
    log_verbose(&format!(
        "transfer_select_start: requested={} max_items={} max_bytes={} now_ms={}",
        requested_item_ids.len(),
        max_items,
        max_bytes,
        now_ms
    ));
    let mut selected = Vec::new();
    let mut consumed_bytes = 0u64;
    let store = load_store()?;

    for item_id in requested_item_ids {
        if selected.len() >= max_items as usize || selected.len() >= MAX_TRANSFER_ITEMS {
            break;
        }

        let Some(stored) = store.items.get(item_id) else {
            continue;
        };
        if now_ms + CLOCK_SKEW_TOLERANCE_MS >= stored.expiry_unix_ms {
            continue;
        }

        let envelope_raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&stored.envelope_b64)
            .map_err(|err| format!("stored envelope decode failed: {err}"))?;
        let projected = consumed_bytes.saturating_add(envelope_raw.len() as u64);
        if projected > max_bytes.min(MAX_TRANSFER_BYTES) {
            break;
        }

        consumed_bytes = projected;
        selected.push(TransferObject {
            item_id: stored.item_id.clone(),
            envelope_b64: stored.envelope_b64.clone(),
            expiry_unix_ms: stored.expiry_unix_ms,
            hop_count: stored.hop_count.saturating_add(1),
        });
    }

    log_verbose(&format!(
        "transfer_select_done: selected_items={} consumed_bytes={}",
        selected.len(),
        consumed_bytes
    ));

    Ok(selected)
}

pub fn import_transfer_items(
    local_wayfarer_id: &str,
    transport_peer: Option<&str>,
    session_peer_wayfarer_id: Option<&str>,
    objects: &[TransferObject],
    now_ms: u64,
) -> Result<ImportTransferResult, String> {
    log_verbose(&format!(
        "transfer_import_start: local={} transport_peer={} session_peer={} objects={} now_ms={}",
        local_wayfarer_id,
        transport_peer.unwrap_or("none"),
        session_peer_wayfarer_id.unwrap_or("none"),
        objects.len(),
        now_ms
    ));
    let mut store = load_store()?;
    let mut accepted_item_ids = Vec::new();
    let mut rejected_items = Vec::new();
    let mut new_messages = Vec::new();

    for object in objects {
        let parsed = match validate_transfer_object(object)
            .and_then(|_| decode_envelope_payload_b64(&object.envelope_b64))
        {
            Ok(decoded) => decoded,
            Err(err) => {
                rejected_items.push(RejectedItem {
                    item_id: object.item_id.clone(),
                    code: "MALFORMED_OBJECT".to_string(),
                    message: err,
                });
                continue;
            }
        };

        if now_ms + CLOCK_SKEW_TOLERANCE_MS >= object.expiry_unix_ms {
            rejected_items.push(RejectedItem {
                item_id: object.item_id.clone(),
                code: "EXPIRED".to_string(),
                message: "object is expired by local clock".to_string(),
            });
            continue;
        }

        let incoming = StoredItem {
            item_id: object.item_id.clone(),
            envelope_b64: object.envelope_b64.clone(),
            expiry_unix_ms: object.expiry_unix_ms,
            hop_count: object.hop_count,
            recorded_at_unix_ms: now_ms,
        };

        match store.items.get(&object.item_id) {
            Some(existing) if existing.envelope_b64 != incoming.envelope_b64 => {
                rejected_items.push(RejectedItem {
                    item_id: object.item_id.clone(),
                    code: "ITEM_ID_MISMATCH".to_string(),
                    message: "existing item_id maps to different envelope bytes".to_string(),
                });
            }
            Some(existing) if object.hop_count < existing.hop_count => {
                rejected_items.push(RejectedItem {
                    item_id: object.item_id.clone(),
                    code: "HOP_REGRESSION".to_string(),
                    message: "incoming hop_count regressed for known item".to_string(),
                });
            }
            Some(_) => {
                accepted_item_ids.push(object.item_id.clone());
            }
            None => {
                store.items.insert(object.item_id.clone(), incoming);
                accepted_item_ids.push(object.item_id.clone());
                if parsed.to_wayfarer_id_hex == local_wayfarer_id {
                    match decode_envelope_payload_utf8_preview(&object.envelope_b64) {
                        Ok(text) => {
                            new_messages.push(ImportedEnvelope {
                                item_id: object.item_id.clone(),
                                author_wayfarer_id: Some(parsed.author_wayfarer_id_hex.clone()),
                                transport_peer: transport_peer.map(|value| value.to_string()),
                                session_peer: session_peer_wayfarer_id
                                    .map(|value| value.to_string()),
                                text,
                                received_at_unix: (now_ms / 1000) as i64,
                                manifest_id_hex: Some(parsed.manifest_id_hex),
                            });
                        }
                        Err(_) => {
                            log_verbose(&format!(
                                "transfer_import_skip_non_utf8_payload: item_id={} transport_peer={} session_peer={}",
                                object.item_id,
                                transport_peer.unwrap_or("none"),
                                session_peer_wayfarer_id.unwrap_or("none")
                            ));
                        }
                    }
                } else {
                    log_verbose(&format!(
                        "transfer_import_stored_nonlocal: item_id={} to={} local={}",
                        object.item_id, parsed.to_wayfarer_id_hex, local_wayfarer_id
                    ));
                }
            }
        }
    }

    prune_expired(&mut store, now_ms);
    save_store(&store)?;
    log_verbose(&format!(
        "transfer_import_done: accepted={} rejected={} new_messages={}",
        accepted_item_ids.len(),
        rejected_items.len(),
        new_messages.len()
    ));
    Ok(ImportTransferResult {
        accepted_item_ids,
        rejected_items,
        new_messages,
    })
}

pub fn build_bloom_filter(item_ids: &[String]) -> Result<Vec<u8>, String> {
    let mut bloom = vec![0u8; BLOOM_FILTER_BYTES];
    for item_id in item_ids {
        let item_bytes = decode_item_id(item_id)?;
        for hash_idx in 0..BLOOM_HASH_COUNT {
            let mut hasher = Sha256::new();
            hasher.update(&item_bytes);
            hasher.update([hash_idx]);
            let digest = hasher.finalize();
            let mut n = [0u8; 8];
            n.copy_from_slice(&digest[..8]);
            let bit_index = (u64::from_be_bytes(n) % (BLOOM_FILTER_BYTES as u64 * 8)) as usize;
            let byte_index = bit_index / 8;
            let bit_offset = bit_index % 8;
            bloom[byte_index] |= 1 << bit_offset;
        }
    }
    Ok(bloom)
}

pub fn bloom_might_contain(bloom_filter: &[u8], item_id: &str) -> Result<bool, String> {
    if bloom_filter.len() != BLOOM_FILTER_BYTES {
        return Err("invalid bloom filter length".to_string());
    }
    let item_bytes = decode_item_id(item_id)?;
    for hash_idx in 0..BLOOM_HASH_COUNT {
        let mut hasher = Sha256::new();
        hasher.update(&item_bytes);
        hasher.update([hash_idx]);
        let digest = hasher.finalize();
        let mut n = [0u8; 8];
        n.copy_from_slice(&digest[..8]);
        let bit_index = (u64::from_be_bytes(n) % (BLOOM_FILTER_BYTES as u64 * 8)) as usize;
        let byte_index = bit_index / 8;
        let bit_offset = bit_index % 8;
        if bloom_filter[byte_index] & (1 << bit_offset) == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn validate_hello(hello: &HelloFrame) -> Result<(), String> {
    if hello.version != GOSSIP_VERSION {
        return Err("HELLO version mismatch".to_string());
    }
    if !is_valid_item_id(&hello.node_id) {
        return Err("HELLO node_id format invalid".to_string());
    }
    let pubkey = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&hello.node_pubkey)
        .map_err(|err| format!("HELLO node_pubkey decode failed: {err}"))?;
    let derived = bytes_to_hex_lower(&Sha256::digest(pubkey));
    if derived != hello.node_id {
        return Err("HELLO node_id does not match SHA-256(node_pubkey)".to_string());
    }
    if hello.max_want == 0 || hello.max_want > MAX_WANT_ITEMS as u64 {
        return Err("HELLO max_want out of range".to_string());
    }
    if hello.max_transfer == 0 || hello.max_transfer > MAX_TRANSFER_ITEMS as u64 {
        return Err("HELLO max_transfer out of range".to_string());
    }
    Ok(())
}

fn validate_summary(summary: &SummaryFrame) -> Result<(), String> {
    if summary.bloom_filter.len() != BLOOM_FILTER_BYTES {
        return Err("SUMMARY bloom_filter length mismatch".to_string());
    }

    if let Some(preview_item_ids) = &summary.preview_item_ids {
        if preview_item_ids.len() > MAX_SUMMARY_PREVIEW_ITEMS {
            return Err("SUMMARY preview_item_ids exceeds MAX_SUMMARY_PREVIEW_ITEMS".to_string());
        }
        validate_sorted_unique_item_ids(preview_item_ids, "SUMMARY.preview_item_ids")?;
    }

    if let Some(preview_cursor) = &summary.preview_cursor {
        if !is_valid_item_id(preview_cursor) {
            return Err("SUMMARY preview_cursor format invalid".to_string());
        }
        let preview_item_ids = summary.preview_item_ids.as_ref().ok_or_else(|| {
            "SUMMARY preview_cursor must be absent when preview_item_ids is absent".to_string()
        })?;
        if preview_item_ids.is_empty() {
            return Err(
                "SUMMARY preview_cursor must be absent when preview_item_ids is empty".to_string(),
            );
        }
        if preview_item_ids.last() != Some(preview_cursor) {
            return Err(
                "SUMMARY preview_cursor must equal last preview_item_ids element".to_string(),
            );
        }
    }

    Ok(())
}

fn build_summary_preview_item_ids(now_ms: u64) -> Result<Vec<String>, String> {
    let mut store = load_store()?;
    prune_expired(&mut store, now_ms);
    save_store(&store)?;

    let mut ranked = store
        .items
        .values()
        .map(|item| {
            (
                item.expiry_unix_ms,
                item.hop_count,
                decode_item_id(&item.item_id).unwrap_or_default(),
                item.item_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });

    let mut preview_item_ids = ranked
        .into_iter()
        .take(MAX_SUMMARY_PREVIEW_ITEMS)
        .map(|(_, _, _, item_id)| item_id)
        .collect::<Vec<_>>();
    preview_item_ids.sort_by_key(|item_id| decode_item_id(item_id).unwrap_or_default());
    preview_item_ids.dedup();
    Ok(preview_item_ids)
}

fn validate_transfer(frame: &TransferFrame) -> Result<(), String> {
    if frame.objects.len() > MAX_TRANSFER_ITEMS {
        return Err("TRANSFER objects exceeds MAX_TRANSFER_ITEMS".to_string());
    }
    let mut total_bytes = 0u64;
    for object in &frame.objects {
        let Ok(raw) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&object.envelope_b64)
        else {
            continue;
        };
        total_bytes = total_bytes.saturating_add(raw.len() as u64);
        if total_bytes > MAX_TRANSFER_BYTES {
            return Err("TRANSFER decoded payload exceeds MAX_TRANSFER_BYTES".to_string());
        }
    }
    Ok(())
}

fn validate_transfer_object(object: &TransferObject) -> Result<(), String> {
    if !is_valid_item_id(&object.item_id) {
        return Err("TRANSFER object.item_id format invalid".to_string());
    }
    if !is_valid_payload_b64(&object.envelope_b64) {
        return Err("TRANSFER object.envelope_b64 format invalid".to_string());
    }
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&object.envelope_b64)
        .map_err(|err| format!("TRANSFER object envelope decode failed: {err}"))?;
    let derived = item_id_from_envelope_bytes(&raw);
    if derived != object.item_id {
        return Err("TRANSFER object.item_id mismatch with envelope bytes".to_string());
    }
    Ok(())
}

fn validate_unique_item_ids(item_ids: &[String], label: &str) -> Result<(), String> {
    let mut unique = BTreeSet::new();
    for item_id in item_ids {
        if !is_valid_item_id(item_id) {
            return Err(format!("{label} has malformed item_id"));
        }
        if !unique.insert(item_id.as_str()) {
            return Err(format!("{label} contains duplicate item_id"));
        }
    }
    Ok(())
}

fn validate_sorted_unique_item_ids(item_ids: &[String], label: &str) -> Result<(), String> {
    validate_unique_item_ids(item_ids, label)?;
    let mut prev: Option<Vec<u8>> = None;
    for item_id in item_ids {
        let decoded = decode_item_id(item_id)?;
        if let Some(ref p) = prev {
            if decoded < *p {
                return Err(format!("{label} must be bytewise lexicographically sorted"));
            }
        }
        prev = Some(decoded);
    }
    Ok(())
}

fn decode_item_id(item_id: &str) -> Result<Vec<u8>, String> {
    if !is_valid_item_id(item_id) {
        return Err("invalid item_id format".to_string());
    }
    let mut out = Vec::with_capacity(32);
    for i in (0..64).step_by(2) {
        out.push(
            u8::from_str_radix(&item_id[i..i + 2], 16)
                .map_err(|err| format!("invalid item_id hex: {err}"))?,
        );
    }
    Ok(out)
}

fn item_id_from_envelope_bytes(raw: &[u8]) -> String {
    bytes_to_hex_lower(&Sha256::digest(raw))
}

fn is_valid_item_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn prune_expired(store: &mut GossipStore, now_ms: u64) {
    store.items.retain(|_, item| {
        !item.envelope_b64.is_empty()
            && now_ms + CLOCK_SKEW_TOLERANCE_MS < item.expiry_unix_ms
            && decode_envelope_payload_b64(&item.envelope_b64).is_ok()
    });
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

fn frame_type_and_payload(frame: &GossipSyncFrame) -> Result<(&'static str, Value), String> {
    match frame {
        GossipSyncFrame::Hello(payload) => Ok(("HELLO", to_cbor_value(payload)?)),
        GossipSyncFrame::Summary(payload) => Ok(("SUMMARY", to_cbor_value(payload)?)),
        GossipSyncFrame::Request(payload) => Ok(("REQUEST", to_cbor_value(payload)?)),
        GossipSyncFrame::Transfer(payload) => Ok(("TRANSFER", to_cbor_value(payload)?)),
        GossipSyncFrame::Receipt(payload) => Ok(("RECEIPT", to_cbor_value(payload)?)),
        GossipSyncFrame::RelayIngest(payload) => Ok(("RELAY_INGEST", to_cbor_value(payload)?)),
    }
}

fn frame_from_envelope_value(envelope: Value) -> Result<GossipSyncFrame, String> {
    let Value::Map(entries) = envelope else {
        return Err("parse gossip frame cbor: envelope must be map".to_string());
    };

    let mut frame_type = None;
    let mut payload = None;
    for (key, value) in entries {
        let Value::Text(key_text) = key else {
            continue;
        };
        match key_text.as_str() {
            "type" => {
                let Value::Text(type_text) = value else {
                    return Err("parse gossip frame cbor: envelope.type must be text".to_string());
                };
                frame_type = Some(type_text);
            }
            "payload" => {
                payload = Some(value);
            }
            _ => {
                // Unknown top-level envelope keys are intentionally ignored by spec.
            }
        }
    }

    let frame_type = frame_type.ok_or_else(|| {
        "parse gossip frame cbor: envelope missing required key `type`".to_string()
    })?;
    let payload = payload.ok_or_else(|| {
        "parse gossip frame cbor: envelope missing required key `payload`".to_string()
    })?;

    match frame_type.as_str() {
        "HELLO" => decode_payload_frame(payload, GossipSyncFrame::Hello),
        "SUMMARY" => decode_payload_frame(payload, GossipSyncFrame::Summary),
        "REQUEST" => decode_payload_frame(payload, GossipSyncFrame::Request),
        "TRANSFER" => decode_payload_frame(payload, GossipSyncFrame::Transfer),
        "RECEIPT" => decode_payload_frame(payload, GossipSyncFrame::Receipt),
        "RELAY_INGEST" => decode_payload_frame(payload, GossipSyncFrame::RelayIngest),
        _ => Err(format!(
            "parse gossip frame cbor: unsupported frame type `{frame_type}`"
        )),
    }
}

fn decode_payload_frame<T, F>(payload: Value, wrap: F) -> Result<GossipSyncFrame, String>
where
    T: for<'de> serde::Deserialize<'de>,
    F: FnOnce(T) -> GossipSyncFrame,
{
    let mut raw = Vec::new();
    into_writer(&payload, &mut raw).map_err(|err| format!("payload re-encode failed: {err}"))?;
    let payload_value: T =
        from_reader(raw.as_slice()).map_err(|err| format!("payload decode failed: {err}"))?;
    Ok(wrap(payload_value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock};

    static TEST_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_env_lock() -> &'static Mutex<()> {
        TEST_ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn unique_test_state_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let counter = TEST_DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("{prefix}-{nanos}-{counter}"))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.as_ref() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn item(fill: u8) -> String {
        format!("{:02x}", fill).repeat(32)
    }

    fn summary_with_bloom_for(items: &[String]) -> SummaryFrame {
        SummaryFrame {
            bloom_filter: build_bloom_filter(items).expect("build bloom"),
            item_count: items.len() as u64,
            preview_item_ids: None,
            preview_cursor: None,
        }
    }

    #[test]
    fn summary_reconciliation_uses_candidates_when_preview_empty() {
        let candidate_a = item(0x20);
        let candidate_b = item(0x10);
        let summary = summary_with_bloom_for(&[candidate_a.clone(), candidate_b.clone()]);
        let want = select_request_item_ids_from_summary_with_context(
            &summary,
            8,
            &[candidate_a.clone(), candidate_b.clone()],
            &[],
        )
        .expect("select want");
        assert_eq!(want, vec![candidate_b, candidate_a]);
    }

    #[test]
    fn summary_reconciliation_excludes_local_have_from_preview() {
        let preview_a = item(0x01);
        let preview_b = item(0x02);
        let summary = SummaryFrame {
            bloom_filter: build_bloom_filter(&[preview_a.clone(), preview_b.clone()])
                .expect("bloom"),
            item_count: 2,
            preview_item_ids: Some(vec![preview_a.clone(), preview_b.clone()]),
            preview_cursor: Some(preview_b.clone()),
        };
        let want =
            select_request_item_ids_from_summary_with_context(&summary, 8, &[], &[preview_a])
                .expect("select want");
        assert_eq!(want, vec![preview_b]);
    }

    #[test]
    fn summary_reconciliation_is_deterministic_for_same_inputs() {
        let preview_a = item(0x0a);
        let preview_b = item(0x0b);
        let candidate = item(0x0c);
        let summary = SummaryFrame {
            bloom_filter: build_bloom_filter(&[
                preview_a.clone(),
                preview_b.clone(),
                candidate.clone(),
            ])
            .expect("bloom"),
            item_count: 3,
            preview_item_ids: Some(vec![preview_b.clone(), preview_a.clone()]),
            preview_cursor: Some(preview_a.clone()),
        };
        let first = select_request_item_ids_from_summary_with_context(
            &summary,
            8,
            std::slice::from_ref(&candidate),
            &[],
        )
        .expect("first");
        let second = select_request_item_ids_from_summary_with_context(
            &summary,
            8,
            std::slice::from_ref(&candidate),
            &[],
        )
        .expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn parse_frame_accepts_unknown_top_level_envelope_key() {
        let pubkey = [0x11u8; 32];
        let node_id = bytes_to_hex_lower(&Sha256::digest(pubkey));
        let hello = build_hello_frame(
            &node_id,
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pubkey),
        )
        .expect("hello");
        let raw = serialize_frame(&hello).expect("serialize");
        let mut value = decode_cbor_value_exact(&raw, "frame").expect("decode");
        let Value::Map(entries) = &mut value else {
            panic!("expected envelope map")
        };
        entries.push((
            Value::Text("x_unknown".to_string()),
            Value::Text("ignored".to_string()),
        ));
        let with_unknown = encode_cbor_value_deterministic(&value).expect("encode");
        assert!(matches!(
            parse_frame(&with_unknown).expect("parse should ignore unknown envelope keys"),
            GossipSyncFrame::Hello(_)
        ));
    }

    #[test]
    fn parse_frame_rejects_summary_unknown_payload_key() {
        let summary = GossipSyncFrame::Summary(SummaryFrame {
            bloom_filter: vec![0u8; BLOOM_FILTER_BYTES],
            item_count: 0,
            preview_item_ids: None,
            preview_cursor: None,
        });
        let raw = serialize_frame(&summary).expect("serialize");
        let mut value = decode_cbor_value_exact(&raw, "frame").expect("decode");
        let Value::Map(envelope_entries) = &mut value else {
            panic!("expected envelope map")
        };
        let payload = envelope_entries
            .iter_mut()
            .find_map(|(key, value)| match key {
                Value::Text(text) if text == "payload" => Some(value),
                _ => None,
            })
            .expect("payload field");
        let Value::Map(payload_entries) = payload else {
            panic!("expected payload map")
        };
        payload_entries.push((
            Value::Text("extra".to_string()),
            Value::Text("boom".to_string()),
        ));
        let invalid = encode_cbor_value_deterministic(&value).expect("encode");
        assert!(parse_frame(&invalid)
            .expect_err("must reject unknown summary payload key")
            .contains("unknown field"));
    }

    #[test]
    fn parse_frame_rejects_multiple_frames_in_one_datagram() {
        let pubkey = [0x11u8; 32];
        let node_id = bytes_to_hex_lower(&Sha256::digest(pubkey));
        let hello = build_hello_frame(
            &node_id,
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pubkey),
        )
        .expect("hello");
        let raw = serialize_frame(&hello).expect("serialize");
        let mut combined = raw.clone();
        combined.extend_from_slice(&raw);
        assert!(parse_frame(&combined)
            .expect_err("must reject trailing frame data")
            .contains("exactly one complete CBOR value"));
    }

    #[test]
    fn parse_and_import_transfer_allows_mixed_validity_objects() {
        let _lock = test_env_lock().lock().expect("lock test env");
        let temp_dir = unique_test_state_dir("aethos-gossip-import-test");
        let _env_guard = EnvVarGuard::set("XDG_STATE_HOME", &temp_dir);

        let local_wayfarer = item(0x77);
        let signing_seed = [5u8; 32];
        let valid_payload = crate::aethos_core::protocol::build_envelope_payload_b64_from_utf8(
            &local_wayfarer,
            "hello",
            &signing_seed,
        )
        .expect("valid payload");
        let valid_item = super::item_id_from_envelope_bytes(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&valid_payload)
                .expect("decode valid payload"),
        );

        let frame = GossipSyncFrame::Transfer(TransferFrame {
            objects: vec![
                TransferObject {
                    item_id: item(0xaa),
                    envelope_b64: "not-base64url".to_string(),
                    expiry_unix_ms: now_unix_ms() + 60_000,
                    hop_count: 1,
                },
                TransferObject {
                    item_id: valid_item.clone(),
                    envelope_b64: valid_payload,
                    expiry_unix_ms: now_unix_ms() + 60_000,
                    hop_count: 1,
                },
            ],
        });

        let raw = serialize_frame(&frame).expect("serialize transfer");
        let parsed = parse_frame(&raw).expect("parse should allow mixed validity objects");
        let GossipSyncFrame::Transfer(parsed_transfer) = parsed else {
            panic!("expected transfer frame")
        };

        let imported = import_transfer_items(
            &local_wayfarer,
            Some("10.0.0.3:47655"),
            Some(&item(0x66)),
            &parsed_transfer.objects,
            now_unix_ms(),
        )
        .expect("import mixed objects");

        assert_eq!(imported.accepted_item_ids, vec![valid_item]);
        assert_eq!(imported.rejected_items.len(), 1);
        assert_eq!(imported.rejected_items[0].code, "MALFORMED_OBJECT");
    }

    #[test]
    fn import_transfer_keeps_message_with_unresolved_author() {
        let _lock = test_env_lock().lock().expect("lock test env");
        let temp_dir = unique_test_state_dir("aethos-gossip-import-test");
        let _env_guard = EnvVarGuard::set("XDG_STATE_HOME", &temp_dir);

        let local_wayfarer = item(0x99);
        let signing_seed = [6u8; 32];
        let payload = crate::aethos_core::protocol::build_envelope_payload_b64_from_utf8(
            &local_wayfarer,
            "hello",
            &signing_seed,
        )
        .expect("payload");
        let object = TransferObject {
            item_id: super::item_id_from_envelope_bytes(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(&payload)
                    .expect("decode payload"),
            ),
            envelope_b64: payload,
            expiry_unix_ms: now_unix_ms() + 60_000,
            hop_count: 1,
        };
        let imported = import_transfer_items(
            &local_wayfarer,
            Some("10.0.0.2:47655"),
            Some(&item(0x55)),
            &[object],
            now_unix_ms(),
        )
        .expect("import");
        assert_eq!(imported.new_messages.len(), 1);
        let msg = &imported.new_messages[0];
        assert!(msg
            .author_wayfarer_id
            .as_ref()
            .map(|value| value.len() == 64)
            .unwrap_or(false));
        assert_eq!(msg.transport_peer.as_deref(), Some("10.0.0.2:47655"));
        let expected_session_peer = item(0x55);
        assert_eq!(
            msg.session_peer.as_deref(),
            Some(expected_session_peer.as_str())
        );
    }

    #[test]
    fn import_transfer_does_not_substitute_author_from_peer() {
        let _lock = test_env_lock().lock().expect("lock test env");
        let temp_dir = unique_test_state_dir("aethos-gossip-import-test");
        let _env_guard = EnvVarGuard::set("XDG_STATE_HOME", &temp_dir);

        let local_wayfarer = item(0x42);
        let signing_seed = [7u8; 32];
        let payload = crate::aethos_core::protocol::build_envelope_payload_b64_from_utf8(
            &local_wayfarer,
            "hello",
            &signing_seed,
        )
        .expect("payload");
        let object = TransferObject {
            item_id: super::item_id_from_envelope_bytes(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(&payload)
                    .expect("decode payload"),
            ),
            envelope_b64: payload,
            expiry_unix_ms: now_unix_ms() + 60_000,
            hop_count: 1,
        };
        let imported = import_transfer_items(
            &local_wayfarer,
            Some("10.0.0.9:47655"),
            Some(&item(0x33)),
            &[object],
            now_unix_ms(),
        )
        .expect("import");
        assert_eq!(imported.new_messages.len(), 1);
        assert!(imported.new_messages[0]
            .author_wayfarer_id
            .as_ref()
            .map(|value| value.len() == 64)
            .unwrap_or(false));
    }

    #[test]
    fn import_transfer_skips_non_utf8_payload_from_chat_preview() {
        let _lock = test_env_lock().lock().expect("lock test env");
        let temp_dir = unique_test_state_dir("aethos-gossip-import-test");
        let _env_guard = EnvVarGuard::set("XDG_STATE_HOME", &temp_dir);

        let local_wayfarer = item(0x24);
        let signing_seed = [8u8; 32];
        let payload = crate::aethos_core::protocol::build_envelope_payload_b64(
            &local_wayfarer,
            &[0xff, 0xfe, 0xfd, 0x00],
            &signing_seed,
        )
        .expect("payload");
        let object = TransferObject {
            item_id: super::item_id_from_envelope_bytes(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(&payload)
                    .expect("decode payload"),
            ),
            envelope_b64: payload,
            expiry_unix_ms: now_unix_ms() + 60_000,
            hop_count: 1,
        };

        let imported = import_transfer_items(
            &local_wayfarer,
            Some("10.0.0.10:47655"),
            Some(&item(0x11)),
            &[object],
            now_unix_ms(),
        )
        .expect("import");

        assert_eq!(imported.accepted_item_ids.len(), 1);
        assert!(imported.rejected_items.is_empty());
        assert!(imported.new_messages.is_empty());
    }
}
