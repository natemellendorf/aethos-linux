mod aethos_core;
mod relay;

use gtk4::gdk::Display;
use gtk4::prelude::*;
use gtk4::{
    glib, Application, ApplicationWindow, Box as GtkBox, Button, CssProvider, Entry, Label,
    Orientation, STYLE_PROVIDER_PRIORITY_APPLICATION,
};
use serde_json::json;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::aethos_core::identity_store::{delete_wayfair_id, load_wayfair_id, save_wayfair_id};
use crate::relay::client::{
    connect_to_relay, connect_to_relay_with_auth, normalize_http_endpoint, RelayFrame,
    RelayRequestDispatcher, RelaySessionConfig, RelaySessionManager,
};

const APP_ID: &str = "org.aethos.linux";
const DEFAULT_RELAY_HTTP_PRIMARY: &str = "http://192.168.1.200:8082";
const DEFAULT_RELAY_HTTP_SECONDARY: &str = "http://192.168.1.200:9082";

#[derive(Clone, Debug)]
struct RelayStatus {
    relay_slot: usize,
    relay_http: String,
    relay_ws: String,
    state: String,
    dispatch: String,
}

fn main() -> glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &Application) {
    apply_styles();

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Aethos Waypoint · Linux MVP2")
        .default_width(920)
        .default_height(620)
        .build();

    let root = GtkBox::new(Orientation::Vertical, 12);
    root.add_css_class("root");
    root.set_margin_top(20);
    root.set_margin_bottom(20);
    root.set_margin_start(20);
    root.set_margin_end(20);

    let header = Label::new(Some("Aethos Waypoint · Linux MVP2"));
    header.add_css_class("header");
    header.set_xalign(0.0);

    let subtitle = Label::new(Some(
        "Cockpit preview: generate identity and run relay session manager checks with correlation tracking.",
    ));
    subtitle.add_css_class("subtitle");
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);

    let glass_panel = GtkBox::new(Orientation::Vertical, 10);
    glass_panel.add_css_class("glass-panel");

    let relay_config_title = Label::new(Some("Relay HTTP listeners (editable):"));
    relay_config_title.set_xalign(0.0);

    let relay_http_primary_entry = Entry::builder().hexpand(true).build();
    relay_http_primary_entry.set_text(DEFAULT_RELAY_HTTP_PRIMARY);
    let relay_http_secondary_entry = Entry::builder().hexpand(true).build();
    relay_http_secondary_entry.set_text(DEFAULT_RELAY_HTTP_SECONDARY);

    let id_box = GtkBox::new(Orientation::Horizontal, 8);
    let wayfair_id_entry = Entry::builder().hexpand(true).editable(false).build();
    wayfair_id_entry.set_placeholder_text(Some("No Wayfair ID generated yet"));

    if let Ok(Some(existing_id)) = load_wayfair_id() {
        wayfair_id_entry.set_text(&existing_id);
    }

    let generate_button = Button::with_label("Generate Wayfair ID");
    generate_button.add_css_class("action");
    let delete_button = Button::with_label("Delete Wayfair ID");
    delete_button.add_css_class("danger");

    id_box.append(&wayfair_id_entry);
    id_box.append(&generate_button);
    id_box.append(&delete_button);

    let identity_notice = Label::new(Some(
        "Deleting your Wayfair ID is like changing your email address. If you do not back up your keypair, you may lose access to data sent to this identity.",
    ));
    identity_notice.add_css_class("warning");
    identity_notice.set_xalign(0.0);
    identity_notice.set_wrap(true);

    let relay_primary_label = Label::new(Some("Primary relay status: idle"));
    relay_primary_label.set_xalign(0.0);
    relay_primary_label.set_wrap(true);

    let relay_secondary_label = Label::new(Some("Secondary relay status: idle"));
    relay_secondary_label.set_xalign(0.0);
    relay_secondary_label.set_wrap(true);

    let connect_button = Button::with_label("Connect to Relays");
    connect_button.add_css_class("action");

    glass_panel.append(&relay_config_title);
    glass_panel.append(&relay_http_primary_entry);
    glass_panel.append(&relay_http_secondary_entry);
    glass_panel.append(&id_box);
    glass_panel.append(&identity_notice);
    glass_panel.append(&connect_button);
    glass_panel.append(&relay_primary_label);
    glass_panel.append(&relay_secondary_label);

    root.append(&header);
    root.append(&subtitle);
    root.append(&glass_panel);
    window.set_child(Some(&root));

    let (tx, rx) = channel::<RelayStatus>();

    {
        let wayfair_id_entry = wayfair_id_entry.clone();
        generate_button.connect_clicked(move |_| {
            let wayfair_id = Uuid::new_v4().to_string();
            if let Err(err) = save_wayfair_id(&wayfair_id) {
                eprintln!("{err}");
            }
            wayfair_id_entry.set_text(&wayfair_id);
        });
    }

    {
        let wayfair_id_entry = wayfair_id_entry.clone();
        delete_button.connect_clicked(move |_| {
            if let Err(err) = delete_wayfair_id() {
                eprintln!("{err}");
            }
            wayfair_id_entry.set_text("");
        });
    }

    {
        let tx = tx.clone();
        let wayfair_id_entry = wayfair_id_entry.clone();
        connect_button.connect_clicked(move |button| {
            button.set_sensitive(false);

            if wayfair_id_entry.text().is_empty() {
                let wayfair_id = Uuid::new_v4().to_string();
                if let Err(err) = save_wayfair_id(&wayfair_id) {
                    eprintln!("{err}");
                }
                wayfair_id_entry.set_text(&wayfair_id);
            }

            let wayfair_id = wayfair_id_entry.text().to_string();
            let relay_http_primary = normalize_http_endpoint(&relay_http_primary_entry.text());
            let relay_http_secondary = normalize_http_endpoint(&relay_http_secondary_entry.text());

            spawn_relay_checks(
                vec![relay_http_primary, relay_http_secondary],
                &wayfair_id,
                tx.clone(),
            );
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

fn spawn_relay_checks(
    relay_http_endpoints: Vec<String>,
    wayfair_id: &str,
    tx: Sender<RelayStatus>,
) {
    let wayfair_id = wayfair_id.to_string();

    thread::spawn(move || {
        let mut session_manager =
            RelaySessionManager::new(relay_http_endpoints, RelaySessionConfig::default());
        let mut dispatcher = RelayRequestDispatcher::default();

        let shared_auth = std::env::var("AETHOS_RELAY_AUTH_TOKEN").ok();
        let relay_count = session_manager.relays().len();
        for relay_slot in 0..relay_count {
            session_manager.set_auth_token(relay_slot, shared_auth.clone());
        }

        let mut completed = 0;
        while completed < relay_count {
            let Some(selection) = session_manager.select_relay(Instant::now()) else {
                thread::sleep(Duration::from_millis(50));
                continue;
            };

            let outbound = dispatcher.register_outbound(
                "hello",
                json!({"wayfair_id": wayfair_id, "relay_slot": selection.relay_slot}),
            );

            let state = match selection.auth_token.as_deref() {
                Some(token) => {
                    connect_to_relay_with_auth(&selection.relay_ws, &wayfair_id, Some(token))
                }
                None => connect_to_relay(&selection.relay_ws, &wayfair_id),
            };

            if state.starts_with("connected + hello sent") {
                session_manager.mark_success(selection.relay_slot);
            } else {
                session_manager.mark_failure(selection.relay_slot);
            }

            let response = RelayFrame {
                correlation_id: outbound.correlation_id,
                message_type: if state.starts_with("connected + hello sent") {
                    "hello_ack".to_string()
                } else {
                    "hello_error".to_string()
                },
                payload: json!({"relay_ws": selection.relay_ws, "state": state}),
            };

            let dispatch = match dispatcher.resolve_response(response) {
                Ok(resolved) => {
                    format!(
                        "corr={} req={} resp={} pending={} payload={}",
                        resolved.correlation_id,
                        resolved.request_message_type,
                        resolved.response_message_type,
                        dispatcher.pending_count(),
                        resolved.payload
                    )
                }
                Err(_) => "dispatcher error: unknown correlation".to_string(),
            };

            let _ = tx.send(RelayStatus {
                relay_slot: selection.relay_slot,
                relay_http: selection.relay_http,
                relay_ws: selection.relay_ws,
                state,
                dispatch,
            });
            completed += 1;
        }
    });
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
                "{} -> {} · {} · {}",
                status.relay_http, status.relay_ws, status.state, status.dispatch
            );

            if status.relay_slot == 0 {
                relay_primary_label.set_text(&format!("Primary relay status: {text}"));
            } else {
                relay_secondary_label.set_text(&format!("Secondary relay status: {text}"));
            }
        }

        if completed >= 2 {
            completed = 0;
            connect_button.set_sensitive(true);
        }

        glib::ControlFlow::Continue
    });
}

