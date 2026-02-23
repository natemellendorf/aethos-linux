use gtk4::prelude::*;
use gtk4::{
    glib, Application, ApplicationWindow, Box as GtkBox, Button, Entry, Label, Orientation,
};
use std::net::TcpStream;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::Duration;
use tungstenite::{client, Message};
use url::Url;
use uuid::Uuid;

const APP_ID: &str = "org.aethos.linux";
const DEFAULT_RELAY_HTTP_PRIMARY: &str = "http://192.168.1.200:8082";
const DEFAULT_RELAY_HTTP_SECONDARY: &str = "http://192.168.1.200:9082";
const RELAY_CONNECT_TIMEOUT_SECS: u64 = 5;

#[derive(Clone, Debug)]
struct RelayStatus {
    relay_slot: usize,
    relay_http: String,
    relay_ws: String,
    state: String,
}

fn main() -> glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &Application) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Aethos Linux MVP0")
        .default_width(840)
        .default_height(480)
        .build();

    let root = GtkBox::new(Orientation::Vertical, 12);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(16);
    root.set_margin_end(16);

    let title = Label::new(Some("Aethos Linux Client (MVP 0)"));
    title.add_css_class("title-2");

    let subtitle = Label::new(Some(
        "Generate a Wayfair ID and connect over WebSocket to configured Aethos relay endpoints.",
    ));
    subtitle.set_wrap(true);
    subtitle.set_xalign(0.0);

    let relay_config_title = Label::new(Some("Relay HTTP listeners (editable):"));
    relay_config_title.set_xalign(0.0);

    let relay_http_primary_entry = Entry::builder().hexpand(true).build();
    relay_http_primary_entry.set_text(DEFAULT_RELAY_HTTP_PRIMARY);
    let relay_http_secondary_entry = Entry::builder().hexpand(true).build();
    relay_http_secondary_entry.set_text(DEFAULT_RELAY_HTTP_SECONDARY);

    let id_box = GtkBox::new(Orientation::Horizontal, 8);
    let wayfair_id_entry = Entry::builder().hexpand(true).editable(false).build();
    wayfair_id_entry.set_placeholder_text(Some("No Wayfair ID generated yet"));
    let generate_button = Button::with_label("Generate Wayfair ID");

    id_box.append(&wayfair_id_entry);
    id_box.append(&generate_button);

    let relay_primary_label = Label::new(Some(&format!(
        "{DEFAULT_RELAY_HTTP_PRIMARY} -> ws://192.168.1.200:8082 - not connected"
    )));
    relay_primary_label.set_xalign(0.0);
    relay_primary_label.set_wrap(true);

    let relay_secondary_label = Label::new(Some(&format!(
        "{DEFAULT_RELAY_HTTP_SECONDARY} -> ws://192.168.1.200:9082 - not connected"
    )));
    relay_secondary_label.set_xalign(0.0);
    relay_secondary_label.set_wrap(true);

    let connect_button = Button::with_label("Connect to Relays");

    root.append(&title);
    root.append(&subtitle);
    root.append(&relay_config_title);
    root.append(&relay_http_primary_entry);
    root.append(&relay_http_secondary_entry);
    root.append(&id_box);
    root.append(&connect_button);
    root.append(&relay_primary_label);
    root.append(&relay_secondary_label);

    window.set_child(Some(&root));

    let (tx, rx) = channel::<RelayStatus>();

    {
        let wayfair_id_entry = wayfair_id_entry.clone();
        generate_button.connect_clicked(move |_| {
            wayfair_id_entry.set_text(&Uuid::new_v4().to_string());
        });
    }

    {
        let tx = tx.clone();
        connect_button.connect_clicked(move |button| {
            button.set_sensitive(false);

            let relay_http_primary = normalize_http_endpoint(&relay_http_primary_entry.text());
            let relay_http_secondary = normalize_http_endpoint(&relay_http_secondary_entry.text());

            spawn_relay_check(0, &relay_http_primary, tx.clone());
            spawn_relay_check(1, &relay_http_secondary, tx.clone());
        });
    }

    attach_status_poller(
        rx,
        connect_button,
        relay_primary_label,
        relay_secondary_label,
    );

    window.present();
}

fn attach_status_poller(
    rx: Receiver<RelayStatus>,
    connect_button: Button,
    relay_primary_label: Label,
    relay_secondary_label: Label,
) {
    let mut completed = 0;

    glib::timeout_add_local(Duration::from_millis(200), move || {
        while let Ok(status) = rx.try_recv() {
            completed += 1;
            let text = format!(
                "{} -> {} - {}",
                status.relay_http, status.relay_ws, status.state
            );

            if status.relay_slot == 0 {
                relay_primary_label.set_text(&text);
            } else {
                relay_secondary_label.set_text(&text);
            }
        }

        if completed >= 2 {
            completed = 0;
            connect_button.set_sensitive(true);
        }

        glib::ControlFlow::Continue
    });
}

fn spawn_relay_check(relay_slot: usize, relay_http: &str, tx: Sender<RelayStatus>) {
    let relay_http = relay_http.to_string();
    thread::spawn(move || {
        let relay_ws = to_ws_endpoint(&relay_http);
        let state = connect_to_relay(&relay_ws);
        let _ = tx.send(RelayStatus {
            relay_slot,
            relay_http,
            relay_ws,
            state,
        });
    });
}

fn normalize_http_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return trimmed.to_string();
    }

    format!("http://{trimmed}")
}

fn to_ws_endpoint(http_like: &str) -> String {
    if let Some(stripped) = http_like.strip_prefix("http://") {
        return format!("ws://{stripped}");
    }
    if let Some(stripped) = http_like.strip_prefix("https://") {
        return format!("wss://{stripped}");
    }

    http_like.to_string()
}

fn connect_to_relay(relay_ws: &str) -> String {
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
            let hello_payload =
                r#"{"type":"hello","platform":"linux","client":"aethos-linux-mvp0"}"#;
            match socket.send(Message::Text(hello_payload.into())) {
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
    fn converts_http_to_ws() {
        assert_eq!(
            to_ws_endpoint("http://192.168.1.200:8082"),
            "ws://192.168.1.200:8082"
        );
    }

    #[test]
    fn converts_https_to_wss() {
        assert_eq!(
            to_ws_endpoint("https://relay.example"),
            "wss://relay.example"
        );
    }
}
