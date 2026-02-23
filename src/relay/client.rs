use std::net::TcpStream;
use std::time::Duration;

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

    match client(url.as_str(), stream) {
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

#[cfg(test)]
mod tests {
    use super::{normalize_http_endpoint, to_ws_endpoint};

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
}
