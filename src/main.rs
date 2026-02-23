mod aethos_core;
mod relay;

use gtk4::gdk::Display;
use gtk4::prelude::*;
use gtk4::{
    glib, Application, ApplicationWindow, Box as GtkBox, Button, CssProvider, Entry, Label,
    ListBox, ListBoxRow, Orientation, ScrolledWindow, Stack, StackSwitcher, TextView,
    STYLE_PROVIDER_PRIORITY_APPLICATION,
};
use serde_json::json;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::aethos_core::identity_store::{
    delete_wayfair_id, ensure_local_identity, load_relay_session_cache, regenerate_local_identity,
    save_relay_session_cache, RelaySessionCache,
};
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
        .title("Aethos Waypoint · Linux Wayfarer Shell")
        .default_width(1100)
        .default_height(760)
        .build();

    let root = GtkBox::new(Orientation::Vertical, 12);
    root.add_css_class("root");
    root.set_margin_top(20);
    root.set_margin_bottom(20);
    root.set_margin_start(20);
    root.set_margin_end(20);

    let header = Label::new(Some("Aethos Waypoint · Linux Wayfarer Shell"));
    header.add_css_class("header");
    header.set_xalign(0.0);

    let subtitle = Label::new(Some(
        "Paralleling iOS app flow: Home · Peers · Inbox · Compose · Console.",
    ));
    subtitle.add_css_class("subtitle");
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);

    let tab_switcher = StackSwitcher::new();
    tab_switcher.set_halign(gtk4::Align::Start);

    let views = Stack::new();
    views.set_hexpand(true);
    views.set_vexpand(true);
    tab_switcher.set_stack(Some(&views));

    let home_panel = GtkBox::new(Orientation::Vertical, 10);
    home_panel.add_css_class("glass-panel");

    let home_title = Label::new(Some("Home"));
    home_title.add_css_class("section-title");
    home_title.set_xalign(0.0);

    let home_hint = Label::new(Some(
        "Cockpit summary view mirroring iOS: identity, relay state, peers, and transfer queues.",
    ));
    home_hint.set_xalign(0.0);
    home_hint.set_wrap(true);

    let home_identity = Label::new(Some("Local Wayfarer ID: not provisioned"));
    home_identity.set_xalign(0.0);

    let home_connectivity = Label::new(Some("Relay connectivity: idle"));
    home_connectivity.set_xalign(0.0);

    let home_peer_count = Label::new(Some("Peers: 3 known · 1 recently active"));
    home_peer_count.set_xalign(0.0);

    let home_transfer_count = Label::new(Some("Transfers: outbound 0 · inbound 0"));
    home_transfer_count.set_xalign(0.0);

    home_panel.append(&home_title);
    home_panel.append(&home_hint);
    home_panel.append(&home_identity);
    home_panel.append(&home_connectivity);
    home_panel.append(&home_peer_count);
    home_panel.append(&home_transfer_count);

    let peers_panel = GtkBox::new(Orientation::Vertical, 10);
    peers_panel.add_css_class("glass-panel");

    let peers_title = Label::new(Some("Peers"));
    peers_title.add_css_class("section-title");
    peers_title.set_xalign(0.0);

    let peers_hint = Label::new(Some(
        "Grouped peer roster preview (freshness buckets + identity entrypoint).",
    ));
    peers_hint.set_xalign(0.0);

    let peers_list = ListBox::new();
    peers_list.add_css_class("session-list");
    for peer_row in [
        "fresh · field-node-17 · wayfair_id=wf-17 · seen=30s ago",
        "active · ops-bridge · wayfair_id=wf-ops · seen=12m ago",
        "stale · archive-proxy · wayfair_id=wf-arch · seen=2d ago",
    ] {
        let row = ListBoxRow::new();
        row.set_selectable(false);
        let label = Label::new(Some(peer_row));
        label.set_xalign(0.0);
        label.set_margin_top(6);
        label.set_margin_bottom(6);
        row.set_child(Some(&label));
        peers_list.append(&row);
    }

    peers_panel.append(&peers_title);
    peers_panel.append(&peers_hint);
    peers_panel.append(&peers_list);

    let inbox_panel = GtkBox::new(Orientation::Vertical, 10);
    inbox_panel.add_css_class("glass-panel");

    let inbox_title = Label::new(Some("Inbox"));
    inbox_title.add_css_class("section-title");
    inbox_title.set_xalign(0.0);

    let inbox_hint = Label::new(Some(
        "Transfer history grouped by lifecycle state (queued/sending/receiving/delivered).",
    ));
    inbox_hint.set_xalign(0.0);
    inbox_hint.set_wrap(true);

    let inbox_list = ListBox::new();
    inbox_list.add_css_class("session-list");
    for transfer_row in [
        "delivered · tx#2026-001 · from field-node-17 · 4.2 MB",
        "receiving · tx#2026-004 · from ops-bridge · 18.1 MB",
        "queued outbound · tx#2026-005 · to wf-ops · 1 attachment",
    ] {
        let row = ListBoxRow::new();
        row.set_selectable(false);
        let label = Label::new(Some(transfer_row));
        label.set_xalign(0.0);
        label.set_margin_top(6);
        label.set_margin_bottom(6);
        row.set_child(Some(&label));
        inbox_list.append(&row);
    }

    inbox_panel.append(&inbox_title);
    inbox_panel.append(&inbox_hint);
    inbox_panel.append(&inbox_list);

    let compose_panel = GtkBox::new(Orientation::Vertical, 10);
    compose_panel.add_css_class("glass-panel");

    let compose_title = Label::new(Some("Compose"));
    compose_title.add_css_class("section-title");
    compose_title.set_xalign(0.0);

    let compose_hint = Label::new(Some(
        "File-send flow preview: destination + payload note + queued status signal.",
    ));
    compose_hint.set_xalign(0.0);
    compose_hint.set_wrap(true);

    let destination_entry = Entry::builder().hexpand(true).build();
    destination_entry.set_placeholder_text(Some("Destination Wayfarer ID"));

    let payload_entry = Entry::builder().hexpand(true).build();
    payload_entry.set_placeholder_text(Some("Payload/file note"));

    let compose_send = Button::with_label("Queue Transfer");
    compose_send.add_css_class("action");

    let compose_status = Label::new(Some("Compose status: waiting"));
    compose_status.set_xalign(0.0);
    compose_status.set_wrap(true);

    compose_panel.append(&compose_title);
    compose_panel.append(&compose_hint);
    compose_panel.append(&destination_entry);
    compose_panel.append(&payload_entry);
    compose_panel.append(&compose_send);
    compose_panel.append(&compose_status);

    let console_panel = GtkBox::new(Orientation::Vertical, 10);
    console_panel.add_css_class("glass-panel");

    let console_title = Label::new(Some("Console"));
    console_title.add_css_class("section-title");
    console_title.set_xalign(0.0);

    let onboarding_status = Label::new(Some(
        "Identity provisioning: Step 1/2 · Generate identity (or rotate if needed).",
    ));
    onboarding_status.set_xalign(0.0);
    onboarding_status.set_wrap(true);

    let id_box = GtkBox::new(Orientation::Horizontal, 8);
    let wayfair_id_entry = Entry::builder().hexpand(true).editable(false).build();
    wayfair_id_entry.set_placeholder_text(Some("No Wayfair ID generated yet"));

    let generate_button = Button::with_label("Generate / Rotate Identity");
    generate_button.add_css_class("action");
    let delete_button = Button::with_label("Delete Identity");
    delete_button.add_css_class("danger");

    id_box.append(&wayfair_id_entry);
    id_box.append(&generate_button);
    id_box.append(&delete_button);

    let identity_meta_label = Label::new(Some("Identity metadata: unavailable"));
    identity_meta_label.set_xalign(0.0);
    identity_meta_label.set_wrap(true);

    let relay_http_primary_entry = Entry::builder().hexpand(true).build();
    relay_http_primary_entry.set_text(DEFAULT_RELAY_HTTP_PRIMARY);
    let relay_http_secondary_entry = Entry::builder().hexpand(true).build();
    relay_http_secondary_entry.set_text(DEFAULT_RELAY_HTTP_SECONDARY);

    let connect_button = Button::with_label("Run Relay Diagnostics");
    connect_button.add_css_class("action");

    let relay_primary_label = Label::new(Some("Primary relay status: idle"));
    relay_primary_label.set_xalign(0.0);
    relay_primary_label.set_wrap(true);
    let relay_secondary_label = Label::new(Some("Secondary relay status: idle"));
    relay_secondary_label.set_xalign(0.0);
    relay_secondary_label.set_wrap(true);

    let diagnostics_text = TextView::new();
    diagnostics_text.set_editable(false);
    diagnostics_text.set_cursor_visible(false);
    diagnostics_text.set_wrap_mode(gtk4::WrapMode::WordChar);
    diagnostics_text
        .buffer()
        .set_text("Console timeline:\n- waiting for first relay run");

    let diagnostics_scroll = ScrolledWindow::builder().min_content_height(180).build();
    diagnostics_scroll.set_child(Some(&diagnostics_text));

    console_panel.append(&console_title);
    console_panel.append(&onboarding_status);
    console_panel.append(&id_box);
    console_panel.append(&identity_meta_label);
    console_panel.append(&relay_http_primary_entry);
    console_panel.append(&relay_http_secondary_entry);
    console_panel.append(&connect_button);
    console_panel.append(&relay_primary_label);
    console_panel.append(&relay_secondary_label);
    console_panel.append(&diagnostics_scroll);

    views.add_titled(&home_panel, Some("home"), "Home");
    views.add_titled(&peers_panel, Some("peers"), "Peers");
    views.add_titled(&inbox_panel, Some("inbox"), "Inbox");
    views.add_titled(&compose_panel, Some("compose"), "Compose");
    views.add_titled(&console_panel, Some("console"), "Console");

    root.append(&header);
    root.append(&subtitle);
    root.append(&tab_switcher);
    root.append(&views);
    window.set_child(Some(&root));

    if let Ok(identity) = ensure_local_identity() {
        wayfair_id_entry.set_text(&identity.wayfair_id);
        home_identity.set_text(&format!("Local Wayfarer ID: {}", identity.wayfair_id));
        let key_preview: String = identity.verifying_key_b64.chars().take(16).collect();
        identity_meta_label.set_text(&format!(
            "Identity metadata: device={} · verify_key={}…",
            identity.device_name, key_preview
        ));
        onboarding_status.set_text("Identity provisioning: Step 2/2 · Identity provisioned.");
    }

    if let Ok(Some(cache)) = load_relay_session_cache() {
        relay_primary_label.set_text(&format!("Primary relay status: {}", cache.primary_status));
        relay_secondary_label.set_text(&format!(
            "Secondary relay status: {}",
            cache.secondary_status
        ));
    }

    let (tx, rx) = channel::<RelayStatus>();

    {
        let wayfair_id_entry = wayfair_id_entry.clone();
        let identity_meta_label = identity_meta_label.clone();
        let onboarding_status = onboarding_status.clone();
        let home_identity = home_identity.clone();
        generate_button.connect_clicked(move |_| match regenerate_local_identity() {
            Ok(identity) => {
                let key_preview: String = identity.verifying_key_b64.chars().take(16).collect();
                wayfair_id_entry.set_text(&identity.wayfair_id);
                home_identity.set_text(&format!("Local Wayfarer ID: {}", identity.wayfair_id));
                identity_meta_label.set_text(&format!(
                    "Identity metadata: device={} · verify_key={}…",
                    identity.device_name, key_preview
                ));
                onboarding_status.set_text("Identity provisioning: Step 2/2 · Identity rotated.");
            }
            Err(err) => eprintln!("{err}"),
        });
    }

    {
        let wayfair_id_entry = wayfair_id_entry.clone();
        let identity_meta_label = identity_meta_label.clone();
        let onboarding_status = onboarding_status.clone();
        let home_identity = home_identity.clone();
        delete_button.connect_clicked(move |_| {
            if let Err(err) = delete_wayfair_id() {
                eprintln!("{err}");
            }
            wayfair_id_entry.set_text("");
            home_identity.set_text("Local Wayfarer ID: not provisioned");
            identity_meta_label.set_text("Identity metadata: unavailable");
            onboarding_status.set_text("Identity provisioning: Step 1/2 · Identity deleted.");
        });
    }

    {
        let tx = tx.clone();
        let wayfair_id_entry = wayfair_id_entry.clone();
        let identity_meta_label = identity_meta_label.clone();
        let onboarding_status = onboarding_status.clone();
        let home_identity = home_identity.clone();
        connect_button.connect_clicked(move |button| {
            button.set_sensitive(false);

            if wayfair_id_entry.text().is_empty() {
                match ensure_local_identity() {
                    Ok(identity) => {
                        let key_preview: String =
                            identity.verifying_key_b64.chars().take(16).collect();
                        wayfair_id_entry.set_text(&identity.wayfair_id);
                        home_identity
                            .set_text(&format!("Local Wayfarer ID: {}", identity.wayfair_id));
                        identity_meta_label.set_text(&format!(
                            "Identity metadata: device={} · verify_key={}…",
                            identity.device_name, key_preview
                        ));
                        onboarding_status
                            .set_text("Identity provisioning: Step 2/2 · Auto-provisioned.");
                    }
                    Err(err) => eprintln!("{err}"),
                }
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

    {
        let compose_status = compose_status.clone();
        let destination_entry = destination_entry.clone();
        let payload_entry = payload_entry.clone();
        compose_send.connect_clicked(move |_| {
            let destination = destination_entry.text().to_string();
            let payload = payload_entry.text().to_string();

            if destination.trim().is_empty() || payload.trim().is_empty() {
                compose_status
                    .set_text("Compose status: destination and payload note are required");
                return;
            }

            compose_status.set_text(&format!(
                "Compose status: queued outbound transfer to {} ({})",
                destination, payload
            ));
        });
    }

    attach_status_poller(
        rx,
        connect_button,
        relay_primary_label,
        relay_secondary_label,
        diagnostics_text,
        home_connectivity,
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
    diagnostics_text: TextView,
    home_connectivity: Label,
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

            let buffer = diagnostics_text.buffer();
            let previous = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            let next = format!("{previous}\n- {text}");
            buffer.set_text(&next);

            if text.contains("connected + hello sent") {
                home_connectivity.set_text("Relay connectivity: online");
            } else {
                home_connectivity.set_text("Relay connectivity: degraded/offline");
            }
        }

        if completed >= 2 {
            let primary = relay_primary_label.text().to_string();
            let secondary = relay_secondary_label.text().to_string();
            if let Err(err) = save_relay_session_cache(&RelaySessionCache {
                primary_status: primary,
                secondary_status: secondary,
            }) {
                eprintln!("{err}");
            }

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

        .section-title {
            font-size: 18px;
            font-weight: 700;
            color: #e8f1ff;
        }

        .glass-panel {
            border-radius: 18px;
            padding: 18px;
            border: 1px solid rgba(214, 230, 255, 0.35);
            background: linear-gradient(135deg, rgba(139, 176, 255, 0.18), rgba(126, 95, 255, 0.12));
            box-shadow: 0 8px 18px rgba(15, 19, 40, 0.35);
        }

        entry, textview, list {
            border-radius: 10px;
            border: 1px solid rgba(204, 221, 255, 0.4);
            background: rgba(11, 18, 39, 0.5);
            color: #eff6ff;
        }

        stackswitcher button {
            border-radius: 8px;
            margin-right: 6px;
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
