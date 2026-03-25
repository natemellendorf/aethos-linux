use std::collections::HashMap;
use std::io::Read;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use sha2::{Digest, Sha256};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message};
use url::Url;

use crate::aethos_core::gossip_sync::{
    build_hello_frame, build_relay_ingest_frame, build_request_frame, build_summary_frame,
    import_transfer_items, missing_item_ids, parse_frame,
    select_request_item_ids_from_summary_with_candidates, serialize_frame,
    transfer_items_for_request, GossipSyncFrame, HelloFrame, RelayIngestFrame,
};
use crate::aethos_core::identity_store::LocalIdentitySummary;
use crate::aethos_core::logging::log_verbose;
use crate::aethos_core::protocol::decode_envelope_payload_utf8_preview;

type RelaySocket = tungstenite::WebSocket<MaybeTlsStream<TcpStream>>;
static RUSTLS_PROVIDER_INIT: Once = Once::new();
static RELAY_SESSION_REGISTRY: OnceLock<Mutex<HashMap<String, ActiveRelaySession>>> =
    OnceLock::new();
static RELAY_INGEST_WINDOW_REGISTRY: OnceLock<Mutex<HashMap<String, RelayIngestWindowState>>> =
    OnceLock::new();
static RELAY_INGEST_WINDOW_LOCK_POISON_LOGGED: AtomicBool = AtomicBool::new(false);
static RELAY_ATTEMPT_COUNTER: AtomicU64 = AtomicU64::new(1);
const RELAY_INGEST_PROCESS_MAX_ITEMS: usize = 1024;
const UNIX_DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone)]
struct ActiveRelaySession {
    attempt_id: u64,
    trigger: &'static str,
    state: &'static str,
    since: Instant,
}

