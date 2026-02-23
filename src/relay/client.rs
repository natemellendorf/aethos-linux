use std::collections::HashMap;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use tungstenite::{client, Message};
use url::Url;

use crate::aethos_core::protocol::HelloEnvelope;

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

pub fn connect_to_relay(relay_ws: &str, wayfair_id: &str) -> String {
    connect_to_relay_with_auth(relay_ws, wayfair_id, None)
}

pub fn connect_to_relay_with_auth(
    relay_ws: &str,
    wayfair_id: &str,
    auth_token: Option<&str>,
) -> String {
    let Ok(url) = Url::parse(relay_ws) else {
        return "invalid relay URL".to_string();
    };

    let Some(host) = url.host_str() else {
        return "relay URL missing host".to_string();
    };

    let port = url.port_or_known_default().unwrap_or(80);
    let socket_addr = format!("{host}:{port}");
    let timeout = Duration::from_secs(RELAY_CONNECT_TIMEOUT_SECS);

    let Ok(stream) = TcpStream::connect_timeout(
        &match socket_addr.parse() {
            Ok(addr) => addr,
            Err(err) => return format!("invalid socket address: {err}"),
        },
        timeout,
    ) else {
        return format!("tcp connection timeout/failure to {socket_addr}");
    };

    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let mut request = match tungstenite::client::IntoClientRequest::into_client_request(relay_ws) {
        Ok(request) => request,
        Err(err) => return format!("invalid websocket request: {err}"),
    };

    if let Some(token) = auth_token {
        match format!("Bearer {token}").parse() {
            Ok(header_value) => {
                request.headers_mut().insert("Authorization", header_value);
            }
            Err(err) => return format!("invalid auth token header: {err}"),
        }
    }

    match client(request, stream) {
        Ok((mut socket, _response)) => {
            let envelope = HelloEnvelope::new(wayfair_id);
            match envelope
                .to_json()
                .map_err(|err| err.to_string())
                .and_then(|json| {
                    socket
                        .send(Message::Text(json))
                        .map_err(|err| err.to_string())
                }) {
                Ok(_) => "connected + hello sent".to_string(),
                Err(err) => format!("connected; hello send failed: {err}"),
            }
        }
        Err(err) => format!("websocket handshake failed: {err}"),
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
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::{
        normalize_http_endpoint, to_ws_endpoint, DispatcherError, RelayRequestDispatcher,
        RelaySessionConfig, RelaySessionManager,
    };

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
}