fn apply_styles() {
    let provider = CssProvider::new();
    provider.load_from_data(
        "
        window {
            background: radial-gradient(circle at top left, #263f76, #1a2147 45%, #111632 75%, #0d1022 100%);
            color: #e4efff;
        }

        .root {
            background: transparent;
        }

        .header {
            font-size: 30px;
            font-weight: 800;
            color: #e8f1ff;
        }

        .subtitle {
            font-size: 15px;
            color: rgba(221, 233, 255, 0.85);
        }

        .glass-panel {
            border-radius: 18px;
            padding: 18px;
            border: 1px solid rgba(214, 230, 255, 0.35);
            background: linear-gradient(135deg, rgba(139, 176, 255, 0.18), rgba(126, 95, 255, 0.12));
            box-shadow: 0 8px 18px rgba(15, 19, 40, 0.35);
        }

        entry {
            border-radius: 10px;
            border: 1px solid rgba(204, 221, 255, 0.4);
            background: rgba(11, 18, 39, 0.5);
            color: #eff6ff;
        }

        button.action {
            border-radius: 10px;
            border: 1px solid rgba(194, 218, 255, 0.45);
            background: linear-gradient(90deg, rgba(120, 159, 255, 0.35), rgba(146, 120, 255, 0.25));
            color: #ecf5ff;
            font-weight: 700;
            padding: 8px 12px;
        }

        button.danger {
            border-radius: 10px;
            border: 1px solid rgba(255, 187, 187, 0.45);
            background: linear-gradient(90deg, rgba(160, 74, 115, 0.35), rgba(138, 64, 96, 0.25));
            color: #ffe7ef;
            font-weight: 700;
            padding: 8px 12px;
        }

        .warning {
            color: rgba(255, 217, 217, 0.95);
            font-size: 13px;
        }
        ",
    );

    if let Some(display) = Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