#[derive(Debug, Clone)]
struct RelayIngestWindowState {
    day: u64,
    round: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RelayIngestItemSelection {
    processed_item_ids: Vec<String>,
    input_item_ids: usize,
    unique_item_ids: usize,
    deduped_item_ids: usize,
    truncated_item_ids: usize,
    window_offset: usize,
    window_round: u64,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RelaySessionSnapshot {
    pub attempt_id: u64,
    pub trigger: String,
    pub state: String,
    pub age_ms: u128,
}

#[derive(Debug)]
struct RelaySessionLease {
    key: String,
    attempt_id: u64,
    trigger: &'static str,
}

impl RelaySessionLease {
    fn acquire(relay_ws: &str, wayfarer_id: &str, trigger: &'static str) -> Result<Self, String> {
        let key = relay_session_key(relay_ws, wayfarer_id);
        let mut registry = relay_session_registry()
            .lock()
            .map_err(|_| "relay session registry poisoned".to_string())?;

        if let Some(active) = registry.get(&key) {
            return Err(format!(
                "session already active (attempt_id={} state={} trigger={} age_ms={})",
                active.attempt_id,
                active.state,
                active.trigger,
                active.since.elapsed().as_millis()
            ));
        }

        let attempt_id = RELAY_ATTEMPT_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        registry.insert(
            key.clone(),
            ActiveRelaySession {
                attempt_id,
                trigger,
                state: "connecting",
                since: Instant::now(),
            },
        );
        log_verbose(&format!(
            "relay_session_state: key={} attempt_id={} state=connecting prev=idle trigger={}",
            key, attempt_id, trigger
        ));

        Ok(Self {
            key,
            attempt_id,
            trigger,
        })
    }

    fn transition(&self, next_state: &'static str, event: &'static str) {
        let Ok(mut registry) = relay_session_registry().lock() else {
            return;
        };

        let Some(active) = registry.get_mut(&self.key) else {
            log_verbose(&format!(
                "relay_session_state_ignored: key={} attempt_id={} event={} reason=missing",
                self.key, self.attempt_id, event
            ));
            return;
        };

        if active.attempt_id != self.attempt_id {
            log_verbose(&format!(
                "relay_session_state_ignored: key={} attempt_id={} event={} reason=stale active_attempt_id={}",
                self.key, self.attempt_id, event, active.attempt_id
            ));
            return;
        }

        let prev = active.state;
        active.state = next_state;
        log_verbose(&format!(
            "relay_session_state: key={} attempt_id={} state={} prev={} trigger={} event={}",
            self.key, self.attempt_id, next_state, prev, self.trigger, event
        ));
    }
}

impl Drop for RelaySessionLease {
    fn drop(&mut self) {
        let Ok(mut registry) = relay_session_registry().lock() else {
            return;
        };

        let Some(active) = registry.get(&self.key) else {
            return;
        };

        if active.attempt_id != self.attempt_id {
            return;
        }

        let prev = active.state;
        let age_ms = active.since.elapsed().as_millis();
        registry.remove(&self.key);
        log_verbose(&format!(
            "relay_session_state: key={} attempt_id={} state=closed prev={} trigger={} event=lease_dropped age_ms={}",
            self.key, self.attempt_id, prev, self.trigger, age_ms
        ));
    }
}

fn relay_session_registry() -> &'static Mutex<HashMap<String, ActiveRelaySession>> {
    RELAY_SESSION_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn relay_ingest_window_registry() -> &'static Mutex<HashMap<String, RelayIngestWindowState>> {
    RELAY_INGEST_WINDOW_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn relay_session_key(relay_ws: &str, wayfarer_id: &str) -> String {
    format!("{}::{}", relay_ws, wayfarer_id)
}

#[allow(dead_code)]
pub fn relay_session_snapshot(relay_ws: &str, wayfarer_id: &str) -> Option<RelaySessionSnapshot> {
    let key = relay_session_key(relay_ws, wayfarer_id);
    let Ok(registry) = relay_session_registry().lock() else {
        return None;
    };
    let active = registry.get(&key)?;
    Some(RelaySessionSnapshot {
        attempt_id: active.attempt_id,
        trigger: active.trigger.to_string(),
        state: active.state.to_string(),
        age_ms: active.since.elapsed().as_millis(),
    })
}

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
    let lease = match RelaySessionLease::acquire(relay_ws, &identity.wayfarer_id, "connect_probe") {
        Ok(lease) => lease,
        Err(err) => {
            log_verbose(&format!(
                "relay_connect_probe_skipped: relay_ws={} wayfarer={} reason={}",
                relay_ws, identity.wayfarer_id, err
            ));
            return format!("skipped: {err}");
        }
    };

    let mut socket = match open_relay_socket(relay_ws, auth_token) {
        Ok(socket) => {
            lease.transition("connected", "socket_connected");
            socket
        }
        Err(err) => {
            lease.transition("closing", "connect_failed");
            return err;
        }
    };

    lease.transition("hello_sent", "hello_dispatch");
    match complete_hello_handshake(&mut socket, identity) {
        Ok(peer_hello) => {
            lease.transition("active", "hello_validated");
            lease.transition("closing", "connect_probe_complete");
            graceful_close_socket(
                &mut socket,
                relay_ws,
                lease.attempt_id,
                "probe_complete",
                "connect_to_relay_gossipv1_with_auth",
            );
            format!(
                "connected + HELLO(version={}, peer={})",
                peer_hello.version,
                &peer_hello.node_id[..12]
            )
        }
        Err(err) => {
            lease.transition("closing", "hello_failed");
            graceful_close_socket(
                &mut socket,
                relay_ws,
                lease.attempt_id,
                "hello_failed",
                "connect_to_relay_gossipv1_with_auth",
            );
            err
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncounterReport {
    pub transferred_items: usize,
    pub pulled_messages: Vec<EncounterMessagePreview>,
    pub trace_requested_by_peer: bool,
    pub trace_receipted_by_peer: bool,
    #[allow(dead_code)]
    pub remote_closed: bool,
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

#[allow(dead_code)]
pub struct RelayPersistentSession {
    relay_ws: String,
    lease: RelaySessionLease,
    socket: RelaySocket,
    peer_hello: HelloFrame,
    relay_ingest_allowed: bool,
    last_heartbeat_at: Instant,
}

#[allow(dead_code)]
pub fn open_relay_persistent_session(
    relay_ws: &str,
    identity: &LocalIdentitySummary,
    auth_token: Option<&str>,
) -> Result<RelayPersistentSession, String> {
    let lease = RelaySessionLease::acquire(relay_ws, &identity.wayfarer_id, "persistent_encounter")
        .map_err(|err| format!("relay encounter skipped: {err}"))?;
    let mut socket = match open_relay_socket(relay_ws, auth_token) {
        Ok(socket) => {
            lease.transition("connected", "socket_connected");
            socket
        }
        Err(err) => {
            lease.transition("closing", "connect_failed");
            return Err(err);
        }
    };
    lease.transition("hello_sent", "hello_dispatch");
    let peer_hello = match complete_hello_handshake(&mut socket, identity) {
        Ok(peer_hello) => {
            lease.transition("active", "hello_validated");
            peer_hello
        }
        Err(err) => {
            lease.transition("closing", "hello_failed");
            graceful_close_socket(
                &mut socket,
                relay_ws,
                lease.attempt_id,
                "hello_failed",
                "open_relay_persistent_session",
            );
            return Err(err);
        }
    };

    log_verbose(&format!(
        "relay_session_opened: relay_ws={} attempt_id={} peer_node={} mode=persistent",
        relay_ws, lease.attempt_id, peer_hello.node_id
    ));

    Ok(RelayPersistentSession {
        relay_ws: relay_ws.to_string(),
        lease,
        socket,
        peer_hello,
        relay_ingest_allowed: is_relay_ingest_allowed(relay_ws),
        last_heartbeat_at: Instant::now(),
    })
}

#[allow(dead_code)]
pub fn run_relay_round_on_persistent_session(
    session: &mut RelayPersistentSession,
    identity: &LocalIdentitySummary,
    trace_item_id: Option<&str>,
    encounter_window: Duration,
) -> Result<EncounterReport, String> {
    run_relay_round_on_socket(
        &mut session.socket,
        &session.relay_ws,
        identity,
        &session.peer_hello,
        session.relay_ingest_allowed,
        trace_item_id,
        encounter_window,
    )
}

#[allow(dead_code)]
pub fn maybe_send_relay_heartbeat(session: &mut RelayPersistentSession) -> Result<bool, String> {
    if session.last_heartbeat_at.elapsed() < Duration::from_secs(20) {
        return Ok(false);
    }
    session
        .socket
        .send(Message::Ping(Vec::new()))
        .map_err(|err| format!("relay heartbeat ping failed: {err}"))?;
    session.last_heartbeat_at = Instant::now();
    log_verbose(&format!(
        "relay_session_heartbeat_sent: relay_ws={} attempt_id={}",
        session.relay_ws, session.lease.attempt_id
    ));
    Ok(true)
}

#[allow(dead_code)]
pub fn close_relay_persistent_session(mut session: RelayPersistentSession, reason: &str) {
    session.lease.transition("closing", "persistent_close");
    graceful_close_socket(
        &mut session.socket,
        &session.relay_ws,
        session.lease.attempt_id,
        reason,
        "close_relay_persistent_session",
    );
    log_verbose(&format!(
        "relay_session_closed: relay_ws={} attempt_id={} reason={} mode=persistent",
        session.relay_ws, session.lease.attempt_id, reason
    ));
}

pub fn run_relay_encounter_gossipv1(
    relay_ws: &str,
    identity: &LocalIdentitySummary,
    auth_token: Option<&str>,
    trace_item_id: Option<&str>,
) -> Result<EncounterReport, String> {
    run_relay_encounter_gossipv1_for_duration(
        relay_ws,
        identity,
        auth_token,
        trace_item_id,
        Duration::from_secs(3),
    )
}

pub fn run_relay_encounter_gossipv1_for_duration(
    relay_ws: &str,
    identity: &LocalIdentitySummary,
    auth_token: Option<&str>,
    trace_item_id: Option<&str>,
    encounter_window: Duration,
) -> Result<EncounterReport, String> {
    let lease = RelaySessionLease::acquire(relay_ws, &identity.wayfarer_id, "encounter")
        .map_err(|err| format!("relay encounter skipped: {err}"))?;
    let relay_ingest_secure_transport = is_relay_ingest_secure_transport(relay_ws);
    let relay_ingest_allowed = is_relay_ingest_allowed(relay_ws);
    log_verbose(&format!(
        "relay_encounter_open: relay_ws={} wayfarer={} auth={} relay_ingest_allowed={} relay_ingest_secure_transport={} trace_item_id={}",
        relay_ws,
        identity.wayfarer_id,
        auth_token.is_some(),
        relay_ingest_allowed,
        relay_ingest_secure_transport,
        trace_item_id.unwrap_or("none")
    ));
    let mut socket = match open_relay_socket(relay_ws, auth_token) {
        Ok(socket) => {
            lease.transition("connected", "socket_connected");
            socket
        }
        Err(err) => {
            lease.transition("closing", "connect_failed");
            return Err(err);
        }
    };
    lease.transition("hello_sent", "hello_dispatch");
    let peer_hello = match complete_hello_handshake(&mut socket, identity) {
        Ok(peer_hello) => {
            lease.transition("active", "hello_validated");
            peer_hello
        }
        Err(err) => {
            lease.transition("closing", "hello_failed");
            graceful_close_socket(
                &mut socket,
                relay_ws,
                lease.attempt_id,
                "hello_failed",
                "run_relay_encounter_gossipv1",
            );
            return Err(err);
        }
    };
    log_verbose(&format!(
        "relay_encounter_hello_ok: relay_ws={} peer_node={} peer_max_want={} peer_max_transfer={}",
        relay_ws, peer_hello.node_id, peer_hello.max_want, peer_hello.max_transfer
    ));

    let report = match run_relay_round_on_socket(
        &mut socket,
        relay_ws,
        identity,
        &peer_hello,
        relay_ingest_allowed,
        trace_item_id,
        encounter_window,
    ) {
        Ok(report) => report,
        Err(err) => {
            lease.transition("closing", "read_failed");
            graceful_close_socket(
                &mut socket,
                relay_ws,
                lease.attempt_id,
                "read_failed",
                "run_relay_encounter_gossipv1",
            );
            return Err(err);
        }
    };

    lease.transition("closing", "encounter_complete");
    graceful_close_socket(
        &mut socket,
        relay_ws,
        lease.attempt_id,
        "encounter_complete",
        "run_relay_encounter_gossipv1",
    );
    Ok(report)
}

fn run_relay_round_on_socket(
    socket: &mut RelaySocket,
    relay_ws: &str,
    identity: &LocalIdentitySummary,
    peer_hello: &HelloFrame,
    relay_ingest_allowed: bool,
    trace_item_id: Option<&str>,
    encounter_window: Duration,
) -> Result<EncounterReport, String> {
    log_verbose(&format!(
        "relay_encounter_post_hello_send_summary: relay_ws={}",
        relay_ws
    ));
    send_binary_frame(socket, &build_summary_frame(now_unix_ms())?)?;
    log_verbose(&format!(
        "relay_encounter_post_hello_send_relay_ingest: relay_ws={}",
        relay_ws
    ));
    send_binary_frame(socket, &build_relay_ingest_frame(now_unix_ms())?)?;

    let mut transferred_items = 0usize;
    let mut pulled_messages = Vec::new();
    let mut latest_summary: Option<crate::aethos_core::gossip_sync::SummaryFrame> = None;
    let mut relay_ingest_candidates = Vec::<String>::new();
    let deadline = Instant::now() + encounter_window.max(Duration::from_millis(250));
    let started_at = Instant::now();
    let mut recv_frame_count = 0usize;
    let mut saw_summary = false;
    let mut saw_transfer = false;
    let mut saw_request = false;
    let mut saw_relay_ingest = false;
    let mut saw_receipt = false;
    let mut trace_requested_by_peer = false;
    let mut trace_receipted_by_peer = false;
    let mut no_progress_streak = 0usize;
    let mut remote_closed = false;

    while Instant::now() <= deadline {
        let frame = match read_binary_frame(socket) {
            Ok(frame) => frame,
            Err(err) if is_nonfatal_read_timeout(&err) => {
                log_verbose(&format!(
                    "relay_encounter_loop_timeout: relay_ws={} elapsed_ms={} recv_frames={} saw_summary={} saw_request={} saw_transfer={} saw_relay_ingest={} saw_receipt={}",
                    relay_ws,
                    started_at.elapsed().as_millis(),
                    recv_frame_count,
                    saw_summary,
                    saw_request,
                    saw_transfer,
                    saw_relay_ingest,
                    saw_receipt
                ));
                break;
            }
            Err(err) => {
                if is_nonfatal_remote_close(&err) {
                    remote_closed = true;
                    log_verbose(&format!(
                        "relay_encounter_loop_remote_close: relay_ws={} elapsed_ms={} recv_frames={} error={}",
                        relay_ws,
                        started_at.elapsed().as_millis(),
                        recv_frame_count,
                        err
                    ));
                    break;
                }
                log_verbose(&format!(
                    "relay_encounter_loop_read_failed: relay_ws={} elapsed_ms={} recv_frames={} error={}",
                    relay_ws,
                    started_at.elapsed().as_millis(),
                    recv_frame_count,
                    err
                ));
                return Err(err);
            }
        };
        recv_frame_count = recv_frame_count.saturating_add(1);
        let mut made_progress = false;

        match frame {
            GossipSyncFrame::Summary(summary) => {
                saw_summary = true;
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
                made_progress = !want.is_empty();
                send_binary_frame(
                    socket,
                    &build_request_frame(want, peer_hello.max_want as usize)?,
                )?;
            }
            GossipSyncFrame::RelayIngest(RelayIngestFrame { item_ids }) => {
                saw_relay_ingest = true;
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

                if !relay_ingest_allowed {
                    log_verbose(&format!(
                        "relay_encounter_ignore_relay_ingest_insecure_transport: relay_ws={relay_ws}"
                    ));
                    continue;
                }

                let now_ms = now_unix_ms();
                let (next_candidates, selection) = relay_ingest_candidates_with_lookup(
                    item_ids,
                    relay_ws,
                    &identity.wayfarer_id,
                    now_ms,
                    missing_item_ids,
                );

                if selection.truncated_item_ids > 0 {
                    log_verbose(&format!(
                        "relay_encounter_relay_ingest_truncated: relay_ws={} relay_ingest_item_ids_truncated={} relay_ingest_item_ids_processed={} relay_ingest_item_ids_cap={} relay_ingest_item_ids_unique={} relay_ingest_item_ids_deduped={} relay_ingest_window_offset={} relay_ingest_window_round={}",
                        relay_ws,
                        selection.truncated_item_ids,
                        selection.processed_item_ids.len(),
                        RELAY_INGEST_PROCESS_MAX_ITEMS,
                        selection.unique_item_ids,
                        selection.deduped_item_ids,
                        selection.window_offset,
                        selection.window_round
                    ));
                }

                if selection.deduped_item_ids > 0 {
                    log_verbose(&format!(
                        "relay_encounter_relay_ingest_deduped: relay_ws={} relay_ingest_item_ids_raw={} relay_ingest_item_ids_unique={} relay_ingest_item_ids_deduped={}",
                        relay_ws,
                        selection.input_item_ids,
                        selection.unique_item_ids,
                        selection.deduped_item_ids
                    ));
                }

                if let Some(candidates) = next_candidates {
                    relay_ingest_candidates = candidates;
                }

                if let Some(summary) = latest_summary.as_ref() {
                    let want = select_request_item_ids_from_summary_with_candidates(
                        summary,
                        peer_hello.max_want as usize,
                        &relay_ingest_candidates,
                    )?;
                    made_progress = !want.is_empty();
                    send_binary_frame(
                        socket,
                        &build_request_frame(want, peer_hello.max_want as usize)?,
                    )?;
                }
            }
            GossipSyncFrame::Request(request) => {
                saw_request = true;
                if let Some(trace_item_id) = trace_item_id {
                    trace_requested_by_peer |= request.want.iter().any(|id| id == trace_item_id);
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
                made_progress = !objects.is_empty();
                send_binary_frame(
                    socket,
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
                saw_transfer = true;
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
                made_progress = !imported.accepted_item_ids.is_empty();

                let received = imported.accepted_item_ids.clone();
                send_binary_frame(
                    socket,
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
                saw_receipt = true;
                if let Some(trace_item_id) = trace_item_id {
                    trace_receipted_by_peer |= receipt
                        .received
                        .iter()
                        .any(|item_id| item_id == trace_item_id);
                }
                made_progress = !receipt.received.is_empty();
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

        if made_progress {
            no_progress_streak = 0;
        } else {
            no_progress_streak = no_progress_streak.saturating_add(1);
            if should_stop_for_no_progress(no_progress_streak) {
                log_verbose(&format!(
                    "relay_encounter_converged: relay_ws={} reason=no_progress_streak rounds={} recv_frames={}",
                    relay_ws, no_progress_streak, recv_frame_count
                ));
                break;
            }
        }
    }

    log_verbose(&format!(
        "relay_encounter_done: relay_ws={} transferred_items={} pulled_messages={} recv_frames={} saw_summary={} saw_request={} saw_transfer={} saw_relay_ingest={} saw_receipt={} elapsed_ms={}",
        relay_ws,
        transferred_items,
        pulled_messages.len(),
        recv_frame_count,
        saw_summary,
        saw_request,
        saw_transfer,
        saw_relay_ingest,
        saw_receipt,
        started_at.elapsed().as_millis()
    ));

    Ok(EncounterReport {
        transferred_items,
        pulled_messages,
        trace_requested_by_peer,
        trace_receipted_by_peer,
        remote_closed,
    })
}

fn should_stop_for_no_progress(streak: usize) -> bool {
    streak >= 2
}

fn relay_ingest_candidates_with_lookup<F>(
    item_ids: Vec<String>,
    relay_ws: &str,
    wayfarer_id: &str,
    now_ms: u64,
    lookup_missing: F,
) -> (Option<Vec<String>>, RelayIngestItemSelection)
where
    F: Fn(&[String]) -> Result<Vec<String>, String>,
{
    let selection =
        select_relay_ingest_item_ids_for_processing(item_ids, relay_ws, wayfarer_id, now_ms);
    let mut candidates = match lookup_missing(&selection.processed_item_ids) {
        Ok(candidates) => candidates,
        Err(err) => {
            // Rationale: preserve the previous relay_ingest_candidates on transient DB failures
            // so relay rounds continue with last-known-good state instead of collapsing to
            // an empty request set.
            log_verbose(&format!(
                "relay_encounter_relay_ingest_lookup_failed: relay_ws={} relay_ingest_item_ids_processed={} action=preserve_previous_candidates error={}",
                relay_ws,
                selection.processed_item_ids.len(),
                err
            ));
            return (None, selection);
        }
    };
    candidates.sort();
    candidates.dedup();
    (Some(candidates), selection)
}

fn select_relay_ingest_item_ids_for_processing(
    item_ids: Vec<String>,
    relay_ws: &str,
    wayfarer_id: &str,
    now_ms: u64,
) -> RelayIngestItemSelection {
    let input_item_ids = item_ids.len();
    let mut unique_sorted_item_ids = item_ids;
    unique_sorted_item_ids.sort();
    unique_sorted_item_ids.dedup();
    let unique_item_ids = unique_sorted_item_ids.len();
    let deduped_item_ids = input_item_ids.saturating_sub(unique_item_ids);

    if unique_item_ids <= RELAY_INGEST_PROCESS_MAX_ITEMS {
        return RelayIngestItemSelection {
            processed_item_ids: unique_sorted_item_ids,
            input_item_ids,
            unique_item_ids,
            deduped_item_ids,
            truncated_item_ids: 0,
            window_offset: 0,
            window_round: 0,
        };
    }

    let round = next_relay_ingest_window_round(relay_ws, wayfarer_id, now_ms);
    let day = unix_day(now_ms);
    let seed_offset = relay_ingest_seed_offset(relay_ws, wayfarer_id, day, unique_item_ids);
    let round_index = (round % unique_item_ids as u64) as usize;
    let round_stride = RELAY_INGEST_PROCESS_MAX_ITEMS;
    let round_offset = (round_index * round_stride) % unique_item_ids;
    let window_offset = (seed_offset + round_offset) % unique_item_ids;
    let processed_item_ids = select_relay_ingest_window_with_wraparound(
        &unique_sorted_item_ids,
        RELAY_INGEST_PROCESS_MAX_ITEMS,
        window_offset,
    );

    RelayIngestItemSelection {
        processed_item_ids,
        input_item_ids,
        unique_item_ids,
        deduped_item_ids,
        truncated_item_ids: unique_item_ids - RELAY_INGEST_PROCESS_MAX_ITEMS,
        window_offset,
        window_round: round,
    }
}

fn select_relay_ingest_window_with_wraparound(
    sorted_unique_item_ids: &[String],
    max_items: usize,
    window_offset: usize,
) -> Vec<String> {
    if sorted_unique_item_ids.len() <= max_items {
        return sorted_unique_item_ids.to_vec();
    }
    (0..max_items)
        .map(|idx| {
            let selected_idx = (window_offset + idx) % sorted_unique_item_ids.len();
            sorted_unique_item_ids[selected_idx].clone()
        })
        .collect()
}

fn next_relay_ingest_window_round(relay_ws: &str, wayfarer_id: &str, now_ms: u64) -> u64 {
    let key = relay_session_key(relay_ws, wayfarer_id);
    let day = unix_day(now_ms);
    let mut registry = match relay_ingest_window_registry().lock() {
        Ok(registry) => registry,
        Err(poison) => {
            if !RELAY_INGEST_WINDOW_LOCK_POISON_LOGGED.swap(true, AtomicOrdering::Relaxed) {
                log_verbose(
                    "relay_ingest_window_registry_lock_poisoned: action=recover_with_inner",
                );
            }
            poison.into_inner()
        }
    };

    let prior_len = registry.len();
    registry.retain(|_, state| state.day == day);
    let pruned_entries = prior_len.saturating_sub(registry.len());
    if pruned_entries > 0 {
        log_verbose(&format!(
            "relay_ingest_window_registry_pruned: removed_entries={} remaining_entries={} day={}",
            pruned_entries,
            registry.len(),
            day
        ));
    }

    let state = registry
        .entry(key)
        .or_insert(RelayIngestWindowState { day, round: 0 });
    if state.day != day {
        state.day = day;
        state.round = 0;
    }

    let round = state.round;
    state.round = state.round.saturating_add(1);
    round
}

fn relay_ingest_seed_offset(relay_ws: &str, wayfarer_id: &str, day: u64, modulo: usize) -> usize {
    if modulo == 0 {
        return 0;
    }

    let mut hasher = Sha256::new();
    hasher.update(relay_ws.as_bytes());
    hasher.update([0u8]);
    hasher.update(wayfarer_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(day.to_be_bytes());
    let digest = hasher.finalize();

    let mut seed_bytes = [0u8; 8];
    seed_bytes.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(seed_bytes) as usize) % modulo
}

fn unix_day(now_ms: u64) -> u64 {
    now_ms / UNIX_DAY_MS
}

fn is_relay_ingest_allowed(relay_ws: &str) -> bool {
    // Policy stub: RELAY_INGEST hints are only accepted over secure websocket transport.
    // No relay allowlist or bearer-token authenticity checks are implemented here.
    is_relay_ingest_secure_transport(relay_ws)
}

fn is_relay_ingest_secure_transport(relay_ws: &str) -> bool {
    Url::parse(relay_ws)
        .map(|url| url.scheme() == "wss")
        .unwrap_or(false)
}

fn is_nonfatal_read_timeout(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("wouldblock")
        || lower.contains("timed out")
        || lower.contains("resource temporarily unavailable")
        || lower.contains("os error 11")
}

fn is_nonfatal_remote_close(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("connection reset without closing handshake")
        || lower.contains("close 1005")
        || lower.contains("connection reset by peer")
        || lower.contains("broken pipe")
}

fn open_relay_socket(relay_ws: &str, auth_token: Option<&str>) -> Result<RelaySocket, String> {
    ensure_rustls_provider_installed();

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
    let mut request = tungstenite::client::IntoClientRequest::into_client_request(relay_ws)
        .map_err(|err| format!("invalid websocket request: {err}"))?;

    if let Some(token) = auth_token {
        let header_value = format!("Bearer {token}")
            .parse()
            .map_err(|err| format!("invalid auth token header: {err}"))?;
        request.headers_mut().insert("Authorization", header_value);
    }

    connect(request)
        .map(|(socket, _)| {
            log_verbose(&format!(
                "relay_socket_connected: relay_ws={} max_frame_bytes={} framing=ws-binary",
                relay_ws,
                crate::aethos_core::gossip_sync::MAX_FRAME_BYTES
            ));
            socket
        })
        .map_err(|err| format!("websocket handshake failed: {err}"))
}

fn ensure_rustls_provider_installed() {
    RUSTLS_PROVIDER_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn complete_hello_handshake(
    socket: &mut RelaySocket,
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

fn send_binary_frame(socket: &mut RelaySocket, frame: &GossipSyncFrame) -> Result<(), String> {
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

fn read_binary_frame(socket: &mut RelaySocket) -> Result<GossipSyncFrame, String> {
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
    log_verbose(&format!(
        "relay_frame_recv_binary_raw: ws_bytes={} max_frame_bytes={} prefix_hex={}",
        raw.len(),
        crate::aethos_core::gossip_sync::MAX_FRAME_BYTES,
        hex_prefix(raw, 16)
    ));

    match decode_stream_frame(raw) {
        Ok(payload) => {
            log_verbose(&format!(
                "relay_frame_recv_binary: framing=length-prefixed framed_bytes={} payload_bytes={}",
                raw.len(),
                payload.len()
            ));
            match parse_frame(payload) {
                Ok(frame) => Ok(frame),
                Err(payload_err) => {
                    log_verbose(&format!(
                        "relay_frame_parse_failed_prefixed_payload: framed_bytes={} payload_bytes={} error={}",
                        raw.len(),
                        payload.len(),
                        payload_err
                    ));
                    match parse_frame(raw) {
                        Ok(frame) => {
                            log_verbose(&format!(
                                "relay_frame_recv_binary: framing=raw-cbor-after-prefixed-failure bytes={}",
                                raw.len()
                            ));
                            Ok(frame)
                        }
                        Err(raw_err) => Err(format!(
                            "relay frame parse failed after prefixed decode (payload: {payload_err}; raw: {raw_err})"
                        )),
                    }
                }
            }
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

fn graceful_close_socket(
    socket: &mut RelaySocket,
    relay_ws: &str,
    attempt_id: u64,
    reason: &str,
    callsite: &str,
) {
    log_verbose(&format!(
        "relay_socket_close_initiated: relay_ws={} attempt_id={} reason={} callsite={}",
        relay_ws, attempt_id, reason, callsite
    ));
    match socket.send(Message::Close(None)) {
        Ok(_) => log_verbose(&format!(
            "relay_socket_close_sent: relay_ws={} attempt_id={} reason={}",
            relay_ws, attempt_id, reason
        )),
        Err(err) => log_verbose(&format!(
            "relay_socket_close_send_failed: relay_ws={} attempt_id={} reason={} error={}",
            relay_ws, attempt_id, reason, err
        )),
    }
}

fn hex_prefix(data: &[u8], count: usize) -> String {
    data.iter()
        .take(count)
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
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

#[cfg(test)]
mod tests {
    use super::{
        is_nonfatal_remote_close, is_relay_ingest_allowed, relay_ingest_candidates_with_lookup,
        select_relay_ingest_item_ids_for_processing, should_stop_for_no_progress,
        RELAY_INGEST_PROCESS_MAX_ITEMS,
    };

    #[test]
    fn nonfatal_remote_close_patterns_match_expected_errors() {
        assert!(is_nonfatal_remote_close(
            "websocket read failed: WebSocket protocol error: Connection reset without closing handshake"
        ));
        assert!(is_nonfatal_remote_close(
            "websocket read failed: websocket: close 1005 (no status)"
        ));
        assert!(is_nonfatal_remote_close(
            "io error: Connection reset by peer (os error 104)"
        ));
        assert!(is_nonfatal_remote_close(
            "write failed: Broken pipe (os error 32)"
        ));
        assert!(!is_nonfatal_remote_close(
            "websocket read failed: utf8 decode error"
        ));
    }

    #[test]
    fn no_progress_stop_cap_matches_encounter_policy() {
        assert!(!should_stop_for_no_progress(0));
        assert!(!should_stop_for_no_progress(1));
        assert!(should_stop_for_no_progress(2));
        assert!(should_stop_for_no_progress(3));
    }

    #[test]
    fn relay_ingest_policy_only_allows_secure_websocket_scheme() {
        assert!(is_relay_ingest_allowed("wss://relay.example/ws"));
        assert!(!is_relay_ingest_allowed("ws://relay.example/ws"));
        assert!(!is_relay_ingest_allowed("https://relay.example/ws"));
        assert!(!is_relay_ingest_allowed("not-a-url"));
    }

    #[test]
    fn relay_ingest_item_cap_truncates_large_frames() {
        let item_ids = (0..(RELAY_INGEST_PROCESS_MAX_ITEMS + 7))
            .map(|idx| format!("{:064x}", idx as u64))
            .collect::<Vec<_>>();

        let selection = select_relay_ingest_item_ids_for_processing(
            item_ids,
            "wss://relay.example/ws",
            "wayfarer-cap",
            1_700_000_000_000,
        );
        assert_eq!(
            selection.processed_item_ids.len(),
            RELAY_INGEST_PROCESS_MAX_ITEMS
        );
        assert_eq!(selection.truncated_item_ids, 7);
    }

    #[test]
    fn relay_ingest_item_cap_keeps_small_frames_intact() {
        let item_ids = (0..8)
            .map(|idx| format!("{:064x}", idx as u64))
            .collect::<Vec<_>>();

        let selection = select_relay_ingest_item_ids_for_processing(
            item_ids.clone(),
            "wss://relay.example/ws",
            "wayfarer-small",
            1_700_000_000_000,
        );
        assert_eq!(selection.processed_item_ids, item_ids);
        assert_eq!(selection.truncated_item_ids, 0);
    }

    #[test]
    fn relay_ingest_item_cap_applies_after_dedup_and_sort() {
        let unique = (0..RELAY_INGEST_PROCESS_MAX_ITEMS)
            .map(|idx| format!("{:064x}", idx as u64))
            .collect::<Vec<_>>();

        let mut item_ids = unique.clone();
        item_ids.extend(unique.iter().take(64).cloned());
        item_ids.reverse();

        let selection = select_relay_ingest_item_ids_for_processing(
            item_ids,
            "wss://relay.example/ws",
            "wayfarer-dedup",
            1_700_000_000_000,
        );

        assert_eq!(
            selection.processed_item_ids.len(),
            RELAY_INGEST_PROCESS_MAX_ITEMS
        );
        assert_eq!(selection.unique_item_ids, RELAY_INGEST_PROCESS_MAX_ITEMS);
        assert_eq!(selection.truncated_item_ids, 0);
        assert_eq!(selection.processed_item_ids, unique);
    }

    #[test]
    fn relay_ingest_window_rotates_across_rounds() {
        let item_ids = (0..(RELAY_INGEST_PROCESS_MAX_ITEMS + 11))
            .map(|idx| format!("{:064x}", idx as u64))
            .collect::<Vec<_>>();

        let first = select_relay_ingest_item_ids_for_processing(
            item_ids.clone(),
            "wss://relay-window.example/ws",
            "wayfarer-window",
            1_700_000_000_000,
        );
        let second = select_relay_ingest_item_ids_for_processing(
            item_ids,
            "wss://relay-window.example/ws",
            "wayfarer-window",
            1_700_000_000_000,
        );

        assert_eq!(
            first.processed_item_ids.len(),
            RELAY_INGEST_PROCESS_MAX_ITEMS
        );
        assert_eq!(
            second.processed_item_ids.len(),
            RELAY_INGEST_PROCESS_MAX_ITEMS
        );
        assert_ne!(first.processed_item_ids, second.processed_item_ids);
        assert_ne!(first.window_offset, second.window_offset);
    }

    #[test]
    fn relay_ingest_lookup_failure_preserves_previous_candidates_without_abort() {
        let item_ids = (0..16)
            .map(|idx| format!("{:064x}", idx as u64))
            .collect::<Vec<_>>();

        let (candidates, selection) = relay_ingest_candidates_with_lookup(
            item_ids,
            "wss://relay.example/ws",
            "wayfarer-lookup-fail",
            1_700_000_000_000,
            |_| Err("db unavailable".to_string()),
        );

        assert!(candidates.is_none());
        assert_eq!(selection.processed_item_ids.len(), 16);
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
