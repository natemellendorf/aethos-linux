use std::collections::HashMap;
use std::io::Read;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use base64::Engine;
use tungstenite::{client, Message};
use url::Url;

use crate::aethos_core::gossip_sync::{
    build_hello_frame, build_relay_ingest_frame, build_request_frame, build_summary_frame,
    has_item, import_transfer_items, parse_frame,
    select_request_item_ids_from_summary_with_candidates, serialize_frame,
    transfer_items_for_request, GossipSyncFrame, HelloFrame, RelayIngestFrame,
};
use crate::aethos_core::identity_store::LocalIdentitySummary;
use crate::aethos_core::logging::log_verbose;
use crate::aethos_core::protocol::decode_envelope_payload_utf8_preview;

pub const RELAY_CONNECT_TIMEOUT_SECS: u64 = 5;

pub fn normalize_http_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim();
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("ws://")
        || trimmed.starts_with("wss://")
    {
        return trimmed.to_string();
    }
    format!("http://{trimmed}")
}

pub fn to_ws_endpoint(http_like: &str) -> String {
    let normalized = normalize_http_endpoint(http_like);
    let Ok(mut url) = Url::parse(&normalized) else {
        return http_like.to_string();
    };

    let next_scheme = match url.scheme() {
        "http" => Some("ws"),
        "https" => Some("wss"),
        "ws" | "wss" => None,
        _ => return http_like.to_string(),
    };

    if let Some(next_scheme) = next_scheme {
        if url.set_scheme(next_scheme).is_err() {
            return http_like.to_string();
        }
    }

    if url.path().is_empty() || url.path() == "/" {
        url.set_path("/ws");
    }
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

pub fn connect_to_relay_gossipv1(relay_ws: &str, identity: &LocalIdentitySummary) -> String {
    connect_to_relay_gossipv1_with_auth(relay_ws, identity, None)
}

pub fn connect_to_relay_gossipv1_with_auth(
    relay_ws: &str,
    identity: &LocalIdentitySummary,
    auth_token: Option<&str>,
) -> String {
    match open_relay_socket(relay_ws, auth_token)
        .and_then(|mut socket| complete_hello_handshake(&mut socket, identity))
    {
        Ok(peer_hello) => format!(
            "connected + HELLO(version={}, peer={})",
            peer_hello.version,
            &peer_hello.node_id[..12]
        ),
        Err(err) => err,
    }
}

#[derive(Debug, Clone)]
pub struct EncounterReport {
    pub transferred_items: usize,
    pub pulled_messages: Vec<EncounterMessagePreview>,
}

#[derive(Debug, Clone)]
pub struct EncounterMessagePreview {
    pub author_wayfarer_id: Option<String>,
    pub session_peer: Option<String>,
    pub transport_peer: Option<String>,
    pub item_id: String,
    pub text: String,
    pub received_at_unix: i64,
    pub manifest_id_hex: Option<String>,
}

pub fn run_relay_encounter_gossipv1(
    relay_ws: &str,
    identity: &LocalIdentitySummary,
    auth_token: Option<&str>,
    trace_item_id: Option<&str>,
) -> Result<EncounterReport, String> {
    log_verbose(&format!(
        "relay_encounter_open: relay_ws={} wayfarer={} auth={} trace_item_id={}",
        relay_ws,
        identity.wayfarer_id,
        auth_token.is_some(),
        trace_item_id.unwrap_or("none")
    ));
    let mut socket = open_relay_socket(relay_ws, auth_token)?;
    let peer_hello = complete_hello_handshake(&mut socket, identity)?;
    log_verbose(&format!(
        "relay_encounter_hello_ok: relay_ws={} peer_node={} peer_max_want={} peer_max_transfer={}",
        relay_ws, peer_hello.node_id, peer_hello.max_want, peer_hello.max_transfer
    ));

    send_binary_frame(&mut socket, &build_summary_frame(now_unix_ms())?)?;
    send_binary_frame(&mut socket, &build_relay_ingest_frame(now_unix_ms())?)?;

    let mut transferred_items = 0usize;
    let mut pulled_messages = Vec::new();
    let mut latest_summary: Option<crate::aethos_core::gossip_sync::SummaryFrame> = None;
    let mut relay_ingest_candidates = Vec::<String>::new();
    let relay_ingest_trusted = auth_token.is_some();
    let deadline = Instant::now() + Duration::from_secs(3);

    while Instant::now() <= deadline {
        let frame = match read_binary_frame(&mut socket) {
            Ok(frame) => frame,
            Err(err) if is_nonfatal_read_timeout(&err) => break,
            Err(err) => return Err(err),
        };

        match frame {
            GossipSyncFrame::Summary(summary) => {
                log_verbose(&format!(
                    "relay_encounter_recv_summary: relay_ws={} item_count={} bloom_bytes={}",
                    relay_ws,
                    summary.item_count,
                    summary.bloom_filter.len()
                ));
                latest_summary = Some(summary.clone());
                let want = select_request_item_ids_from_summary_with_candidates(
                    &summary,
                    peer_hello.max_want as usize,
                    &relay_ingest_candidates,
                )?;
                send_binary_frame(
                    &mut socket,
                    &build_request_frame(want, peer_hello.max_want as usize)?,
                )?;
            }
            GossipSyncFrame::RelayIngest(RelayIngestFrame { item_ids }) => {
                if let Some(trace_item_id) = trace_item_id {
                    log_verbose(&format!(
                        "relay_trace_relay_ingest_contains_item: relay_ws={} item_id={} seen_in_relay_ingest={}",
                        relay_ws,
                        trace_item_id,
                        item_ids.iter().any(|id| id == trace_item_id)
                    ));
                }
                log_verbose(&format!(
                    "relay_encounter_recv_relay_ingest: relay_ws={} item_ids={}",
                    relay_ws,
                    item_ids.len()
                ));
                if !relay_ingest_trusted {
                    log_verbose(&format!(
                        "relay_encounter_ignore_untrusted_relay_ingest: relay_ws={relay_ws}"
                    ));
                    continue;
                }

                relay_ingest_candidates = item_ids
                    .into_iter()
                    .filter(|item_id| has_item(item_id).map(|have| !have).unwrap_or(false))
                    .collect::<Vec<_>>();
                relay_ingest_candidates.sort();
                relay_ingest_candidates.dedup();

                if let Some(summary) = latest_summary.as_ref() {
                    let want = select_request_item_ids_from_summary_with_candidates(
                        summary,
                        peer_hello.max_want as usize,
                        &relay_ingest_candidates,
                    )?;
                    send_binary_frame(
                        &mut socket,
                        &build_request_frame(want, peer_hello.max_want as usize)?,
                    )?;
                }
            }
            GossipSyncFrame::Request(request) => {
                if let Some(trace_item_id) = trace_item_id {
                    log_verbose(&format!(
                        "relay_trace_request_contains_item: relay_ws={} item_id={} requested_by_peer={}",
                        relay_ws,
                        trace_item_id,
                        request.want.iter().any(|id| id == trace_item_id)
                    ));
                }
                log_verbose(&format!(
                    "relay_encounter_recv_request: relay_ws={} want_items={}",
                    relay_ws,
                    request.want.len()
                ));
                let objects = transfer_items_for_request(
                    &request.want,
                    peer_hello.max_transfer as u32,
                    crate::aethos_core::gossip_sync::MAX_TRANSFER_BYTES,
                    now_unix_ms(),
                )?;
                let trace_sent_in_transfer = trace_item_id.map(|item_id| {
                    objects
                        .iter()
                        .any(|object| object.item_id.as_str() == item_id)
                });
                transferred_items += objects.len();
                send_binary_frame(
                    &mut socket,
                    &GossipSyncFrame::Transfer(crate::aethos_core::gossip_sync::TransferFrame {
                        objects,
                    }),
                )?;
                if let Some(trace_item_id) = trace_item_id {
                    log_verbose(&format!(
                        "relay_trace_transfer_contains_item: relay_ws={} item_id={} sent_in_transfer={}",
                        relay_ws,
                        trace_item_id,
                        trace_sent_in_transfer.unwrap_or(false)
                    ));
                }
            }
            GossipSyncFrame::Transfer(transfer) => {
                log_verbose(&format!(
                    "relay_encounter_recv_transfer: relay_ws={} objects={}",
                    relay_ws,
                    transfer.objects.len()
                ));
                let imported = import_transfer_items(
                    &identity.wayfarer_id,
                    Some(relay_ws),
                    Some(&peer_hello.node_id),
                    &transfer.objects,
                    now_unix_ms(),
                )?;

                let received = imported.accepted_item_ids.clone();
                send_binary_frame(
                    &mut socket,
                    &GossipSyncFrame::Receipt(crate::aethos_core::gossip_sync::ReceiptFrame {
                        received,
                    }),
                )?;
                log_verbose(&format!(
                    "relay_encounter_send_receipt: relay_ws={} received_items={} new_messages={}",
                    relay_ws,
                    imported.accepted_item_ids.len(),
                    imported.new_messages.len()
                ));

                for message in imported.new_messages {
                    let item_id = message.item_id.clone();
                    pulled_messages.push(EncounterMessagePreview {
                        author_wayfarer_id: message.author_wayfarer_id,
                        session_peer: message.session_peer,
                        transport_peer: message.transport_peer,
                        item_id: item_id.clone(),
                        text: decode_envelope_payload_utf8_preview(
                            &transfer
                                .objects
                                .iter()
                                .find(|o| o.item_id == item_id)
                                .map(|o| o.envelope_b64.clone())
                                .unwrap_or_default(),
                        )
                        .unwrap_or(message.text),
                        received_at_unix: message.received_at_unix,
                        manifest_id_hex: message.manifest_id_hex,
                    });
                }
            }
            GossipSyncFrame::Receipt(receipt) => {
                log_verbose(&format!(
                    "relay_encounter_recv_receipt: relay_ws={} received_items={}",
                    relay_ws,
                    receipt.received.len()
                ));
            }
            GossipSyncFrame::Hello(peer) => {
                log_verbose(&format!(
                    "relay_encounter_recv_hello_midstream: relay_ws={} peer_node={}",
                    relay_ws, peer.node_id
                ));
            }
        }
    }

    log_verbose(&format!(
        "relay_encounter_done: relay_ws={} transferred_items={} pulled_messages={}",
        relay_ws,
        transferred_items,
        pulled_messages.len()
    ));
    Ok(EncounterReport {
        transferred_items,
        pulled_messages,
    })
}

fn is_nonfatal_read_timeout(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("wouldblock")
        || lower.contains("timed out")
        || lower.contains("resource temporarily unavailable")
        || lower.contains("os error 11")
}

fn open_relay_socket(
    relay_ws: &str,
    auth_token: Option<&str>,
) -> Result<tungstenite::WebSocket<TcpStream>, String> {
    let Ok(url) = Url::parse(relay_ws) else {
        return Err("invalid relay URL".to_string());
    };
    let Some(host) = url.host_str() else {
        return Err("relay URL missing host".to_string());
    };

    let port = url.port_or_known_default().unwrap_or(80);
    let socket_addr = format!("{host}:{port}");
    log_verbose(&format!(
        "relay_socket_connecting: relay_ws={} addr={}",
        relay_ws, socket_addr
    ));
    let timeout = Duration::from_secs(RELAY_CONNECT_TIMEOUT_SECS);

    let Ok(stream) = TcpStream::connect_timeout(
        &socket_addr
            .parse()
            .map_err(|err| format!("invalid socket address: {err}"))?,
        timeout,
    ) else {
        return Err(format!("tcp connection timeout/failure to {socket_addr}"));
    };

    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let mut request = tungstenite::client::IntoClientRequest::into_client_request(relay_ws)
        .map_err(|err| format!("invalid websocket request: {err}"))?;

    if let Some(token) = auth_token {
        let header_value = format!("Bearer {token}")
            .parse()
            .map_err(|err| format!("invalid auth token header: {err}"))?;
        request.headers_mut().insert("Authorization", header_value);
    }

    client(request, stream)
        .map(|(socket, _)| socket)
        .map_err(|err| format!("websocket handshake failed: {err}"))
}

fn complete_hello_handshake(
    socket: &mut tungstenite::WebSocket<TcpStream>,
    identity: &LocalIdentitySummary,
) -> Result<HelloFrame, String> {
    let node_pubkey_raw = base64::engine::general_purpose::STANDARD
        .decode(&identity.verifying_key_b64)
        .map_err(|err| format!("failed decoding local pubkey: {err}"))?;
    let node_pubkey = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(node_pubkey_raw);
    let hello = build_hello_frame(&identity.wayfarer_id, &node_pubkey)?;
    log_verbose(&format!(
        "relay_handshake_send_hello: node_id={} device_id={}...",
        identity.wayfarer_id,
        &identity.device_id.chars().take(8).collect::<String>()
    ));
    send_binary_frame(socket, &hello)?;

    let frame = read_binary_frame(socket)?;
    match frame {
        GossipSyncFrame::Hello(peer) => Ok(peer),
        other => Err(format!(
            "connected; unexpected first frame after HELLO: {other:?}"
        )),
    }
}

fn send_binary_frame(
    socket: &mut tungstenite::WebSocket<TcpStream>,
    frame: &GossipSyncFrame,
) -> Result<(), String> {
    let raw = serialize_frame(frame)?;
    let framed = encode_stream_frame(&raw)?;
    log_verbose(&format!(
        "relay_frame_send: type={} payload_bytes={} framed_bytes={}",
        relay_frame_type(frame),
        raw.len(),
        framed.len()
    ));
    socket
        .send(Message::Binary(framed))
        .map_err(|err| format!("websocket send failed: {err}"))
}

fn read_binary_frame(
    socket: &mut tungstenite::WebSocket<TcpStream>,
) -> Result<GossipSyncFrame, String> {
    match socket.read() {
        Ok(Message::Binary(raw)) => parse_relay_binary_message(&raw),
        Ok(Message::Ping(payload)) => {
            let _ = socket.send(Message::Pong(payload));
            Err("WouldBlock".to_string())
        }
        Ok(Message::Text(text)) => {
            let mut cursor = std::io::Cursor::new(text.as_bytes());
            let mut raw = Vec::new();
            cursor
                .read_to_end(&mut raw)
                .map_err(|err| format!("text frame read failed: {err}"))?;
            parse_frame(&raw)
        }
        Ok(other) => Err(format!("unexpected relay frame: {other:?}")),
        Err(err) => Err(format!("websocket read failed: {err}")),
    }
}

fn parse_relay_binary_message(raw: &[u8]) -> Result<GossipSyncFrame, String> {
    match decode_stream_frame(raw) {
        Ok(payload) => {
            log_verbose(&format!(
                "relay_frame_recv_binary: framing=length-prefixed framed_bytes={} payload_bytes={}",
                raw.len(),
                payload.len()
            ));
            parse_frame(payload)
        }
        Err(prefix_err) => {
            log_verbose(&format!(
                "relay_frame_recv_binary_unframed_attempt: framed_bytes={} reason={}",
                raw.len(),
                prefix_err
            ));
            match parse_frame(raw) {
                Ok(frame) => {
                    log_verbose(&format!(
                        "relay_frame_recv_binary: framing=raw-cbor bytes={}",
                        raw.len()
                    ));
                    Ok(frame)
                }
                Err(raw_err) => Err(format!(
                    "relay frame decode failed (length-prefixed: {prefix_err}; raw-cbor: {raw_err})"
                )),
            }
        }
    }
}

fn encode_stream_frame(payload: &[u8]) -> Result<Vec<u8>, String> {
    let len = u32::try_from(payload.len()).map_err(|_| "frame too large".to_string())?;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

fn decode_stream_frame(raw: &[u8]) -> Result<&[u8], String> {
    if raw.len() < 4 {
        return Err("gossip stream frame too short".to_string());
    }
    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&raw[..4]);
    let frame_len = u32::from_be_bytes(len_bytes) as usize;
    if frame_len > crate::aethos_core::gossip_sync::MAX_FRAME_BYTES {
        return Err(format!("gossip frame length exceeds max: {frame_len}"));
    }
    if raw.len() != frame_len + 4 {
        return Err(format!(
            "gossip frame length mismatch: expected {} bytes, got {}",
            frame_len + 4,
            raw.len()
        ));
    }
    Ok(&raw[4..])
}

fn relay_frame_type(frame: &GossipSyncFrame) -> &'static str {
    match frame {
        GossipSyncFrame::Hello(_) => "HELLO",
        GossipSyncFrame::Summary(_) => "SUMMARY",
        GossipSyncFrame::Request(_) => "REQUEST",
        GossipSyncFrame::Transfer(_) => "TRANSFER",
        GossipSyncFrame::Receipt(_) => "RECEIPT",
        GossipSyncFrame::RelayIngest(_) => "RELAY_INGEST",
    }
}

