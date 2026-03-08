use std::collections::HashMap;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use tungstenite::{client, Message};
use url::Url;

use crate::aethos_core::protocol::{
    is_valid_device_id, is_valid_wayfarer_id, AckFrame, HelloFrame, MessageItem, PullFrame,
    RelayInboundFrame, SendFrame,
};

pub const RELAY_CONNECT_TIMEOUT_SECS: u64 = 5;

pub fn normalize_http_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
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
        "http" => "ws",
        "https" => "wss",
        _ => return http_like.to_string(),
    };

    if url.set_scheme(next_scheme).is_err() {
        return http_like.to_string();
    }

    url.set_path("/ws");
    url.set_query(None);
    url.set_fragment(None);

    url.to_string()
}

pub fn connect_to_relay(relay_ws: &str, wayfarer_id: &str, device_id: &str) -> String {
    connect_to_relay_with_auth(relay_ws, wayfarer_id, device_id, None)
}

pub fn connect_to_relay_with_auth(
    relay_ws: &str,
    wayfarer_id: &str,
    device_id: &str,
    auth_token: Option<&str>,
) -> String {
    let mut socket = match open_relay_socket(relay_ws, auth_token) {
        Ok(socket) => socket,
        Err(err) => return err,
    };

    match complete_hello_handshake(&mut socket, wayfarer_id, device_id) {
        Ok(relay_id) => match relay_id {
            Some(relay_id) => format!("connected + hello_ok ({relay_id})"),
            None => "connected + hello_ok".to_string(),
        },
        Err(err) => err,
    }
}

#[allow(dead_code)]
pub fn send_to_relay_v1(
    relay_ws: &str,
    wayfarer_id: &str,
    device_id: &str,
    to: &str,
    payload_b64: &str,
    client_msg_id: Option<&str>,
    ttl_seconds: Option<i64>,
) -> Result<(String, Option<i64>, Option<i64>), String> {
    send_to_relay_v1_with_auth(
        relay_ws,
        (wayfarer_id, device_id),
        to,
        payload_b64,
        client_msg_id,
        ttl_seconds,
        None,
    )
}

#[allow(dead_code)]
pub fn send_to_relay_v1_with_auth(
    relay_ws: &str,
    identity: (&str, &str),
    to: &str,
    payload_b64: &str,
    client_msg_id: Option<&str>,
    ttl_seconds: Option<i64>,
    auth_token: Option<&str>,
) -> Result<(String, Option<i64>, Option<i64>), String> {
    let (wayfarer_id, device_id) = identity;
    let frame = SendFrame::new(
        to,
        payload_b64,
        client_msg_id.map(ToString::to_string),
        ttl_seconds,
    )?;

    let mut socket = open_relay_socket(relay_ws, auth_token)?;
    complete_hello_handshake(&mut socket, wayfarer_id, device_id)?;
    send_json(&mut socket, &frame)?;

    let (frame, raw) = read_relay_frame(&mut socket)?;
    match frame {
        RelayInboundFrame::SendOk {
            msg_id,
            received_at,
            expires_at,
        } => Ok((msg_id, received_at, expires_at)),
        RelayInboundFrame::Message {
            msg_id,
            received_at,
            ..
        } => Ok((msg_id, Some(received_at), None)),
        RelayInboundFrame::Error { code, message } => Err(format!(
            "relay error {code} while sending: {message} (raw={raw})"
        )),
        other => Err(format!(
            "unexpected relay frame after send: {other:?} (raw={raw})"
        )),
    }
}

#[allow(dead_code)]
pub fn pull_from_relay_v1(
    relay_ws: &str,
    wayfarer_id: &str,
    device_id: &str,
    limit: Option<i64>,
) -> Result<Vec<MessageItem>, String> {
    pull_from_relay_v1_with_auth(relay_ws, wayfarer_id, device_id, limit, None)
}

#[allow(dead_code)]
pub fn pull_from_relay_v1_with_auth(
    relay_ws: &str,
    wayfarer_id: &str,
    device_id: &str,
    limit: Option<i64>,
    auth_token: Option<&str>,
) -> Result<Vec<MessageItem>, String> {
    let frame = PullFrame::new(limit)?;
    let mut socket = open_relay_socket(relay_ws, auth_token)?;
    complete_hello_handshake(&mut socket, wayfarer_id, device_id)?;
    send_json(&mut socket, &frame)?;

    let (frame, raw) = read_relay_frame(&mut socket)?;
    match frame {
        RelayInboundFrame::Message {
            msg_id,
            from_wayfarer_id,
            payload_b64,
            received_at,
        } => Ok(vec![MessageItem {
            msg_id,
            from_wayfarer_id,
            payload_b64,
            received_at,
        }]),
        RelayInboundFrame::Messages { messages } => Ok(messages),
        RelayInboundFrame::Error { code, message } => Err(format!(
            "relay error {code} while pulling: {message} (raw={raw})"
        )),
        other => Err(format!(
            "unexpected relay frame after pull: {other:?} (raw={raw})"
        )),
    }
}

