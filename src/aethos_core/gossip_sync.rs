use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use ciborium::value::Value;
use ciborium::{de::from_reader, ser::into_writer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::aethos_core::encounter_scheduler::{
    BudgetProfile as SchedulerBudgetProfile, CargoItem as SchedulerCargoItem, EncounterClass,
    EncounterSchedulerV1, ProximityClass as SchedulerProximityClass,
};
use crate::aethos_core::gossip_store_sqlite::{
    self, ImportWriteObject, RecordPutOutcome, StoredItemRecord,
};
use crate::aethos_core::identity_store::ensure_local_identity;
use crate::aethos_core::logging::log_verbose;
use crate::aethos_core::protocol::{
    bytes_to_hex_lower, decode_cbor_value_exact, decode_envelope_payload_b64,
    decode_envelope_payload_text_preview, encode_cbor_value_deterministic, is_valid_payload_b64,
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
const RELAY_INGEST_MAX_ITEMS_DEFAULT: usize = MAX_WANT_ITEMS;

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
    pub body_bytes: Vec<u8>,
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
    if require_canonical_inbound_frame() {
        let canonical = encode_cbor_value_deterministic(&envelope).map_err(|err| {
            classify_frame_parse_error(&format!("canonical frame encode failed: {err}"))
        })?;
        if canonical.as_slice() != raw {
            return Err(
                "parse gossip frame cbor: frame is not deterministic canonical CBOR".to_string(),
            );
        }
    }

    let frame =
        frame_from_envelope_value(envelope).map_err(|err| classify_frame_parse_error(&err))?;
    validate_frame(&frame)?;
    Ok(frame)
}

fn require_canonical_inbound_frame() -> bool {
    std::env::var("AETHOS_GOSSIP_REQUIRE_CANONICAL_INBOUND_CBOR")
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
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
    let local_have = gossip_store_sqlite::eligible_item_ids(now_unix_ms())?;
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
    let relay_ingest_max_items = relay_ingest_max_items();
    let item_ids = eligible_relay_ingest_item_ids(now_ms, relay_ingest_max_items)?;
    log_verbose(&format!(
        "relay_ingest_frame_built: item_ids={} cap={}",
        item_ids.len(),
        relay_ingest_max_items
    ));
    let frame = GossipSyncFrame::RelayIngest(RelayIngestFrame { item_ids });
    validate_frame(&frame)?;
    Ok(frame)
}

fn relay_ingest_max_items() -> usize {
    std::env::var("AETHOS_RELAY_INGEST_MAX_ITEMS")
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .map(|value| value.clamp(16, MAX_WANT_ITEMS))
        .unwrap_or(RELAY_INGEST_MAX_ITEMS_DEFAULT)
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

pub fn missing_item_ids(item_ids: &[String]) -> Result<Vec<String>, String> {
    if item_ids.is_empty() {
        return Ok(Vec::new());
    }

    let existing = gossip_store_sqlite::get_existing_items_for_ids(item_ids)?;
    Ok(item_ids
        .iter()
        .filter(|item_id| !existing.contains_key(*item_id))
        .cloned()
        .collect())
}

pub fn eligible_item_ids(now_ms: u64) -> Result<Vec<String>, String> {
    let candidate_ids = gossip_store_sqlite::eligible_item_ids(now_ms)?;
    filter_non_self_advertisable_item_ids(candidate_ids)
}

fn eligible_relay_ingest_item_ids(now_ms: u64, max_items: usize) -> Result<Vec<String>, String> {
    let selected = gossip_store_sqlite::eligible_relay_ingest_item_ids(now_ms, max_items)?;
    let mut selected = filter_non_self_advertisable_item_ids(selected)?;
    selected.sort_by_key(|item_id| decode_item_id(item_id).unwrap_or_default());
    Ok(selected)
}

fn filter_non_self_advertisable_item_ids(item_ids: Vec<String>) -> Result<Vec<String>, String> {
    let local_wayfarer_id = match ensure_local_identity() {
        Ok(identity) => identity.wayfarer_id,
        Err(err) => {
            log_verbose(&format!(
                "gossip_advertise_filter_identity_unavailable: {}",
                err
            ));
            return Ok(item_ids);
        }
    };

    let existing = gossip_store_sqlite::get_existing_items_for_ids(&item_ids)?;
    let mut filtered = Vec::with_capacity(item_ids.len());
    let mut dropped = 0usize;
    for item_id in item_ids {
        let Some(record) = existing.get(&item_id) else {
            continue;
        };
        match decode_envelope_payload_b64(&record.envelope_b64) {
            Ok(decoded) if decoded.to_wayfarer_id_hex == local_wayfarer_id => {
                dropped = dropped.saturating_add(1);
            }
            Ok(_) => filtered.push(item_id),
            Err(err) => {
                log_verbose(&format!(
                    "gossip_advertise_filter_decode_failed: item_id={} error={}",
                    item_id, err
                ));
                filtered.push(item_id);
            }
        }
    }

    if dropped > 0 {
        log_verbose(&format!(
            "gossip_advertise_filter_self_target_dropped: dropped={} kept={}",
            dropped,
            filtered.len()
        ));
    }
    Ok(filtered)
}

pub fn record_local_payload(payload_b64: &str, expiry_unix_ms: u64) -> Result<String, String> {
    if !is_valid_payload_b64(payload_b64) {
        return Err("invalid payload_b64 format for gossip storage".to_string());
    }
    let now = now_unix_ms();
    if now + CLOCK_SKEW_TOLERANCE_MS >= expiry_unix_ms {
        return Err("cannot store already-expired object".to_string());
    }

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

    let outcome =
        gossip_store_sqlite::record_local_item(&item_id, payload_b64, expiry_unix_ms, 0, now)?;
    match outcome {
        RecordPutOutcome::Inserted => {
            log_verbose(&format!("object_store_put_insert: item_id={item_id}"));
        }
        RecordPutOutcome::Refreshed {
            refreshed_expiry_unix_ms,
        } => {
            log_verbose(&format!(
                "object_store_put_refresh: item_id={} expiry_unix_ms={}",
                item_id, refreshed_expiry_unix_ms
            ));
        }
        RecordPutOutcome::Dedupe => {
            log_verbose(&format!("object_store_put_dedupe: item_id={item_id}"));
        }
    }
    Ok(item_id)
}

pub fn transfer_items_for_request(
    requested_item_ids: &[String],
    max_items: u32,
    max_bytes: u64,
    now_ms: u64,
) -> Result<Vec<TransferObject>, String> {
    transfer_items_for_request_with_shadow_context(
        requested_item_ids,
        max_items,
        max_bytes,
        now_ms,
        None,
    )
}

#[derive(Debug, Clone)]
pub struct TransferSelectionTelemetry {
    pub planner: &'static str,
    pub selected_items: usize,
    pub consumed_bytes: u64,
    pub stop_reason: String,
    pub tie_break_reason: String,
    pub ranking_top: Vec<String>,
    pub selected_top: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TransferSelectionOutcome {
    pub objects: Vec<TransferObject>,
    pub telemetry: TransferSelectionTelemetry,
}

pub fn transfer_items_for_request_with_shadow_context_and_diagnostics(
    requested_item_ids: &[String],
    max_items: u32,
    max_bytes: u64,
    now_ms: u64,
    peer_wayfarer_id: Option<&str>,
) -> Result<TransferSelectionOutcome, String> {
    let planner = "encounter-scheduler-v1";
    log_verbose(&format!(
        "transfer_select_start: requested={} max_items={} max_bytes={} now_ms={}",
        requested_item_ids.len(),
        max_items,
        max_bytes,
        now_ms
    ));
    let candidates =
        gossip_store_sqlite::transfer_candidates_for_request(requested_item_ids, now_ms)?;
    let scheduler_plan =
        build_scheduler_transfer_plan(&candidates, max_items, max_bytes, now_ms, peer_wayfarer_id)?;

    transfer_legacy_debug::maybe_log_scheduler_vs_legacy_diff(
        &candidates,
        &scheduler_plan.result,
        max_items,
        max_bytes,
        now_ms,
        peer_wayfarer_id,
    );

    if should_use_legacy_transfer_fallback() {
        let mut legacy_sorted = candidates.clone();
        legacy_sorted.sort_by(|a, b| {
            a.envelope_b64
                .len()
                .cmp(&b.envelope_b64.len())
                .then_with(|| a.item_id.cmp(&b.item_id))
        });
        let legacy_plan = transfer_legacy_debug::build_legacy_transfer_plan(
            &legacy_sorted,
            max_items,
            max_bytes,
        )?;
        log_verbose(&format!(
            "transfer_select_legacy_fallback: selected_items={} consumed_bytes={} stop_reason={}",
            legacy_plan.selected.len(),
            legacy_plan.consumed_bytes,
            legacy_plan.stop_reason.as_str(),
        ));
        return Ok(TransferSelectionOutcome {
            objects: legacy_plan.selected.clone(),
            telemetry: TransferSelectionTelemetry {
                planner: "legacy-fallback",
                selected_items: legacy_plan.selected.len(),
                consumed_bytes: legacy_plan.consumed_bytes,
                stop_reason: legacy_plan.stop_reason.as_str().to_string(),
                tie_break_reason: "none".to_string(),
                ranking_top: legacy_plan.ranking_order.into_iter().take(5).collect(),
                selected_top: legacy_plan
                    .selected
                    .iter()
                    .map(|item| item.item_id.clone())
                    .take(5)
                    .collect(),
            },
        });
    }

    let ranking_top = scheduler_plan
        .result
        .ranking_order()
        .into_iter()
        .take(5)
        .collect::<Vec<_>>();
    let selected_top = scheduler_plan
        .result
        .selected_prefix_item_ids()
        .into_iter()
        .take(5)
        .collect::<Vec<_>>();
    let selected_items_total = scheduler_plan.selected.len();
    log_verbose(&format!(
        "transfer_select_done: planner={} selected_items={} consumed_bytes={} stop_reason={} tie_break_reason={} ranking_top={} selected_top={}",
        planner,
        scheduler_plan.selected.len(),
        scheduler_plan.consumed_bytes,
        scheduler_plan.result.stop_reason.as_str(),
        scheduler_plan.result.tie_break_reason.as_str(),
        json_string_array(&ranking_top),
        json_string_array(&selected_top),
    ));

    Ok(TransferSelectionOutcome {
        objects: scheduler_plan.selected,
        telemetry: TransferSelectionTelemetry {
            planner,
            selected_items: selected_items_total,
            consumed_bytes: scheduler_plan.consumed_bytes,
            stop_reason: scheduler_plan.result.stop_reason.as_str().to_string(),
            tie_break_reason: scheduler_plan.result.tie_break_reason.as_str().to_string(),
            ranking_top,
            selected_top,
        },
    })
}

pub fn transfer_items_for_request_with_shadow_context(
    requested_item_ids: &[String],
    max_items: u32,
    max_bytes: u64,
    now_ms: u64,
    peer_wayfarer_id: Option<&str>,
) -> Result<Vec<TransferObject>, String> {
    transfer_items_for_request_with_shadow_context_and_diagnostics(
        requested_item_ids,
        max_items,
        max_bytes,
        now_ms,
        peer_wayfarer_id,
    )
    .map(|outcome| outcome.objects)
}

#[derive(Debug)]
struct SchedulerTransferPlan {
    selected: Vec<TransferObject>,
    consumed_bytes: u64,
    result: crate::aethos_core::encounter_scheduler::EncounterSchedulerResult,
}

fn build_scheduler_transfer_plan(
    candidates: &[StoredItemRecord],
    max_items: u32,
    max_bytes: u64,
    now_ms: u64,
    peer_wayfarer_id: Option<&str>,
) -> Result<SchedulerTransferPlan, String> {
    let mut scheduler_items = Vec::with_capacity(candidates.len());
    let mut item_to_stored = BTreeMap::<String, &StoredItemRecord>::new();
    let mut item_to_wire_size = BTreeMap::<String, u64>::new();
    for candidate in candidates {
        let (profile, scheduler_item) =
            shadow_profile_from_stored(candidate, now_ms, peer_wayfarer_id)?;
        item_to_stored.insert(profile.item_id.clone(), candidate);
        item_to_wire_size.insert(profile.item_id, profile.decoded_wire_bytes as u64);
        scheduler_items.push(scheduler_item);
    }

    let result = schedule_transfer_candidates_with_encounter_scheduler(
        EncounterClass::Short,
        max_items,
        max_bytes,
        now_ms,
        &scheduler_items,
    )?;

    let mut selected = Vec::with_capacity(result.selected_prefix.len());
    let mut consumed_bytes = 0u64;
    for ranked in &result.selected_prefix {
        let item_id = ranked.cargo_item.item_id.as_str();
        let stored = item_to_stored
            .get(item_id)
            .ok_or_else(|| format!("scheduler selected unknown item_id: {item_id}"))?;
        let fallback_wire_size = stored.envelope_b64.len() as u64;
        let wire_size = *item_to_wire_size
            .get(item_id)
            .unwrap_or(&fallback_wire_size);
        consumed_bytes = consumed_bytes.saturating_add(wire_size);
        selected.push(TransferObject {
            item_id: stored.item_id.clone(),
            envelope_b64: stored.envelope_b64.clone(),
            expiry_unix_ms: stored.expiry_unix_ms,
            hop_count: stored.hop_count.saturating_add(1),
        });
    }

    Ok(SchedulerTransferPlan {
        selected,
        consumed_bytes,
        result,
    })
}

fn schedule_transfer_candidates_with_encounter_scheduler(
    encounter_class: EncounterClass,
    max_items: u32,
    max_bytes: u64,
    now_ms: u64,
    scheduler_items: &[SchedulerCargoItem],
) -> Result<crate::aethos_core::encounter_scheduler::EncounterSchedulerResult, String> {
    let scheduler_budget = SchedulerBudgetProfile {
        max_items: std::cmp::min(max_items as i32, MAX_TRANSFER_ITEMS as i32),
        max_bytes: std::cmp::min(max_bytes, MAX_TRANSFER_BYTES) as i32,
        max_duration_ms: None,
        durable_cargo_ratio_cap: None,
        preferred_transfer_unit_bytes: 32_768,
        expiry_urgency_horizon_ms: 900_000,
        stagnation_horizon_ms: 3_600_000,
        target_replica_count_default: 6,
    };

    EncounterSchedulerV1::new()
        .schedule(encounter_class, &scheduler_budget, now_ms, scheduler_items)
        .map_err(|err| format!("encounter scheduler primary planning failed: {err:?}"))
}

fn should_use_legacy_transfer_fallback() -> bool {
    if !cfg!(any(debug_assertions, test)) {
        return false;
    }
    std::env::var("AETHOS_ROUTING_LEGACY_FALLBACK")
        .map(|raw| {
            let normalized = raw.trim().to_ascii_lowercase();
            normalized == "1" || normalized == "true" || normalized == "on"
        })
        .unwrap_or(false)
}

mod transfer_legacy_debug {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum LegacyPlannerStopReason {
        Completed,
        BudgetItemsExhausted,
        BudgetBytesExhausted,
    }

    impl LegacyPlannerStopReason {
        pub(super) fn as_str(self) -> &'static str {
            match self {
                Self::Completed => "completed",
                Self::BudgetItemsExhausted => "budget-items-exhausted",
                Self::BudgetBytesExhausted => "budget-bytes-exhausted",
            }
        }
    }

    #[derive(Debug)]
    pub(super) struct LegacyTransferPlan {
        pub(super) selected: Vec<TransferObject>,
        pub(super) consumed_bytes: u64,
        pub(super) stop_reason: LegacyPlannerStopReason,
        pub(super) ranking_order: Vec<String>,
    }

    pub(super) fn build_legacy_transfer_plan(
        sorted_candidates: &[StoredItemRecord],
        max_items: u32,
        max_bytes: u64,
    ) -> Result<LegacyTransferPlan, String> {
        let mut selected = Vec::new();
        let mut consumed_bytes = 0u64;
        let mut stop_reason = LegacyPlannerStopReason::Completed;
        let max_transfer_bytes = max_bytes.min(MAX_TRANSFER_BYTES);
        let ranking_order = sorted_candidates
            .iter()
            .map(|stored| stored.item_id.clone())
            .collect::<Vec<_>>();

        for stored in sorted_candidates {
            if selected.len() >= max_items as usize || selected.len() >= MAX_TRANSFER_ITEMS {
                stop_reason = LegacyPlannerStopReason::BudgetItemsExhausted;
                break;
            }

            let envelope_raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&stored.envelope_b64)
                .map_err(|err| format!("stored envelope decode failed: {err}"))?;
            let projected = consumed_bytes.saturating_add(envelope_raw.len() as u64);
            if projected > max_transfer_bytes {
                stop_reason = LegacyPlannerStopReason::BudgetBytesExhausted;
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

        Ok(LegacyTransferPlan {
            selected,
            consumed_bytes,
            stop_reason,
            ranking_order,
        })
    }

    #[derive(Debug, Clone, Copy, Default)]
    struct SelectionRoutingStats {
        direct_count: usize,
        transit_count: usize,
        other_count: usize,
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    struct SelectionTierDistribution {
        tier0: usize,
        tier1: usize,
        tier2: usize,
        tier3: usize,
        tier4: usize,
        tier5: usize,
    }

    impl SelectionTierDistribution {
        fn increment(&mut self, tier: i32) {
            match tier {
                0 => self.tier0 += 1,
                1 => self.tier1 += 1,
                2 => self.tier2 += 1,
                3 => self.tier3 += 1,
                4 => self.tier4 += 1,
                5 => self.tier5 += 1,
                _ => {}
            }
        }

        fn as_json(self) -> String {
            format!(
                "{{\"tier0\":{},\"tier1\":{},\"tier2\":{},\"tier3\":{},\"tier4\":{},\"tier5\":{}}}",
                self.tier0, self.tier1, self.tier2, self.tier3, self.tier4, self.tier5
            )
        }
    }

    impl SelectionRoutingStats {
        fn increment(&mut self, proximity: SchedulerProximityClass) {
            match proximity {
                SchedulerProximityClass::DestinationPeer => self.direct_count += 1,
                SchedulerProximityClass::LikelyCloser => self.transit_count += 1,
                SchedulerProximityClass::Other => self.other_count += 1,
            }
        }

        fn transit_direct_ratio(self) -> String {
            let total = self.direct_count + self.transit_count;
            if total == 0 {
                return "null".to_string();
            }
            format!("{:.6}", self.transit_count as f64 / total as f64)
        }
    }

    #[derive(Debug, Clone)]
    pub(super) struct SchedulerCandidateProfile {
        pub(super) item_id: String,
        pub(super) tier: i32,
        pub(super) proximity: SchedulerProximityClass,
        pub(super) decoded_wire_bytes: usize,
    }

    #[derive(Debug)]
    pub(super) struct ShadowComparisonTelemetry {
        pub(super) old_top_n: Vec<String>,
        pub(super) new_top_n: Vec<String>,
        pub(super) changed_first_selected_item: bool,
        pub(super) old_first_selected_item: Option<String>,
        pub(super) new_first_selected_item: Option<String>,
        pub(super) changed_stop_reason: bool,
        pub(super) old_stop_reason: String,
        pub(super) new_stop_reason: String,
        pub(super) changed_tier_distribution: bool,
        old_tier_distribution: SelectionTierDistribution,
        new_tier_distribution: SelectionTierDistribution,
        pub(super) changed_transit_direct_ratio: bool,
        pub(super) old_transit_direct_ratio: String,
        pub(super) new_transit_direct_ratio: String,
    }

    impl ShadowComparisonTelemetry {
        fn has_diff(&self) -> bool {
            self.old_top_n != self.new_top_n
                || self.changed_first_selected_item
                || self.changed_stop_reason
                || self.changed_tier_distribution
                || self.changed_transit_direct_ratio
        }

        fn as_json_fragment(&self) -> String {
            format!(
            "{{\"old_top_n\":{},\"new_top_n\":{},\"changed_first_selected_item\":{},\"old_first_selected_item\":{},\"new_first_selected_item\":{},\"changed_stop_reason\":{},\"old_stop_reason\":\"{}\",\"new_stop_reason\":\"{}\",\"changed_tier_distribution\":{},\"old_tier_distribution\":{},\"new_tier_distribution\":{},\"changed_transit_direct_ratio\":{},\"old_transit_direct_ratio\":{},\"new_transit_direct_ratio\":{}}}",
            json_string_array(&self.old_top_n),
            json_string_array(&self.new_top_n),
            self.changed_first_selected_item,
            json_option_string(self.old_first_selected_item.as_deref()),
            json_option_string(self.new_first_selected_item.as_deref()),
            self.changed_stop_reason,
            self.old_stop_reason,
            self.new_stop_reason,
            self.changed_tier_distribution,
            self.old_tier_distribution.as_json(),
            self.new_tier_distribution.as_json(),
            self.changed_transit_direct_ratio,
            json_raw_or_null(&self.old_transit_direct_ratio),
            json_raw_or_null(&self.new_transit_direct_ratio)
        )
        }
    }

    pub(super) fn maybe_log_scheduler_vs_legacy_diff(
        candidates: &[StoredItemRecord],
        scheduler_result: &crate::aethos_core::encounter_scheduler::EncounterSchedulerResult,
        max_items: u32,
        max_bytes: u64,
        now_ms: u64,
        peer_wayfarer_id: Option<&str>,
    ) {
        if !scheduler_shadow_mode_enabled() {
            return;
        }

        let mut candidate_profiles = Vec::with_capacity(candidates.len());
        let mut scheduler_items = Vec::with_capacity(candidates.len());
        let mut legacy_sorted = candidates.to_vec();
        legacy_sorted.sort_by(|a, b| {
            a.envelope_b64
                .len()
                .cmp(&b.envelope_b64.len())
                .then_with(|| a.item_id.cmp(&b.item_id))
        });
        let legacy_plan = match build_legacy_transfer_plan(&legacy_sorted, max_items, max_bytes) {
            Ok(plan) => plan,
            Err(err) => {
                log_verbose(&format!(
                "transfer_scheduler_shadow_error: mode=debug reason=legacy_plan_failed error={err}"
            ));
                return;
            }
        };

        for candidate in candidates {
            let (profile, scheduler_item) =
                match shadow_profile_from_stored(candidate, now_ms, peer_wayfarer_id) {
                    Ok(profile) => profile,
                    Err(_) => continue,
                };
            candidate_profiles.push(profile);
            scheduler_items.push(scheduler_item);
        }

        if scheduler_items.is_empty() {
            return;
        }

        let shadow_scheduler_result = match schedule_transfer_candidates_with_encounter_scheduler(
            EncounterClass::Short,
            max_items,
            max_bytes,
            now_ms,
            &scheduler_items,
        ) {
            Ok(result) => result,
            Err(err) => {
                log_verbose(&format!(
                "transfer_scheduler_shadow_error: mode=debug reason=scheduler_failed error={:?}",
                err
            ));
                return;
            }
        };

        let telemetry = build_shadow_comparison_telemetry(
            &candidate_profiles,
            &legacy_plan,
            scheduler_result,
            5,
        );
        if telemetry.has_diff() {
            log_verbose(&format!(
            "transfer_scheduler_shadow_diff: mode=debug payload={} scheduler_shadow_stop_reason={} scheduler_shadow_tie_break_reason={}",
            telemetry.as_json_fragment(),
            shadow_scheduler_result.stop_reason.as_str(),
            shadow_scheduler_result.tie_break_reason.as_str(),
        ));
        }
    }

    pub(super) fn build_shadow_comparison_telemetry(
        candidate_profiles: &[SchedulerCandidateProfile],
        legacy_plan: &LegacyTransferPlan,
        scheduler_result: &crate::aethos_core::encounter_scheduler::EncounterSchedulerResult,
        top_n: usize,
    ) -> ShadowComparisonTelemetry {
        let old_top_n = legacy_plan
            .ranking_order
            .iter()
            .take(top_n)
            .cloned()
            .collect::<Vec<_>>();
        let new_top_n = scheduler_result
            .ranking_order()
            .iter()
            .take(top_n)
            .cloned()
            .collect::<Vec<_>>();
        let old_first_selected_item = legacy_plan
            .selected
            .first()
            .map(|item| item.item_id.clone());
        let new_first_selected_item = scheduler_result
            .selected_prefix
            .first()
            .map(|item| item.cargo_item.item_id.clone());
        let old_stop_reason = legacy_plan.stop_reason.as_str().to_string();
        let new_stop_reason = scheduler_result.stop_reason.as_str().to_string();

        let old_selected_ids = legacy_plan
            .selected
            .iter()
            .map(|item| item.item_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let new_selected_ids = scheduler_result
            .selected_prefix
            .iter()
            .map(|item| item.cargo_item.item_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();

        let mut old_tier_distribution = SelectionTierDistribution::default();
        let mut new_tier_distribution = SelectionTierDistribution::default();
        let mut old_routing_stats = SelectionRoutingStats::default();
        let mut new_routing_stats = SelectionRoutingStats::default();
        for profile in candidate_profiles {
            if old_selected_ids.contains(profile.item_id.as_str()) {
                old_tier_distribution.increment(profile.tier);
                old_routing_stats.increment(profile.proximity);
            }
            if new_selected_ids.contains(profile.item_id.as_str()) {
                new_tier_distribution.increment(profile.tier);
                new_routing_stats.increment(profile.proximity);
            }
        }

        let old_transit_direct_ratio = old_routing_stats.transit_direct_ratio();
        let new_transit_direct_ratio = new_routing_stats.transit_direct_ratio();

        ShadowComparisonTelemetry {
            changed_first_selected_item: old_first_selected_item != new_first_selected_item,
            changed_stop_reason: old_stop_reason != new_stop_reason,
            changed_tier_distribution: old_tier_distribution != new_tier_distribution,
            changed_transit_direct_ratio: old_transit_direct_ratio != new_transit_direct_ratio,
            old_top_n,
            new_top_n,
            old_first_selected_item,
            new_first_selected_item,
            old_stop_reason,
            new_stop_reason,
            old_tier_distribution,
            new_tier_distribution,
            old_transit_direct_ratio,
            new_transit_direct_ratio,
        }
    }

    fn scheduler_shadow_mode_enabled() -> bool {
        if cfg!(any(debug_assertions, test)) {
            return std::env::var("AETHOS_ROUTING_SHADOW_MODE")
                .map(|raw| {
                    let normalized = raw.trim().to_ascii_lowercase();
                    !(normalized == "0" || normalized == "false" || normalized == "off")
                })
                .unwrap_or(true);
        }
        false
    }
}

fn shadow_profile_from_stored(
    stored: &StoredItemRecord,
    now_ms: u64,
    peer_wayfarer_id: Option<&str>,
) -> Result<
    (
        transfer_legacy_debug::SchedulerCandidateProfile,
        SchedulerCargoItem,
    ),
    String,
> {
    let decoded = decode_envelope_payload_b64(&stored.envelope_b64)
        .map_err(|err| format!("decode transfer candidate payload failed: {err}"))?;
    let decoded_wire_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&stored.envelope_b64)
        .map_err(|err| format!("decode transfer candidate envelope bytes failed: {err}"))?
        .len();
    let proximity = if let Some(peer) = peer_wayfarer_id {
        if decoded.to_wayfarer_id_hex == peer {
            SchedulerProximityClass::DestinationPeer
        } else {
            SchedulerProximityClass::LikelyCloser
        }
    } else {
        SchedulerProximityClass::Other
    };

    let ttl_ms = stored.expiry_unix_ms.saturating_sub(now_ms);
    let tier = if decoded.body.len() <= 1024 {
        if ttl_ms <= 900_000 {
            if proximity == SchedulerProximityClass::DestinationPeer {
                1
            } else {
                2
            }
        } else {
            4
        }
    } else {
        4
    };

    let profile = transfer_legacy_debug::SchedulerCandidateProfile {
        item_id: stored.item_id.clone(),
        tier,
        proximity,
        decoded_wire_bytes,
    };
    let scheduler_item = SchedulerCargoItem {
        item_id: stored.item_id.clone(),
        tier,
        size_bytes: std::cmp::max(1, decoded.body.len() as i32),
        expiry_at_unix_ms: stored.expiry_unix_ms,
        known_replica_count: Some(stored.hop_count as i32),
        target_replica_count: Some(6),
        durably_stored: Some(false),
        relay_ingested: Some(stored.hop_count > 0),
        receipt_coverage: Some(0.0),
        last_forwarded_at_unix_ms: Some(stored.recorded_at_unix_ms),
        proximity_class: Some(proximity),
        explicit_user_initiated: Some(false),
        content_class_score: Some(0.0),
        destination_rank: if proximity == SchedulerProximityClass::DestinationPeer {
            1
        } else {
            0
        },
        estimated_duration_ms: None,
    };
    Ok((profile, scheduler_item))
}

fn json_string_array(values: &[String]) -> String {
    let encoded = values
        .iter()
        .map(|value| format!("\"{}\"", value))
        .collect::<Vec<_>>();
    format!("[{}]", encoded.join(","))
}

fn json_option_string(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("\"{}\"", value),
        None => "null".to_string(),
    }
}

fn json_raw_or_null(value: &str) -> String {
    if value == "null" {
        "null".to_string()
    } else {
        value.to_string()
    }
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
    let mut accepted_item_ids = Vec::new();
    let mut rejected_items = Vec::new();
    let mut new_messages = Vec::new();
    let mut pending_new_records = BTreeMap::<String, ImportWriteObject>::new();
    let mut pending_new_inserts = Vec::new();
    let existing = gossip_store_sqlite::get_existing_items_for_ids(
        &objects
            .iter()
            .map(|object| object.item_id.clone())
            .collect::<Vec<_>>(),
    )?;

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

        let existing_item = if let Some(pending) = pending_new_records.get(&object.item_id) {
            Some(StoredItemRecord {
                item_id: pending.item_id.clone(),
                envelope_b64: pending.envelope_b64.clone(),
                expiry_unix_ms: pending.expiry_unix_ms,
                hop_count: pending.hop_count,
                recorded_at_unix_ms: pending.recorded_at_unix_ms,
            })
        } else {
            existing.get(&object.item_id).cloned()
        };

        match existing_item {
            Some(existing_item) if existing_item.envelope_b64 != object.envelope_b64 => {
                rejected_items.push(RejectedItem {
                    item_id: object.item_id.clone(),
                    code: "ITEM_ID_MISMATCH".to_string(),
                    message: "existing item_id maps to different envelope bytes".to_string(),
                });
            }
            Some(existing_item) if object.hop_count < existing_item.hop_count => {
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
                let insert = ImportWriteObject {
                    item_id: object.item_id.clone(),
                    envelope_b64: object.envelope_b64.clone(),
                    expiry_unix_ms: object.expiry_unix_ms,
                    hop_count: object.hop_count,
                    recorded_at_unix_ms: now_ms,
                };
                pending_new_records.insert(object.item_id.clone(), insert.clone());
                pending_new_inserts.push(insert);
                accepted_item_ids.push(object.item_id.clone());
                if parsed.to_wayfarer_id_hex == local_wayfarer_id {
                    let preview_text = decode_envelope_payload_text_preview(&object.envelope_b64)
                        .unwrap_or_default();
                    new_messages.push(ImportedEnvelope {
                        item_id: object.item_id.clone(),
                        author_wayfarer_id: Some(parsed.author_wayfarer_id_hex.clone()),
                        transport_peer: transport_peer.map(|value| value.to_string()),
                        session_peer: session_peer_wayfarer_id.map(|value| value.to_string()),
                        body_bytes: parsed.body,
                        text: preview_text,
                        received_at_unix: (now_ms / 1000) as i64,
                        manifest_id_hex: Some(parsed.manifest_id_hex),
                    });
                } else {
                    log_verbose(&format!(
                        "transfer_import_stored_nonlocal: item_id={} to={} local={}",
                        object.item_id, parsed.to_wayfarer_id_hex, local_wayfarer_id
                    ));
                }
            }
        }
    }

    gossip_store_sqlite::insert_import_items(&pending_new_inserts, now_ms)?;
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
    let local_wayfarer_id = ensure_local_identity()
        .ok()
        .map(|identity| identity.wayfarer_id);
    let mut ranked = gossip_store_sqlite::summary_preview_candidates(now_ms)?
        .into_iter()
        .filter(|item| {
            let Some(local_wayfarer_id) = local_wayfarer_id.as_deref() else {
                return true;
            };
            decode_envelope_payload_b64(&item.envelope_b64)
                .map(|decoded| decoded.to_wayfarer_id_hex != local_wayfarer_id)
                .unwrap_or(true)
        })
        .map(|item| {
            (
                item.hop_count,
                item.envelope_b64.len(),
                item.recorded_at_unix_ms,
                decode_item_id(&item.item_id).unwrap_or_default(),
                item.item_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.3.cmp(&right.3))
    });

    let mut preview_item_ids = ranked
        .into_iter()
        .take(MAX_SUMMARY_PREVIEW_ITEMS)
        .map(|(_, _, _, _, item_id)| item_id)
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
    use crate::aethos_core::encounter_scheduler::EncounterTieBreakReason;
    use crate::aethos_core::vectors::load_envelope_vectors;

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

        fn clear(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
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
    fn parse_frame_accepts_noncanonical_semantic_equivalent_cbor() {
        let _lock = test_env_lock().lock().expect("lock test env");
        let _canonical_off_guard =
            EnvVarGuard::clear("AETHOS_GOSSIP_REQUIRE_CANONICAL_INBOUND_CBOR");

        let pubkey = [0x22u8; 32];
        let node_id = bytes_to_hex_lower(&Sha256::digest(pubkey));
        let hello = build_hello_frame(
            &node_id,
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pubkey),
        )
        .expect("hello");
        let canonical_raw = serialize_frame(&hello).expect("serialize");

        let envelope = decode_cbor_value_exact(&canonical_raw, "frame").expect("decode");
        let Value::Map(entries) = envelope else {
            panic!("expected envelope map")
        };
        let payload = entries
            .iter()
            .find_map(|(key, value)| match key {
                Value::Text(text) if text == "payload" => Some(value.clone()),
                _ => None,
            })
            .expect("payload");
        let frame_type = entries
            .iter()
            .find_map(|(key, value)| match (key, value) {
                (Value::Text(key_text), Value::Text(type_text)) if key_text == "type" => {
                    Some(type_text.clone())
                }
                _ => None,
            })
            .expect("type");

        let noncanonical = Value::Map(vec![
            (Value::Text("payload".to_string()), payload),
            (Value::Text("type".to_string()), Value::Text(frame_type)),
        ]);
        let mut noncanonical_raw = Vec::new();
        into_writer(&noncanonical, &mut noncanonical_raw).expect("encode noncanonical");

        let parsed = parse_frame(&noncanonical_raw).expect("parse noncanonical frame");
        assert!(matches!(parsed, GossipSyncFrame::Hello(_)));
    }

    #[test]
    fn cross_client_vectors_import_into_rust_gossip_store() {
        let _lock = test_env_lock().lock().expect("lock test env");
        let temp_dir = unique_test_state_dir("aethos-gossip-import-cross-client-vectors");
        let _xdg_state_home_guard = EnvVarGuard::set("XDG_STATE_HOME", &temp_dir);
        let _aethos_state_dir_guard = EnvVarGuard::clear("AETHOS_STATE_DIR");

        let vector_set = load_envelope_vectors();

        for vector in vector_set.vectors {
            let now_ms = now_unix_ms();
            let expected_preview =
                crate::aethos_core::protocol::decode_envelope_payload_text_preview(
                    &vector.payload_b64,
                )
                .unwrap_or_default();
            let transfer = TransferObject {
                item_id: vector.item_id_hex.clone(),
                envelope_b64: vector.payload_b64,
                expiry_unix_ms: now_ms + 60_000,
                hop_count: 1,
            };

            let imported = import_transfer_items(
                &vector.expected_decoded.to_wayfarer_id,
                Some("127.0.0.1:47655"),
                Some(&item(0x44)),
                &[transfer],
                now_ms,
            )
            .expect("import vector transfer");

            assert_eq!(imported.accepted_item_ids, vec![vector.item_id_hex]);
            assert!(imported.rejected_items.is_empty());
            assert_eq!(imported.new_messages.len(), 1);
            assert_eq!(imported.new_messages[0].text, expected_preview);
            assert_eq!(
                imported.new_messages[0].item_id,
                imported.accepted_item_ids[0]
            );
        }
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
        assert_eq!(imported.new_messages.len(), 1);
        assert!(imported.new_messages[0].text.is_empty());
        assert_eq!(
            imported.new_messages[0].body_bytes,
            vec![0xff, 0xfe, 0xfd, 0x00]
        );
    }

    #[derive(Debug, Deserialize)]
    struct ShadowFixtureManifest {
        fixtures: Vec<ShadowFixtureRef>,
    }

    #[derive(Debug, Deserialize)]
    struct ShadowFixtureRef {
        fixture: String,
    }

    #[derive(Debug, Deserialize)]
    struct ShadowFixture {
        #[serde(rename = "nowUnixMs")]
        now_unix_ms: u64,
        #[serde(rename = "budgetProfile")]
        budget_profile: ShadowFixtureBudget,
        #[serde(rename = "cargoItems")]
        cargo_items: Vec<ShadowFixtureCargoItem>,
    }

    #[derive(Debug, Deserialize)]
    struct ShadowFixtureBudget {
        #[serde(rename = "maxItems")]
        max_items: i32,
        #[serde(rename = "maxBytes")]
        max_bytes: i32,
    }

    #[derive(Debug, Deserialize)]
    struct ShadowFixtureCargoItem {
        #[serde(rename = "itemID")]
        item_id: String,
        #[serde(rename = "sizeBytes")]
        size_bytes: i32,
        #[serde(rename = "expiryAtUnixMs")]
        expiry_at_unix_ms: u64,
        #[serde(rename = "knownReplicaCount")]
        known_replica_count: Option<i32>,
    }

    #[derive(Debug, Deserialize)]
    struct PrimaryFixture {
        #[serde(rename = "encounterClass")]
        encounter_class: EncounterClass,
        #[serde(rename = "nowUnixMs")]
        now_unix_ms: u64,
        #[serde(rename = "budgetProfile")]
        budget_profile: PrimaryFixtureBudget,
        #[serde(rename = "cargoItems")]
        cargo_items: Vec<PrimaryFixtureCargoItem>,
        expected: PrimaryFixtureExpected,
    }

    #[derive(Debug, Deserialize)]
    struct PrimaryFixtureBudget {
        #[serde(rename = "maxItems")]
        max_items: i32,
        #[serde(rename = "maxBytes")]
        max_bytes: i32,
    }

    #[derive(Debug, Deserialize)]
    struct PrimaryFixtureCargoItem {
        #[serde(rename = "itemID")]
        item_id: String,
        tier: i32,
        #[serde(rename = "sizeBytes")]
        size_bytes: i32,
        #[serde(rename = "expiryAtUnixMs")]
        expiry_at_unix_ms: u64,
        #[serde(rename = "knownReplicaCount")]
        known_replica_count: Option<i32>,
        #[serde(rename = "targetReplicaCount")]
        target_replica_count: Option<i32>,
        #[serde(rename = "durablyStored")]
        durably_stored: Option<bool>,
        #[serde(rename = "relayIngested")]
        relay_ingested: Option<bool>,
        #[serde(rename = "receiptCoverage")]
        receipt_coverage: Option<f64>,
        #[serde(rename = "lastForwardedAtUnixMs")]
        last_forwarded_at_unix_ms: Option<u64>,
        #[serde(rename = "proximityClass")]
        proximity_class: Option<SchedulerProximityClass>,
        #[serde(rename = "explicitUserInitiated")]
        explicit_user_initiated: Option<bool>,
        #[serde(rename = "contentClassScore")]
        content_class_score: Option<f64>,
        #[serde(rename = "destinationRank")]
        destination_rank: i32,
        #[serde(rename = "estimatedDurationMs")]
        estimated_duration_ms: Option<i32>,
    }

    #[derive(Debug, Deserialize)]
    struct PrimaryFixtureExpected {
        #[serde(rename = "rankingOrder")]
        ranking_order: Vec<String>,
        #[serde(rename = "selectedPrefix")]
        selected_prefix: Vec<String>,
        #[serde(rename = "stopReason")]
        stop_reason: String,
        #[serde(rename = "tieBreakReason")]
        tie_break_reason: Option<String>,
    }

    #[test]
    fn primary_transfer_scheduler_path_matches_canonical_fixture_outputs() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("test-data/routing/encounter-ranking");
        let manifest_bytes = std::fs::read(root.join("manifest.json")).expect("read manifest");
        let manifest: ShadowFixtureManifest =
            serde_json::from_slice(&manifest_bytes).expect("parse manifest");

        for fixture_ref in manifest.fixtures {
            let fixture_path = fixture_ref.fixture.trim_start_matches("./");
            let fixture_bytes = std::fs::read(root.join(fixture_path)).expect("read fixture");
            let fixture: PrimaryFixture =
                serde_json::from_slice(&fixture_bytes).expect("parse fixture");

            let scheduler_items = fixture
                .cargo_items
                .iter()
                .map(|item| SchedulerCargoItem {
                    item_id: item.item_id.clone(),
                    tier: item.tier,
                    size_bytes: item.size_bytes,
                    expiry_at_unix_ms: item.expiry_at_unix_ms,
                    known_replica_count: item.known_replica_count,
                    target_replica_count: item.target_replica_count,
                    durably_stored: item.durably_stored,
                    relay_ingested: item.relay_ingested,
                    receipt_coverage: item.receipt_coverage,
                    last_forwarded_at_unix_ms: item.last_forwarded_at_unix_ms,
                    proximity_class: item.proximity_class,
                    explicit_user_initiated: item.explicit_user_initiated,
                    content_class_score: item.content_class_score,
                    destination_rank: item.destination_rank,
                    estimated_duration_ms: item.estimated_duration_ms,
                })
                .collect::<Vec<_>>();

            let result = schedule_transfer_candidates_with_encounter_scheduler(
                fixture.encounter_class,
                fixture.budget_profile.max_items.max(0) as u32,
                fixture.budget_profile.max_bytes.max(0) as u64,
                fixture.now_unix_ms,
                &scheduler_items,
            )
            .expect("schedule fixture via primary transfer helper");

            assert_eq!(result.ranking_order(), fixture.expected.ranking_order);
            assert_eq!(
                result.selected_prefix_item_ids(),
                fixture.expected.selected_prefix
            );
            assert_eq!(result.stop_reason.as_str(), fixture.expected.stop_reason);
            assert_eq!(
                result.tie_break_reason.as_str(),
                fixture
                    .expected
                    .tie_break_reason
                    .as_deref()
                    .unwrap_or(EncounterTieBreakReason::None.as_str())
            );
        }
    }

    #[test]
    fn shadow_comparison_runs_against_canonical_fixture_suite() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("test-data/routing/encounter-ranking");
        let manifest_bytes = std::fs::read(root.join("manifest.json")).expect("read manifest");
        let manifest: ShadowFixtureManifest =
            serde_json::from_slice(&manifest_bytes).expect("parse manifest");

        for fixture_ref in manifest.fixtures {
            let fixture_path = fixture_ref.fixture.trim_start_matches("./");
            let fixture_bytes = std::fs::read(root.join(fixture_path)).expect("read fixture");
            let fixture: ShadowFixture =
                serde_json::from_slice(&fixture_bytes).expect("parse fixture");

            let candidates = fixture
                .cargo_items
                .iter()
                .map(|fixture_item| StoredItemRecord {
                    item_id: fixture_item.item_id.clone(),
                    envelope_b64: crate::aethos_core::protocol::build_envelope_payload_b64(
                        &item(0x42),
                        &vec![0x61; fixture_item.size_bytes.max(1) as usize],
                        &[9u8; 32],
                    )
                    .expect("build payload"),
                    expiry_unix_ms: fixture_item.expiry_at_unix_ms,
                    hop_count: fixture_item.known_replica_count.unwrap_or(0).max(0) as u16,
                    recorded_at_unix_ms: fixture.now_unix_ms.saturating_sub(1_000),
                })
                .collect::<Vec<_>>();

            let legacy = transfer_legacy_debug::build_legacy_transfer_plan(
                &candidates,
                fixture.budget_profile.max_items.max(0) as u32,
                fixture.budget_profile.max_bytes.max(0) as u64,
            )
            .expect("legacy plan");

            let mut profiles = Vec::new();
            let mut scheduler_items = Vec::new();
            for candidate in &candidates {
                let (profile, scheduler_item) =
                    shadow_profile_from_stored(candidate, fixture.now_unix_ms, None)
                        .expect("shadow profile");
                profiles.push(profile);
                scheduler_items.push(scheduler_item);
            }

            let scheduler_budget = SchedulerBudgetProfile {
                max_items: fixture.budget_profile.max_items.max(0),
                max_bytes: fixture.budget_profile.max_bytes.max(0),
                max_duration_ms: None,
                durable_cargo_ratio_cap: None,
                preferred_transfer_unit_bytes: 32_768,
                expiry_urgency_horizon_ms: 900_000,
                stagnation_horizon_ms: 3_600_000,
                target_replica_count_default: 6,
            };
            let scheduler_result = EncounterSchedulerV1::new()
                .schedule(
                    EncounterClass::Short,
                    &scheduler_budget,
                    fixture.now_unix_ms,
                    &scheduler_items,
                )
                .expect("scheduler result");

            let telemetry = transfer_legacy_debug::build_shadow_comparison_telemetry(
                &profiles,
                &legacy,
                &scheduler_result,
                5,
            );
            assert!(!telemetry.old_top_n.is_empty() || !telemetry.new_top_n.is_empty());
            assert_eq!(
                telemetry.old_top_n.len(),
                std::cmp::min(5, legacy.ranking_order.len())
            );
        }
    }

    #[test]
    fn shadow_comparison_detects_changed_first_item_and_stop_reason() {
        let now_ms = 1_760_000_000_000u64;
        let direct_peer = item(0x99);
        let far_peer = item(0x66);
        let small_transit_payload = crate::aethos_core::protocol::build_envelope_payload_b64(
            &far_peer, b"tiny", &[1u8; 32],
        )
        .expect("build small payload");
        let direct_tiny_payload = crate::aethos_core::protocol::build_envelope_payload_b64(
            &direct_peer,
            &vec![0x62; 1000],
            &[2u8; 32],
        )
        .expect("build direct tiny payload");

        let transit_bytes_len = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&small_transit_payload)
            .expect("decode transit payload")
            .len() as u64;
        let direct_bytes_len = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&direct_tiny_payload)
            .expect("decode direct payload")
            .len() as u64;
        let max_bytes = direct_bytes_len.saturating_add(1);
        assert!(transit_bytes_len.saturating_add(direct_bytes_len) > max_bytes);

        let candidates = vec![
            StoredItemRecord {
                item_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1"
                    .to_string(),
                envelope_b64: small_transit_payload,
                expiry_unix_ms: now_ms + 120_000,
                hop_count: 0,
                recorded_at_unix_ms: now_ms - 10_000,
            },
            StoredItemRecord {
                item_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb1"
                    .to_string(),
                envelope_b64: direct_tiny_payload,
                expiry_unix_ms: now_ms + 60_000,
                hop_count: 0,
                recorded_at_unix_ms: now_ms - 20_000,
            },
        ];

        let legacy = transfer_legacy_debug::build_legacy_transfer_plan(&candidates, 2, max_bytes)
            .expect("legacy plan");
        let mut profiles = Vec::new();
        let mut scheduler_items = Vec::new();
        for candidate in &candidates {
            let (profile, scheduler_item) =
                shadow_profile_from_stored(candidate, now_ms, Some(&direct_peer))
                    .expect("shadow profile");
            profiles.push(profile);
            scheduler_items.push(scheduler_item);
        }

        let scheduler_result = EncounterSchedulerV1::new()
            .schedule(
                EncounterClass::Short,
                &SchedulerBudgetProfile {
                    max_items: 2,
                    max_bytes: max_bytes as i32,
                    max_duration_ms: None,
                    durable_cargo_ratio_cap: None,
                    preferred_transfer_unit_bytes: 32_768,
                    expiry_urgency_horizon_ms: 900_000,
                    stagnation_horizon_ms: 3_600_000,
                    target_replica_count_default: 6,
                },
                now_ms,
                &scheduler_items,
            )
            .expect("scheduler result");

        let telemetry = transfer_legacy_debug::build_shadow_comparison_telemetry(
            &profiles,
            &legacy,
            &scheduler_result,
            5,
        );
        assert!(telemetry.changed_first_selected_item);
        assert!(telemetry.changed_stop_reason || telemetry.old_top_n != telemetry.new_top_n);
    }
}