#[derive(Debug, Clone)]
pub struct RelaySessionConfig {
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    pub min_health_score: i32,
    pub max_health_score: i32,
}

impl Default for RelaySessionConfig {
    fn default() -> Self {
        Self {
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            min_health_score: -10,
            max_health_score: 10,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManagedRelayEndpoint {
    pub relay_http: String,
    pub relay_ws: String,
    pub auth_token: Option<String>,
    pub health_score: i32,
    pub consecutive_failures: u32,
    pub next_attempt_at: Instant,
}

#[derive(Debug, Clone)]
pub struct RelaySelection {
    pub relay_slot: usize,
    pub relay_http: String,
    pub relay_ws: String,
    pub auth_token: Option<String>,
}

#[derive(Debug)]
pub struct RelaySessionManager {
    config: RelaySessionConfig,
    relays: Vec<ManagedRelayEndpoint>,
    rr_cursor: usize,
}

impl RelaySessionManager {
    pub fn new(relay_http_endpoints: Vec<String>, config: RelaySessionConfig) -> Self {
        let now = Instant::now();
        let relays = relay_http_endpoints
            .into_iter()
            .map(|relay_http| ManagedRelayEndpoint {
                relay_ws: to_ws_endpoint(&relay_http),
                relay_http,
                auth_token: None,
                health_score: 0,
                consecutive_failures: 0,
                next_attempt_at: now,
            })
            .collect();
        Self {
            config,
            relays,
            rr_cursor: 0,
        }
    }

    pub fn set_auth_token(&mut self, relay_slot: usize, auth_token: Option<String>) {
        if let Some(relay) = self.relays.get_mut(relay_slot) {
            relay.auth_token = auth_token;
        }
    }

    pub fn select_relay(&mut self, now: Instant) -> Option<RelaySelection> {
        if self.relays.is_empty() {
            return None;
        }
        for offset in 0..self.relays.len() {
            let idx = (self.rr_cursor + offset) % self.relays.len();
            let relay = &self.relays[idx];
            if relay.next_attempt_at <= now {
                self.rr_cursor = (idx + 1) % self.relays.len();
                return Some(RelaySelection {
                    relay_slot: idx,
                    relay_http: relay.relay_http.clone(),
                    relay_ws: relay.relay_ws.clone(),
                    auth_token: relay.auth_token.clone(),
                });
            }
        }
        None
    }

    pub fn mark_success(&mut self, relay_slot: usize) {
        if let Some(relay) = self.relays.get_mut(relay_slot) {
            relay.consecutive_failures = 0;
            relay.health_score = (relay.health_score + 1).min(self.config.max_health_score);
            relay.next_attempt_at = Instant::now();
        }
    }

    pub fn mark_failure(&mut self, relay_slot: usize) {
        if let Some(relay) = self.relays.get_mut(relay_slot) {
            relay.consecutive_failures = relay.consecutive_failures.saturating_add(1);
            relay.health_score = (relay.health_score - 1).max(self.config.min_health_score);
            let exponential = self
                .config
                .base_backoff
                .checked_mul(2_u32.saturating_pow(relay.consecutive_failures.saturating_sub(1)))
                .unwrap_or(self.config.max_backoff);
            relay.next_attempt_at = Instant::now() + exponential.min(self.config.max_backoff);
        }
    }

    pub fn relays(&self) -> &[ManagedRelayEndpoint] {
        &self.relays
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RelayFrame {
    pub correlation_id: String,
    pub message_type: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct DispatchedResponse {
    pub correlation_id: String,
    pub request_message_type: String,
    pub response_message_type: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatcherError {
    UnknownCorrelationId,
}

#[derive(Debug, Default)]
pub struct RelayRequestDispatcher {
    next_id: u64,
    pending: HashMap<String, String>,
}

impl RelayRequestDispatcher {
    pub fn register_outbound(
        &mut self,
        message_type: impl Into<String>,
        payload: serde_json::Value,
    ) -> RelayFrame {
        self.next_id = self.next_id.saturating_add(1);
        let correlation_id = format!("linux-{}", self.next_id);
        let message_type = message_type.into();
        self.pending
            .insert(correlation_id.clone(), message_type.clone());
        RelayFrame {
            correlation_id,
            message_type,
            payload,
        }
    }

    pub fn resolve_response(
        &mut self,
        response: RelayFrame,
    ) -> Result<DispatchedResponse, DispatcherError> {
        let Some(request_message_type) = self.pending.remove(&response.correlation_id) else {
            return Err(DispatcherError::UnknownCorrelationId);
        };
        Ok(DispatchedResponse {
            correlation_id: response.correlation_id,
            request_message_type,
            response_message_type: response.message_type,
            payload: response.payload,
        })
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

fn now_unix_ms() -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(_) => 0,
    }
}