#[allow(dead_code)]
pub fn ack_relay_message_v1(
    relay_ws: &str,
    wayfarer_id: &str,
    device_id: &str,
    msg_id: &str,
) -> Result<String, String> {
    ack_relay_message_v1_with_auth(relay_ws, wayfarer_id, device_id, msg_id, None)
}

#[allow(dead_code)]
pub fn ack_relay_message_v1_with_auth(
    relay_ws: &str,
    wayfarer_id: &str,
    device_id: &str,
    msg_id: &str,
    auth_token: Option<&str>,
) -> Result<String, String> {
    let frame = AckFrame::new(msg_id)?;
    let mut socket = open_relay_socket(relay_ws, auth_token)?;
    complete_hello_handshake(&mut socket, wayfarer_id, device_id)?;
    send_json(&mut socket, &frame)?;

    let (frame, raw) = read_relay_frame(&mut socket)?;
    match frame {
        RelayInboundFrame::AckOk { msg_id } => Ok(msg_id),
        RelayInboundFrame::Error { code, message } => Err(format!(
            "relay error {code} while acking: {message} (raw={raw})"
        )),
        other => Err(format!(
            "unexpected relay frame after ack: {other:?} (raw={raw})"
        )),
    }
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
    let timeout = Duration::from_secs(RELAY_CONNECT_TIMEOUT_SECS);

    let Ok(stream) = TcpStream::connect_timeout(
        &match socket_addr.parse() {
            Ok(addr) => addr,
            Err(err) => return Err(format!("invalid socket address: {err}")),
        },
        timeout,
    ) else {
        return Err(format!("tcp connection timeout/failure to {socket_addr}"));
    };

    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let mut request = match tungstenite::client::IntoClientRequest::into_client_request(relay_ws) {
        Ok(request) => request,
        Err(err) => return Err(format!("invalid websocket request: {err}")),
    };

    if let Some(token) = auth_token {
        match format!("Bearer {token}").parse() {
            Ok(header_value) => {
                request.headers_mut().insert("Authorization", header_value);
            }
            Err(err) => return Err(format!("invalid auth token header: {err}")),
        }
    }

    match client(request, stream) {
        Ok((socket, _response)) => Ok(socket),
        Err(err) => Err(format!("websocket handshake failed: {err}")),
    }
}

fn complete_hello_handshake(
    socket: &mut tungstenite::WebSocket<TcpStream>,
    wayfarer_id: &str,
    device_id: &str,
) -> Result<Option<String>, String> {
    if !is_valid_wayfarer_id(wayfarer_id) {
        return Err("invalid wayfarer_id format; expected 64 lowercase hex chars".to_string());
    }
    if !is_valid_device_id(device_id) {
        return Err("invalid device_id format; expected non-empty value".to_string());
    }

    let hello = HelloFrame::new(wayfarer_id, device_id);
    send_json(socket, &hello)?;

    let (frame, raw) = read_relay_frame(socket)?;
    match frame {
        RelayInboundFrame::HelloOk { relay_id } => Ok(relay_id),
        RelayInboundFrame::Error { code, message } => Err(format!(
            "connected; relay error {code}: {message} (raw={raw})"
        )),
        other => Err(format!(
            "connected; unexpected first frame after hello: {other:?} (raw={raw})"
        )),
    }
}

fn send_json<T: serde::Serialize>(
    socket: &mut tungstenite::WebSocket<TcpStream>,
    frame: &T,
) -> Result<(), String> {
    let text = serde_json::to_string(frame).map_err(|err| format!("json encode failed: {err}"))?;
    socket
        .send(Message::Text(text))
        .map_err(|err| format!("websocket send failed: {err}"))
}

