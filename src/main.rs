use gtk4::prelude::*;
use gtk4::{
    glib, Application, ApplicationWindow, Box as GtkBox, Button, Entry, Label, Orientation,
};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use tungstenite::{connect, Message};
use url::Url;
use uuid::Uuid;

const APP_ID: &str = "org.aethos.linux";
const RELAY_HTTP_PRIMARY: &str = "http://192.168.1.200:8082";
const RELAY_HTTP_SECONDARY: &str = "http://192.168.1.200:9082";

#[derive(Clone, Debug)]
struct RelayStatus {
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
        .default_width(760)
        .default_height(420)
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

    let id_box = GtkBox::new(Orientation::Horizontal, 8);
    let wayfair_id_entry = Entry::builder().hexpand(true).editable(false).build();
    wayfair_id_entry.set_placeholder_text(Some("No Wayfair ID generated yet"));
    let generate_button = Button::with_label("Generate Wayfair ID");

    id_box.append(&wayfair_id_entry);
    id_box.append(&generate_button);

    let relay_primary_label = Label::new(Some(&format!(
        "{RELAY_HTTP_PRIMARY} -> ws://192.168.1.200:8082 - not connected"
    )));
    relay_primary_label.set_xalign(0.0);
    relay_primary_label.set_wrap(true);

    let relay_secondary_label = Label::new(Some(&format!(
        "{RELAY_HTTP_SECONDARY} -> ws://192.168.1.200:9082 - not connected"
    )));
    relay_secondary_label.set_xalign(0.0);
    relay_secondary_label.set_wrap(true);

    let connect_button = Button::with_label("Connect to Relays");

    root.append(&title);
    root.append(&subtitle);
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
            spawn_relay_check(RELAY_HTTP_PRIMARY, tx.clone());
            spawn_relay_check(RELAY_HTTP_SECONDARY, tx.clone());
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

    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        while let Ok(status) = rx.try_recv() {
            completed += 1;
            let text = format!(
                "{} -> {} - {}",
                status.relay_http, status.relay_ws, status.state
            );
            if status.relay_http == RELAY_HTTP_PRIMARY {
                relay_primary_label.set_text(&text);
            } else if status.relay_http == RELAY_HTTP_SECONDARY {
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

fn spawn_relay_check(relay_http: &str, tx: Sender<RelayStatus>) {
    let relay_http = relay_http.to_string();
    thread::spawn(move || {
        let relay_ws = to_ws_endpoint(&relay_http);
        let state = connect_to_relay(&relay_ws);
        let _ = tx.send(RelayStatus {
            relay_http,
            relay_ws,
            state,
        });
    });
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

    match connect(url.as_str()) {
        Ok((mut socket, _response)) => {
            let hello_payload =
                r#"{"type":"hello","platform":"linux","client":"aethos-linux-mvp0"}"#;
            match socket.send(Message::Text(hello_payload.into())) {
                Ok(_) => "connected + hello sent".to_string(),
                Err(err) => format!("connected; hello send failed: {err}"),
            }
        }
        Err(err) => format!("connection failed: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::to_ws_endpoint;

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

    #[test]
    fn keeps_ws_as_is() {
        assert_eq!(to_ws_endpoint("ws://relay:8082"), "ws://relay:8082");
    }
}
