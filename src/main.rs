use gtk4::prelude::*;
use gtk4::{
    glib, Application, ApplicationWindow, Box as GtkBox, Button, Entry, Label, Orientation,
};
use std::thread;
use tungstenite::{connect, Message};
use url::Url;
use uuid::Uuid;

const APP_ID: &str = "org.aethos.linux";
const RELAY_PRIMARY: &str = "ws://192.168.1.200:8082";
const RELAY_SECONDARY: &str = "ws://192.168.1.200:9082";

#[derive(Clone, Debug)]
struct RelayStatus {
    relay: String,
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
        .default_width(720)
        .default_height(400)
        .build();

    let root = GtkBox::new(Orientation::Vertical, 12);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(16);
    root.set_margin_end(16);

    let title = Label::new(Some("Aethos Linux Client (MVP 0)"));
    title.add_css_class("title-2");

    let subtitle = Label::new(Some(
        "Generate a Wayfair ID and connect to two configured Aethos relay endpoints.",
    ));
    subtitle.set_wrap(true);

    let id_box = GtkBox::new(Orientation::Horizontal, 8);
    let wayfair_id_entry = Entry::builder().hexpand(true).editable(false).build();
    wayfair_id_entry.set_placeholder_text(Some("No Wayfair ID generated yet"));
    let generate_button = Button::with_label("Generate Wayfair ID");

    id_box.append(&wayfair_id_entry);
    id_box.append(&generate_button);

    let relay_primary_label = Label::new(Some(&format!("{RELAY_PRIMARY} - not connected")));
    relay_primary_label.set_xalign(0.0);
    let relay_secondary_label = Label::new(Some(&format!("{RELAY_SECONDARY} - not connected")));
    relay_secondary_label.set_xalign(0.0);

    let connect_button = Button::with_label("Connect to Relays");

    root.append(&title);
    root.append(&subtitle);
    root.append(&id_box);
    root.append(&connect_button);
    root.append(&relay_primary_label);
    root.append(&relay_secondary_label);

    window.set_child(Some(&root));

    let (tx, rx) = std::sync::mpsc::channel::<RelayStatus>();

    {
        let wayfair_id_entry = wayfair_id_entry.clone();
        generate_button.connect_clicked(move |_| {
            let wayfair_id = Uuid::new_v4().to_string();
            wayfair_id_entry.set_text(&wayfair_id);
        });
    }

    {
        let tx = tx.clone();
        connect_button.connect_clicked(move |button| {
            button.set_sensitive(false);
            for relay in [RELAY_PRIMARY, RELAY_SECONDARY] {
                let tx_inner = tx.clone();
                thread::spawn(move || {
                    let result = connect_to_relay(relay);
                    let _ = tx_inner.send(RelayStatus {
                        relay: relay.to_string(),
                        state: result,
                    });
                });
            }
        });
    }

    glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
        while let Ok(status) = rx.try_recv() {
            if status.relay == RELAY_PRIMARY {
                relay_primary_label.set_text(&format!("{} - {}", status.relay, status.state));
            } else if status.relay == RELAY_SECONDARY {
                relay_secondary_label.set_text(&format!("{} - {}", status.relay, status.state));
            }
        }
        glib::ControlFlow::Continue
    });

    window.present();
}

fn connect_to_relay(relay: &str) -> String {
    let Ok(url) = Url::parse(relay) else {
        return "invalid relay URL".to_string();
    };

    match connect(url) {
        Ok((mut socket, _response)) => {
            let ping_payload = format!("{{\"type\":\"hello\",\"platform\":\"linux\"}}");
            let _ = socket.send(Message::Text(ping_payload));
            "connected".to_string()
        }
        Err(err) => format!("connection failed: {err}"),
    }
}