fn read_relay_frame(
    socket: &mut tungstenite::WebSocket<TcpStream>,
) -> Result<(RelayInboundFrame, String), String> {
    match socket.read() {
        Ok(Message::Text(text)) => {
            let parsed = RelayInboundFrame::from_json(&text)
                .map_err(|err| format!("failed to parse relay frame: {err}; raw={text}"))?;
            Ok((parsed, text.to_string()))
        }
        Ok(other) => Err(format!("unexpected non-text relay frame: {other:?}")),
        Err(err) => Err(format!("websocket read failed: {err}")),
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

            let backoff = exponential.min(self.config.max_backoff);
            relay.next_attempt_at = Instant::now() + backoff;
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

#[cfg(test)]
mod tests {
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::json;
    use tungstenite::accept;

    use super::{
        ack_relay_message_v1, normalize_http_endpoint, pull_from_relay_v1, send_to_relay_v1,
        to_ws_endpoint, DispatcherError, RelayRequestDispatcher, RelaySessionConfig,
        RelaySessionManager,
    };
    use crate::aethos_core::protocol::HelloFrame;

    #[test]
    fn normalizes_without_scheme() {
        assert_eq!(
            normalize_http_endpoint("192.168.1.200:8082"),
            "http://192.168.1.200:8082"
        );
    }

    #[test]
    fn preserves_scheme_when_present() {
        assert_eq!(
            normalize_http_endpoint("https://relay.example"),
            "https://relay.example"
        );
    }

    #[test]
    fn converts_http_to_ws_with_ws_path() {
        assert_eq!(
            to_ws_endpoint("http://192.168.1.200:8082"),
            "ws://192.168.1.200:8082/ws"
        );
    }

    #[test]
    fn converts_https_to_wss_with_ws_path() {
        assert_eq!(
            to_ws_endpoint("https://relay.example"),
            "wss://relay.example/ws"
        );
    }

    #[test]
    fn overrides_existing_path_to_ws_path() {
        assert_eq!(
            to_ws_endpoint("http://relay.example:8082/custom/path"),
            "ws://relay.example:8082/ws"
        );
    }

    #[test]
    fn relay_session_backoff_and_failover() {
        let mut manager = RelaySessionManager::new(
            vec![
                "http://primary.local:8082".to_string(),
                "http://secondary.local:9082".to_string(),
            ],
            RelaySessionConfig {
                base_backoff: Duration::from_millis(50),
                max_backoff: Duration::from_millis(500),
                ..Default::default()
            },
        );

        let now = Instant::now();
        let first = manager.select_relay(now).expect("select primary");
        assert_eq!(first.relay_slot, 0);

        manager.mark_failure(0);
        let second = manager
            .select_relay(Instant::now())
            .expect("select secondary");
        assert_eq!(second.relay_slot, 1);

        manager.mark_success(1);
        assert_eq!(manager.relays()[1].consecutive_failures, 0);
        assert!(manager.relays()[1].health_score > 0);
    }

    #[test]
    fn relay_session_supports_auth_tokens() {
        let mut manager = RelaySessionManager::new(
            vec!["http://relay.local:8082".to_string()],
            RelaySessionConfig::default(),
        );

        manager.set_auth_token(0, Some("session-token".to_string()));
        let selected = manager
            .select_relay(Instant::now())
            .expect("relay selection with token");

        assert_eq!(selected.auth_token.as_deref(), Some("session-token"));
    }

    #[test]
    fn dispatcher_tracks_correlation_and_resolves() {
        let mut dispatcher = RelayRequestDispatcher::default();
        let outbound = dispatcher.register_outbound("ping", json!({"seq": 1}));
        assert_eq!(dispatcher.pending_count(), 1);

        let response = super::RelayFrame {
            correlation_id: outbound.correlation_id.clone(),
            message_type: "pong".to_string(),
            payload: json!({"ok": true}),
        };

        let resolved = dispatcher
            .resolve_response(response)
            .expect("resolve known response");

        assert_eq!(resolved.request_message_type, "ping");
        assert_eq!(resolved.response_message_type, "pong");
        assert_eq!(dispatcher.pending_count(), 0);
    }

    #[test]
    fn dispatcher_rejects_unknown_correlation_id() {
        let mut dispatcher = RelayRequestDispatcher::default();
        let response = super::RelayFrame {
            correlation_id: "linux-404".to_string(),
            message_type: "pong".to_string(),
            payload: json!({}),
        };

        let result = dispatcher.resolve_response(response);
        assert_eq!(result.err(), Some(DispatcherError::UnknownCorrelationId));
    }

    #[test]
    fn hello_frame_uses_type_field() {
        let frame = HelloFrame::new(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "device-1",
        );
        let serialized = frame.to_json().expect("serialize hello frame");
        assert!(serialized.contains("\"type\":\"hello\""));
        assert!(!serialized.contains("message_type"));
    }

    #[test]
    fn send_pull_and_ack_roundtrip_against_mock_relay() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock relay listener");
        let addr = listener.local_addr().expect("mock relay local addr");
        let relay_ws = format!("ws://{addr}/ws");

        let server = thread::spawn(move || {
            for phase in 0..3 {
                let (stream, _) = listener.accept().expect("accept mock relay connection");
                handle_mock_session(stream, phase);
            }
        });

        let local_wayfarer_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let peer_wayfarer_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        let send_result = send_to_relay_v1(
            &relay_ws,
            local_wayfarer_id,
            "device-1",
            peer_wayfarer_id,
            "SGVsbG8",
            Some("550e8400-e29b-41d4-a716-446655440000"),
            Some(3600),
        )
        .expect("send frame through mock relay");
        assert_eq!(send_result.0, "msg-1");
        assert_eq!(send_result.1, Some(1_700_000_000));
        assert_eq!(send_result.2, Some(1_700_003_600));

        let pulled = pull_from_relay_v1(&relay_ws, local_wayfarer_id, "device-1", Some(10))
            .expect("pull frames through mock relay");
        assert_eq!(pulled.len(), 1);
        assert_eq!(pulled[0].msg_id, "msg-1");
        assert_eq!(pulled[0].from_wayfarer_id, peer_wayfarer_id);

        let acked = ack_relay_message_v1(&relay_ws, local_wayfarer_id, "device-1", "msg-1")
            .expect("ack frame through mock relay");
        assert_eq!(acked, "msg-1");

        server.join().expect("mock relay server join");
    }

    #[test]
    fn send_accepts_message_frame_fallback() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock relay listener");
        let addr = listener.local_addr().expect("mock relay local addr");
        let relay_ws = format!("ws://{addr}/ws");

        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept mock relay connection");
            let mut socket = accept(stream).expect("accept websocket");

            let _ = read_text_frame(&mut socket);
            socket
                .send(tungstenite::Message::Text(
                    "{\"type\":\"hello_ok\",\"relay_id\":\"relay-mock\"}".into(),
                ))
                .expect("send hello_ok");

            let request_text = read_text_frame(&mut socket);
            let request: serde_json::Value =
                serde_json::from_str(&request_text).expect("parse send request");
            assert_eq!(request["type"], "send");

            socket
                .send(tungstenite::Message::Text(
                    "{\"type\":\"message\",\"msg_id\":\"msg-fallback\",\"from\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",\"payload_b64\":\"SGVsbG8\",\"received_at\":1700001111}".into(),
                ))
                .expect("send fallback message frame");
        });

        let result = send_to_relay_v1(
            &relay_ws,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "device-1",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "SGVsbG8",
            None,
            Some(3600),
        )
        .expect("send succeeds with message frame fallback");

        assert_eq!(result.0, "msg-fallback");
        assert_eq!(result.1, Some(1_700_001_111));
        assert_eq!(result.2, None);

        server.join().expect("mock relay server join");
    }

    fn handle_mock_session(stream: TcpStream, phase: usize) {
        let mut socket = accept(stream).expect("accept websocket");

        let hello_text = read_text_frame(&mut socket);
        let hello: serde_json::Value = serde_json::from_str(&hello_text).expect("parse hello");
        assert_eq!(hello["type"], "hello");
        assert!(hello.get("wayfarer_id").is_some());
        assert!(hello.get("device_id").is_some());

        socket
            .send(tungstenite::Message::Text(
                "{\"type\":\"hello_ok\",\"relay_id\":\"relay-mock\"}".into(),
            ))
            .expect("send hello_ok");

        let request_text = read_text_frame(&mut socket);
        let request: serde_json::Value =
            serde_json::from_str(&request_text).expect("parse request frame");

        match phase {
            0 => {
                assert_eq!(request["type"], "send");
                socket
                    .send(tungstenite::Message::Text(
                        "{\"type\":\"send_ok\",\"msg_id\":\"msg-1\",\"received_at\":1700000000,\"expires_at\":1700003600}".into(),
                    ))
                    .expect("send send_ok");
            }
            1 => {
                assert_eq!(request["type"], "pull");
                socket
                    .send(tungstenite::Message::Text(
                        "{\"type\":\"messages\",\"messages\":[{\"msg_id\":\"msg-1\",\"from\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",\"payload_b64\":\"SGVsbG8\",\"received_at\":1700000000}]}".into(),
                    ))
                    .expect("send messages");
            }
            2 => {
                assert_eq!(request["type"], "ack");
                assert_eq!(request["msg_id"], "msg-1");
                socket
                    .send(tungstenite::Message::Text(
                        "{\"type\":\"ack_ok\",\"msg_id\":\"msg-1\"}".into(),
                    ))
                    .expect("send ack_ok");
            }
            _ => panic!("unexpected mock phase"),
        }
    }

    fn read_text_frame(socket: &mut tungstenite::WebSocket<TcpStream>) -> String {
        match socket.read().expect("read websocket frame") {
            tungstenite::Message::Text(text) => text.to_string(),
            other => panic!("expected text frame, got {other:?}"),
        }
    }
}
