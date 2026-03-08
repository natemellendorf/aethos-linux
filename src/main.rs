mod aethos_core;
mod relay;

use base64::Engine;
use gtk4::gdk::Display;
use gtk4::pango::EllipsizeMode;
use gtk4::prelude::*;
use gtk4::{
    glib, Application, ApplicationWindow, Box as GtkBox, Button, CheckButton, ComboBoxText,
    CssProvider, Dialog, DrawingArea, Entry, Image, Label, ListBox, ListBoxRow, Orientation, Paned,
    Popover, PositionType, ResponseType, Revealer, RevealerTransitionType, ScrolledWindow, Stack,
    StackSwitcher, TextView, STYLE_PROVIDER_PRIORITY_APPLICATION,
};
use image::{imageops::FilterType, ImageBuffer, Luma, Rgba, RgbaImage};
use qrcode::QrCode;
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::aethos_core::gossip_sync::{
    has_item as gossip_has_item, import_transfer_items,
    inventory_entries as gossip_inventory_entries, parse_frame as parse_gossip_frame,
    record_local_payload as gossip_record_local_payload, serialize_frame as serialize_gossip_frame,
    transfer_items_for_request as gossip_transfer_items, GossipSyncFrame, GOSSIP_LAN_PORT,
    GOSSIP_SYNC_VERSION,
};
use crate::aethos_core::identity_store::{
    delete_wayfarer_id, ensure_local_identity, load_contact_aliases, load_relay_session_cache,
    regenerate_local_identity, save_contact_aliases, save_relay_session_cache, RelaySessionCache,
};
use crate::aethos_core::protocol::{
    build_envelope_payload_b64_from_utf8, decode_envelope_payload_b64,
    decode_envelope_payload_utf8_preview, is_valid_wayfarer_id,
};
use crate::relay::client::{
    ack_relay_message_v1_with_auth, connect_to_relay, connect_to_relay_with_auth,
    normalize_http_endpoint, pull_from_relay_v1_with_auth, send_to_relay_v1_with_auth,
    to_ws_endpoint, RelayFrame, RelayRequestDispatcher, RelaySessionConfig, RelaySessionManager,
};

const APP_ID: &str = "org.aethos.linux";
const DEFAULT_RELAY_HTTP_PRIMARY: &str = "http://192.168.1.200:8082";
const DEFAULT_RELAY_HTTP_SECONDARY: &str = "http://192.168.1.200:9082";
const APP_LOG_FILE_NAME: &str = "aethos-linux.log";
const CHAT_HISTORY_FILE_NAME: &str = "chat-history.json";
const SHARE_QR_FILE_NAME: &str = "share-wayfarer-qr.png";
const APP_SETTINGS_FILE_NAME: &str = "settings.json";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug)]
struct RelayStatus {
    relay_slot: usize,
    batch_total: usize,
    relay_http: String,
    relay_ws: String,
    state: String,
    dispatch: String,
}

#[derive(Clone, Copy, Debug)]
enum SessionOp {
    Send,
    Inbox,
}

#[derive(Clone, Debug)]
struct SessionStatus {
    op: SessionOp,
    text: String,
    ack_msg_id: Option<String>,
    outgoing_contact: Option<String>,
    outgoing_text: Option<String>,
    outgoing_manifest_id: Option<String>,
    outgoing_local_id: Option<String>,
    outgoing_error: Option<String>,
    pulled_messages: Vec<PulledMessagePreview>,
}

#[derive(Clone, Debug)]
struct PulledMessagePreview {
    from_wayfarer_id: String,
    msg_id: String,
    text: String,
    received_at: i64,
    manifest_id_hex: Option<String>,
    receipt_manifest_id: Option<String>,
    receipt_received_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum ChatDirection {
    Incoming,
    Outgoing,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
enum OutboundState {
    Sending,
    Sent,
    Failed { error: String },
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ChatMessage {
    msg_id: String,
    text: String,
    timestamp: String,
    #[serde(default)]
    created_at_unix: i64,
    direction: ChatDirection,
    #[serde(default)]
    seen: bool,
    #[serde(default)]
    manifest_id_hex: Option<String>,
    #[serde(default)]
    delivered_at: Option<String>,
    #[serde(default)]
    outbound_state: Option<OutboundState>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct PersistedChatState {
    selected_contact: Option<String>,
    threads: BTreeMap<String, Vec<ChatMessage>>,
}

#[derive(Default, Debug)]
struct ChatState {
    selected_contact: Option<String>,
    threads: BTreeMap<String, Vec<ChatMessage>>,
    show_full_contact_id: bool,
    contact_aliases: BTreeMap<String, String>,
    new_contacts: BTreeSet<String>,
    contact_filter: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct AppSettings {
    relay_sync_enabled: bool,
    gossip_sync_enabled: bool,
    relay_endpoints: Vec<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            relay_sync_enabled: true,
            gossip_sync_enabled: true,
            relay_endpoints: vec![
                DEFAULT_RELAY_HTTP_PRIMARY.to_string(),
                DEFAULT_RELAY_HTTP_SECONDARY.to_string(),
            ],
        }
    }
}

#[derive(Clone)]
struct RelayStatusUi {
    connect_button: Button,
    relay_primary_label: Label,
    relay_secondary_label: Label,
    diagnostics_text: TextView,
    relay_dot: Label,
    relay_chip_text: Label,
    relay_chip: GtkBox,
}

#[derive(Clone)]
struct SessionPollerUi {
    send_button: Button,
    body_entry: Entry,
    chat_status_label: Label,
    chat_state: Rc<RefCell<ChatState>>,
    contacts_list: ListBox,
    contact_order: Rc<RefCell<Vec<String>>>,
    selected_contact_id_value: Label,
    contact_alias_entry: Entry,
    messages_list: ListBox,
    messages_scroll: ScrolledWindow,
    thread_title: Label,
    thread_contact_id_label: Label,
    compact_contact_picker: ComboBoxText,
    picker_syncing: Rc<Cell<bool>>,
    wave_pending: Rc<Cell<bool>>,
    wave_mode_label: Label,
    auto_scroll_locked: Rc<Cell<bool>>,
}

struct MessageRenderUi<'a> {
    messages_list: &'a ListBox,
    messages_scroll: &'a ScrolledWindow,
    thread_title: &'a Label,
    thread_contact_id_label: &'a Label,
    send_button: &'a Button,
    body_entry: &'a Entry,
    auto_scroll_locked: &'a Rc<Cell<bool>>,
}

fn main() -> glib::ExitCode {
    if let Err(err) = ensure_linux_desktop_integration() {
        eprintln!("desktop integration warning: {err}");
    }

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &Application) {
    apply_styles();

    let initial_settings = load_app_settings().unwrap_or_default();
    let app_settings = Rc::new(RefCell::new(initial_settings.clone()));
    let relay_sync_enabled = Arc::new(AtomicBool::new(initial_settings.relay_sync_enabled));
    let gossip_sync_enabled = Arc::new(AtomicBool::new(initial_settings.gossip_sync_enabled));
    let gossip_last_activity_ms = Arc::new(AtomicU64::new(0));
    let gossip_force_announce = Arc::new(AtomicBool::new(initial_settings.gossip_sync_enabled));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Aethos Chat · Linux")
        .default_width(980)
        .default_height(680)
        .build();
    window.set_icon_name(Some(APP_ID));

    let root = GtkBox::new(Orientation::Vertical, 12);
    root.add_css_class("root");
    root.set_hexpand(true);
    root.set_vexpand(true);
    root.set_margin_top(20);
    root.set_margin_bottom(20);
    root.set_margin_start(20);
    root.set_margin_end(20);

    let tab_switcher = StackSwitcher::new();
    tab_switcher.set_halign(gtk4::Align::Start);
    tab_switcher.set_vexpand(false);

    let relay_chip = GtkBox::new(Orientation::Horizontal, 6);
    relay_chip.add_css_class("relay-chip");
    relay_chip.set_halign(gtk4::Align::End);
    let relay_dot = Label::new(Some("*"));
    relay_dot.add_css_class("relay-dot");
    relay_dot.add_css_class("relay-dot-idle");
    let relay_chip_text = Label::new(Some("Relays: idle"));
    relay_chip_text.add_css_class("relay-chip-text");
    relay_chip.append(&relay_dot);
    relay_chip.append(&relay_chip_text);

    let gossip_chip = GtkBox::new(Orientation::Horizontal, 6);
    gossip_chip.add_css_class("gossip-chip");
    let gossip_dot = Label::new(Some("*"));
    gossip_dot.add_css_class("gossip-dot");
    gossip_dot.add_css_class("gossip-dot-idle");
    let gossip_chip_text = Label::new(Some("LAN Gossip: idle"));
    gossip_chip_text.add_css_class("relay-chip-text");
    gossip_chip.append(&gossip_dot);
    gossip_chip.append(&gossip_chip_text);

    let top_bar = GtkBox::new(Orientation::Horizontal, 8);
    let top_bar_spacer = GtkBox::new(Orientation::Horizontal, 0);
    top_bar_spacer.set_hexpand(true);
    top_bar.set_hexpand(true);
    top_bar.append(&tab_switcher);
    top_bar.append(&top_bar_spacer);
    top_bar.append(&gossip_chip);
    top_bar.append(&relay_chip);

    let wave_strip = DrawingArea::new();
    wave_strip.add_css_class("wave-strip");
    wave_strip.set_content_height(42);
    wave_strip.set_hexpand(true);
    wave_strip.set_vexpand(false);

    let wave_mode_label = Label::new(Some("Ambient mesh active"));
    wave_mode_label.add_css_class("wave-mode-label");
    wave_mode_label.set_xalign(0.0);

    let wave_phase = Rc::new(Cell::new(0.0_f64));
    let wave_pending = Rc::new(Cell::new(false));
    setup_wave_strip(
        &wave_strip,
        Rc::clone(&wave_phase),
        Rc::clone(&wave_pending),
    );

    let views = Stack::new();
    views.set_hexpand(true);
    views.set_vexpand(true);
    tab_switcher.set_stack(Some(&views));

    let onboarding_panel = GtkBox::new(Orientation::Vertical, 10);
    onboarding_panel.add_css_class("glass-panel");

    let onboarding_title = Label::new(Some("Onboarding"));
    onboarding_title.add_css_class("section-title");
    onboarding_title.set_xalign(0.0);

    let onboarding_status =
        Label::new(Some("Step 1/2 · Identity auto-provisions on first launch."));
    onboarding_status.set_xalign(0.0);
    onboarding_status.set_wrap(true);

    let id_box = GtkBox::new(Orientation::Horizontal, 8);
    let wayfarer_id_entry = Entry::builder().hexpand(true).editable(false).build();
    wayfarer_id_entry.set_placeholder_text(Some("No Wayfarer ID generated yet"));

    let identity_meta_label = Label::new(Some("Identity metadata: unavailable"));
    identity_meta_label.set_xalign(0.0);
    identity_meta_label.set_wrap(true);

    let generate_button = Button::with_label("Rotate Wayfarer ID");
    generate_button.add_css_class("action");
    let delete_button = Button::with_label("Reset Wayfarer ID");
    delete_button.add_css_class("danger");

    id_box.append(&wayfarer_id_entry);
    id_box.append(&generate_button);
    id_box.append(&delete_button);

    let identity_notice = Label::new(Some(
        "Your Wayfarer ID is your global address. Resetting it is destructive and can break contact reachability unless everyone learns your new ID.",
    ));
    identity_notice.add_css_class("warning");
    identity_notice.set_xalign(0.0);
    identity_notice.set_wrap(true);

    let proceed_button = Button::with_label("Open Settings");
    proceed_button.add_css_class("action");

    onboarding_panel.append(&onboarding_title);
    onboarding_panel.append(&onboarding_status);
    onboarding_panel.append(&id_box);
    onboarding_panel.append(&identity_meta_label);
    onboarding_panel.append(&identity_notice);
    onboarding_panel.append(&proceed_button);

    let diagnostics_panel = GtkBox::new(Orientation::Vertical, 10);
    diagnostics_panel.add_css_class("glass-panel");

    let relay_config_title = Label::new(Some("Relay diagnostics"));
    relay_config_title.add_css_class("section-title");
    relay_config_title.set_xalign(0.0);

    let relay_toggle = CheckButton::with_label("Enable Relay Sync");
    relay_toggle.set_active(initial_settings.relay_sync_enabled);
    let gossip_toggle = CheckButton::with_label("Enable LAN Gossip Sync");
    gossip_toggle.set_active(initial_settings.gossip_sync_enabled);

    let relay_list = ListBox::new();
    relay_list.add_css_class("contact-list");
    let relay_list_order = Rc::new(RefCell::new(Vec::<String>::new()));
    let relay_list_scroll = ScrolledWindow::builder().min_content_height(120).build();
    relay_list_scroll.set_vexpand(true);
    relay_list_scroll.set_child(Some(&relay_list));

    let relay_entry = Entry::builder().hexpand(true).build();
    relay_entry.set_placeholder_text(Some("Relay endpoint (http://host:port)"));

    let relay_actions = GtkBox::new(Orientation::Horizontal, 8);
    let add_relay_button = Button::with_label("Add Relay");
    add_relay_button.add_css_class("compact");
    let remove_relay_button = Button::with_label("Remove Selected");
    remove_relay_button.add_css_class("danger");
    remove_relay_button.set_sensitive(false);
    relay_actions.append(&add_relay_button);
    relay_actions.append(&remove_relay_button);

    let connect_button = Button::with_label("Run Relay Diagnostics");
    connect_button.add_css_class("action");

    let open_logs_button = Button::with_label("Open Log Folder");
    open_logs_button.add_css_class("compact");

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
        .set_text("Diagnostics timeline:\n- waiting for first relay run");

    let diagnostics_scroll = ScrolledWindow::builder().min_content_height(160).build();
    diagnostics_scroll.set_child(Some(&diagnostics_text));

    diagnostics_panel.append(&relay_config_title);
    diagnostics_panel.append(&relay_toggle);
    diagnostics_panel.append(&gossip_toggle);
    diagnostics_panel.append(&relay_list_scroll);
    diagnostics_panel.append(&relay_entry);
    diagnostics_panel.append(&relay_actions);
    diagnostics_panel.append(&connect_button);
    diagnostics_panel.append(&open_logs_button);
    diagnostics_panel.append(&relay_primary_label);
    diagnostics_panel.append(&relay_secondary_label);
    diagnostics_panel.append(&diagnostics_scroll);

    let conversations_panel = GtkBox::new(Orientation::Vertical, 10);
    conversations_panel.add_css_class("glass-panel");

    let conversations_title = Label::new(Some("Chats"));
    conversations_title.add_css_class("section-title");
    conversations_title.set_xalign(0.0);

    let conversations_hint = Label::new(Some(
        "Messages are grouped by wayfarer identity pairs. Select a contact to view the thread.",
    ));
    conversations_hint.set_xalign(0.0);

    conversations_panel.append(&conversations_title);
    conversations_panel.append(&conversations_hint);

    let chat_shell = Paned::new(Orientation::Horizontal);
    chat_shell.add_css_class("chat-shell");
    chat_shell.set_wide_handle(true);
    chat_shell.set_position(300);
    chat_shell.set_resize_start_child(true);
    chat_shell.set_resize_end_child(true);
    chat_shell.set_shrink_start_child(true);
    chat_shell.set_shrink_end_child(true);
    chat_shell.set_vexpand(true);

    let contacts_column = GtkBox::new(Orientation::Vertical, 8);
    contacts_column.add_css_class("contacts-pane");
    contacts_column.set_hexpand(true);
    let contacts_title = Label::new(Some("Contacts"));
    contacts_title.add_css_class("section-title");
    contacts_title.set_xalign(0.0);
    let contacts_list = ListBox::new();
    contacts_list.add_css_class("contact-list");
    let contacts_scroll = ScrolledWindow::builder().min_content_width(260).build();
    contacts_scroll.set_vexpand(true);
    contacts_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    contacts_scroll.set_child(Some(&contacts_list));
    contacts_column.append(&contacts_title);
    contacts_column.append(&contacts_scroll);
    contacts_column.set_vexpand(true);
    let contacts_revealer = Revealer::builder()
        .transition_type(RevealerTransitionType::SlideRight)
        .transition_duration(180)
        .reveal_child(true)
        .build();
    contacts_revealer.set_child(Some(&contacts_column));

    let thread_column = GtkBox::new(Orientation::Vertical, 10);
    thread_column.add_css_class("thread-pane");
    thread_column.set_hexpand(true);
    let thread_title = Label::new(Some("Thread"));
    thread_title.add_css_class("section-title");
    thread_title.set_xalign(0.0);
    let thread_contact_id_label = Label::new(Some(""));
    thread_contact_id_label.add_css_class("thread-contact-id");
    thread_contact_id_label.set_xalign(0.0);
    let compact_contact_picker = ComboBoxText::new();
    compact_contact_picker.add_css_class("compact-contact-picker");
    let compact_picker_revealer = Revealer::builder()
        .transition_type(RevealerTransitionType::SlideDown)
        .transition_duration(180)
        .reveal_child(false)
        .build();
    compact_picker_revealer.set_child(Some(&compact_contact_picker));
    let id_toggle_button = Button::with_label("Show Full ID");
    id_toggle_button.add_css_class("compact");

    let thread_header = GtkBox::new(Orientation::Horizontal, 8);
    let thread_header_labels = GtkBox::new(Orientation::Vertical, 0);
    thread_header_labels.append(&thread_title);
    thread_header_labels.append(&thread_contact_id_label);
    thread_header_labels.append(&compact_picker_revealer);
    thread_header.append(&thread_header_labels);
    thread_header.append(&id_toggle_button);
    let messages_list = ListBox::new();
    messages_list.add_css_class("messages-list");
    messages_list.set_hexpand(true);
    let messages_scroll = ScrolledWindow::builder().build();
    messages_scroll.set_vexpand(true);
    messages_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    messages_scroll.set_child(Some(&messages_list));

    let auto_scroll_locked = Rc::new(Cell::new(false));
    {
        let auto_scroll_locked = Rc::clone(&auto_scroll_locked);
        let adj = messages_scroll.vadjustment();
        adj.connect_value_changed(move |adj| {
            let bottom = (adj.upper() - adj.page_size()).max(adj.lower());
            let distance = bottom - adj.value();
            let user_has_scrolled = adj.value() > adj.lower() + 1.0;
            auto_scroll_locked.set(user_has_scrolled && distance > 12.0);
        });
    }
    thread_column.append(&thread_header);
    thread_column.append(&messages_scroll);

    thread_column.set_vexpand(true);

    chat_shell.set_start_child(Some(&contacts_revealer));
    chat_shell.set_end_child(Some(&thread_column));
    conversations_panel.append(&chat_shell);
    conversations_panel.set_vexpand(true);

    let recipient_entry = Entry::builder().hexpand(true).build();
    recipient_entry.set_placeholder_text(Some("Select a contact to start messaging"));
    recipient_entry.add_css_class("recipient-entry");
    recipient_entry.set_editable(false);

    let body_entry = Entry::builder().hexpand(true).build();
    body_entry.set_placeholder_text(Some("Type a message..."));
    body_entry.add_css_class("message-entry");

    let send_button = Button::with_label("↑");
    send_button.add_css_class("action");
    send_button.add_css_class("send-fab");

    let chat_status_label = Label::new(Some("Ready"));
    chat_status_label.add_css_class("chat-status");
    chat_status_label.set_xalign(0.0);
    chat_status_label.set_hexpand(true);

    let open_chat_logs_button = Button::with_label("Diagnostics Logs");
    open_chat_logs_button.add_css_class("compact");

    let composer_bar = GtkBox::new(Orientation::Horizontal, 8);
    composer_bar.add_css_class("composer-bar");
    composer_bar.append(&body_entry);
    composer_bar.append(&send_button);

    thread_column.append(&composer_bar);
    thread_column.append(&open_chat_logs_button);

    let contacts_panel = GtkBox::new(Orientation::Vertical, 10);
    contacts_panel.add_css_class("glass-panel");
    let contacts_manage_title = Label::new(Some("Contacts"));
    contacts_manage_title.add_css_class("section-title");
    contacts_manage_title.set_xalign(0.0);
    let contacts_manage_hint = Label::new(Some(
        "Add, rename, or remove contacts here. Chats only handles messages.",
    ));
    contacts_manage_hint.set_xalign(0.0);

    let contacts_manage_list = ListBox::new();
    contacts_manage_list.add_css_class("contact-list");
    let contacts_manage_scroll = ScrolledWindow::builder().min_content_height(280).build();
    contacts_manage_scroll.set_child(Some(&contacts_manage_list));
    contacts_manage_scroll.set_vexpand(true);

    let contact_filter_entry = Entry::builder().hexpand(true).build();
    contact_filter_entry.set_placeholder_text(Some("Filter contacts by name or ID"));
    contact_filter_entry.add_css_class("message-entry");

    let selected_contact_id_value = Label::new(Some("No contact selected"));
    selected_contact_id_value.add_css_class("contact-id-value");
    selected_contact_id_value.set_xalign(0.0);

    let new_contact_id_entry = Entry::builder().hexpand(true).build();
    new_contact_id_entry.set_placeholder_text(Some("New contact Wayfarer ID (64 lowercase hex)"));
    let contact_id_label = Label::new(Some("Selected Contact Wayfarer ID"));
    contact_id_label.add_css_class("field-label");
    contact_id_label.set_xalign(0.0);
    let contact_id_hint = Label::new(Some(
        "Current selected contact address. To add a new contact, paste their ID in the field below.",
    ));
    contact_id_hint.add_css_class("field-hint");
    contact_id_hint.set_xalign(0.0);
    contact_id_hint.set_wrap(true);

    let contact_alias_entry = Entry::builder().hexpand(true).build();
    contact_alias_entry.set_placeholder_text(Some("Display name (optional, local only)"));
    let contact_alias_label = Label::new(Some("Display Name (Local)"));
    contact_alias_label.add_css_class("field-label");
    contact_alias_label.set_xalign(0.0);
    let contact_alias_hint = Label::new(Some(
        "Optional nickname stored only on this Linux device. Leave blank to use Wayfarer ID.",
    ));
    contact_alias_hint.add_css_class("field-hint");
    contact_alias_hint.set_xalign(0.0);
    contact_alias_hint.set_wrap(true);

    let contacts_actions = GtkBox::new(Orientation::Horizontal, 8);
    let add_update_contact_button = Button::with_label("Add / Update Contact");
    add_update_contact_button.add_css_class("action");
    let remove_contact_button = Button::with_label("Remove Contact");
    remove_contact_button.add_css_class("danger");
    contacts_actions.append(&add_update_contact_button);
    contacts_actions.append(&remove_contact_button);

    contacts_panel.append(&contacts_manage_title);
    contacts_panel.append(&contacts_manage_hint);
    contacts_panel.append(&contact_filter_entry);
    contacts_panel.append(&contacts_manage_scroll);
    contacts_panel.append(&contact_id_label);
    contacts_panel.append(&contact_id_hint);
    contacts_panel.append(&selected_contact_id_value);
    contacts_panel.append(&new_contact_id_entry);
    contacts_panel.append(&contact_alias_label);
    contacts_panel.append(&contact_alias_hint);
    contacts_panel.append(&contact_alias_entry);
    contacts_panel.append(&contacts_actions);

    let share_panel = GtkBox::new(Orientation::Vertical, 10);
    share_panel.add_css_class("glass-panel");
    let share_title = Label::new(Some("Share"));
    share_title.add_css_class("section-title");
    share_title.set_xalign(0.0);
    let share_hint = Label::new(Some(
        "Share your Wayfarer ID via QR. The code includes your address with Aethos feather mark.",
    ));
    share_hint.set_xalign(0.0);
    share_hint.set_wrap(true);
    let share_wayfarer_entry = Entry::builder().hexpand(true).editable(false).build();
    let copy_wayfarer_button = Button::with_label("Copy Wayfarer ID");
    copy_wayfarer_button.add_css_class("compact");
    let share_qr_image = Image::new();
    share_qr_image.set_pixel_size(280);
    share_qr_image.set_halign(gtk4::Align::Center);
    let share_status_label = Label::new(Some("QR pending identity"));
    share_status_label.set_xalign(0.0);
    share_status_label.add_css_class("chat-status");

    share_panel.append(&share_title);
    share_panel.append(&share_hint);
    share_panel.append(&share_wayfarer_entry);
    share_panel.append(&copy_wayfarer_button);
    share_panel.append(&share_qr_image);
    share_panel.append(&share_status_label);

    let settings_panel = GtkBox::new(Orientation::Vertical, 12);
    settings_panel.add_css_class("settings-shell");

    let account_group_title = Label::new(Some("Identity & Account"));
    account_group_title.add_css_class("settings-group-title");
    account_group_title.set_xalign(0.0);
    let account_group_hint = Label::new(Some(
        "Manage your local Wayfarer identity and safety-critical reset actions.",
    ));
    account_group_hint.add_css_class("settings-group-hint");
    account_group_hint.set_xalign(0.0);
    onboarding_panel.add_css_class("settings-card");

    let relay_group_title = Label::new(Some("Relay & Connectivity"));
    relay_group_title.add_css_class("settings-group-title");
    relay_group_title.set_xalign(0.0);
    let relay_group_hint = Label::new(Some(
        "Diagnostics and endpoint health for your configured relays.",
    ));
    relay_group_hint.add_css_class("settings-group-hint");
    relay_group_hint.set_xalign(0.0);
    diagnostics_panel.add_css_class("settings-card");

    settings_panel.append(&account_group_title);
    settings_panel.append(&account_group_hint);
    settings_panel.append(&onboarding_panel);
    settings_panel.append(&relay_group_title);
    settings_panel.append(&relay_group_hint);
    settings_panel.append(&diagnostics_panel);
    let settings_scroll = ScrolledWindow::builder().build();
    settings_scroll.add_css_class("settings-scroll");
    settings_scroll.set_child(Some(&settings_panel));
    settings_scroll.set_vexpand(true);

    views.add_titled(&conversations_panel, Some("sessions"), "Chats");
    views.add_titled(&contacts_panel, Some("contacts"), "Contacts");
    views.add_titled(&share_panel, Some("share"), "Share");
    views.add_titled(&settings_scroll, Some("settings"), "Settings");

    let footer = GtkBox::new(Orientation::Horizontal, 10);
    footer.add_css_class("footer-bar");
    footer.set_hexpand(true);
    footer.set_vexpand(false);
    footer.set_valign(gtk4::Align::End);
    footer.set_size_request(-1, 28);

    let footer_note = Label::new(Some(&build_version_text()));
    footer_note.add_css_class("footer-note");
    footer_note.set_xalign(0.0);
    footer_note.set_hexpand(true);

    let footer_logo = Image::from_file("src/img/logo.png");
    footer_logo.add_css_class("footer-logo");
    footer_logo.set_pixel_size(22);
    footer_logo.set_size_request(22, 22);
    footer_logo.set_hexpand(false);
    footer_logo.set_vexpand(false);
    footer_logo.set_halign(gtk4::Align::End);

    footer.append(&footer_note);
    footer.append(&footer_logo);

    root.append(&top_bar);
    root.append(&wave_strip);
    root.append(&wave_mode_label);
    root.append(&views);
    root.append(&footer);
    window.set_child(Some(&root));
    views.set_visible_child_name("sessions");

    if let Ok(identity) = ensure_local_identity() {
        wayfarer_id_entry.set_text(&identity.wayfarer_id);
        share_wayfarer_entry.set_text(&identity.wayfarer_id);
        refresh_share_qr(&identity.wayfarer_id, &share_qr_image, &share_status_label);
        let key_preview: String = identity.verifying_key_b64.chars().take(16).collect();
        let device_preview: String = identity.device_id.chars().take(12).collect();
        identity_meta_label.set_text(&format!(
            "Identity metadata: device={} · device_id={}… · verify_key={}…",
            identity.device_name, device_preview, key_preview
        ));
        onboarding_status
            .set_text("Step 2/2 · Identity provisioned. Proceed to relay diagnostics.");
    }

    {
        let share_wayfarer_entry = share_wayfarer_entry.clone();
        let copy_wayfarer_button = copy_wayfarer_button.clone();
        let copy_wayfarer_popover_anchor = copy_wayfarer_button.clone();
        copy_wayfarer_button.connect_clicked(move |_| {
            if let Some(display) = Display::default() {
                display
                    .clipboard()
                    .set_text(share_wayfarer_entry.text().as_ref());
                show_copied_popover(&copy_wayfarer_popover_anchor);
            }
        });
    }

    if let Ok(Some(cache)) = load_relay_session_cache() {
        relay_primary_label.set_text(&format!("Primary relay status: {}", cache.primary_status));
        relay_secondary_label.set_text(&format!(
            "Secondary relay status: {}",
            cache.secondary_status
        ));
        update_relay_chip(
            &cache.primary_status,
            &cache.secondary_status,
            &relay_dot,
            &relay_chip_text,
            &relay_chip,
        );
    } else {
        update_relay_chip("idle", "idle", &relay_dot, &relay_chip_text, &relay_chip);
    }

    if !relay_sync_enabled.load(Ordering::SeqCst) {
        relay_primary_label.set_text("Primary relay status: relay sync disabled in settings");
        relay_secondary_label.set_text("Secondary relay status: relay sync disabled in settings");
        set_relay_chip_disabled(&relay_dot, &relay_chip_text, &relay_chip);
        connect_button.set_sensitive(false);
    }

    if gossip_sync_enabled.load(Ordering::SeqCst) {
        set_gossip_chip_listening(&gossip_dot, &gossip_chip_text, &gossip_chip);
    } else {
        set_gossip_chip_disabled(&gossip_dot, &gossip_chip_text, &gossip_chip);
    }

    render_relay_list(
        &app_settings.borrow().relay_endpoints,
        &relay_list,
        &relay_list_order,
    );

    let chat_state = Rc::new(RefCell::new(ChatState::default()));
    let contact_order = Rc::new(RefCell::new(Vec::<String>::new()));
    let contacts_manage_order = Rc::new(RefCell::new(Vec::<String>::new()));
    let picker_syncing = Rc::new(Cell::new(false));

    if let Ok(aliases) = load_contact_aliases() {
        chat_state.borrow_mut().contact_aliases = aliases;
    }

    if let Ok(Some(saved_chat)) = load_persisted_chat_state() {
        let mut state = chat_state.borrow_mut();
        state.threads = saved_chat.threads;
        state.selected_contact = saved_chat.selected_contact;
    }

    wave_pending.set(has_pending_outbound(&chat_state.borrow()));
    wave_mode_label.set_text(if wave_pending.get() {
        "Outbound flow active"
    } else {
        "Ambient mesh active"
    });

    let first_contact = chat_state.borrow().contact_aliases.keys().next().cloned();
    if let Some(first_contact) = first_contact {
        if chat_state.borrow().selected_contact.is_none() {
            chat_state.borrow_mut().selected_contact = Some(first_contact.clone());
        }
        recipient_entry.set_text(&first_contact);
    }

    render_contacts(&chat_state.borrow(), &contacts_list, &contact_order);
    render_contacts_manager(
        &chat_state.borrow(),
        &contacts_manage_list,
        &contacts_manage_order,
    );
    picker_syncing.set(true);
    sync_contact_picker(
        &chat_state.borrow(),
        &compact_contact_picker,
        &contact_order,
    );
    picker_syncing.set(false);
    sync_contact_form(
        &chat_state.borrow(),
        &selected_contact_id_value,
        &contact_alias_entry,
    );
    update_contact_action_buttons(
        &chat_state.borrow(),
        &new_contact_id_entry,
        &add_update_contact_button,
        &remove_contact_button,
    );
    render_messages(
        &chat_state.borrow(),
        MessageRenderUi {
            messages_list: &messages_list,
            messages_scroll: &messages_scroll,
            thread_title: &thread_title,
            thread_contact_id_label: &thread_contact_id_label,
            send_button: &send_button,
            body_entry: &body_entry,
            auto_scroll_locked: &auto_scroll_locked,
        },
    );

    {
        let chat_state = Rc::clone(&chat_state);
        let contact_order = Rc::clone(&contact_order);
        let messages_list = messages_list.clone();
        let messages_scroll = messages_scroll.clone();
        let thread_title = thread_title.clone();
        let thread_contact_id_label = thread_contact_id_label.clone();
        let recipient_entry = recipient_entry.clone();
        let compact_contact_picker = compact_contact_picker.clone();
        let selected_contact_id_value = selected_contact_id_value.clone();
        let contact_alias_entry = contact_alias_entry.clone();
        let new_contact_id_entry = new_contact_id_entry.clone();
        let add_update_contact_button = add_update_contact_button.clone();
        let remove_contact_button = remove_contact_button.clone();
        let send_button = send_button.clone();
        let body_entry = body_entry.clone();
        let auto_scroll_locked = Rc::clone(&auto_scroll_locked);
        let picker_syncing = Rc::clone(&picker_syncing);
        contacts_list.connect_row_selected(move |_list, row| {
            let Some(row) = row else {
                return;
            };

            let idx = row.index();
            if idx < 0 {
                return;
            }

            let selected = contact_order.borrow().get(idx as usize).cloned();
            if let Some(contact_id) = selected {
                {
                    let mut state = chat_state.borrow_mut();
                    state.selected_contact = Some(contact_id.clone());
                    state.new_contacts.remove(&contact_id);
                    mark_contact_seen(&mut state, &contact_id);
                }
                auto_scroll_locked.set(false);
                let _ = save_persisted_chat_state(&chat_state.borrow());
                if let Some(selected_contact) = chat_state.borrow().selected_contact.as_ref() {
                    recipient_entry.set_text(selected_contact);
                }
                picker_syncing.set(true);
                sync_contact_picker(
                    &chat_state.borrow(),
                    &compact_contact_picker,
                    &contact_order,
                );
                picker_syncing.set(false);
                sync_contact_form(
                    &chat_state.borrow(),
                    &selected_contact_id_value,
                    &contact_alias_entry,
                );
                update_contact_action_buttons(
                    &chat_state.borrow(),
                    &new_contact_id_entry,
                    &add_update_contact_button,
                    &remove_contact_button,
                );
                render_messages(
                    &chat_state.borrow(),
                    MessageRenderUi {
                        messages_list: &messages_list,
                        messages_scroll: &messages_scroll,
                        thread_title: &thread_title,
                        thread_contact_id_label: &thread_contact_id_label,
                        send_button: &send_button,
                        body_entry: &body_entry,
                        auto_scroll_locked: &auto_scroll_locked,
                    },
                );
            }
        });
    }

    {
        let chat_state = Rc::clone(&chat_state);
        let contact_order = Rc::clone(&contact_order);
        let messages_list = messages_list.clone();
        let messages_scroll = messages_scroll.clone();
        let thread_title = thread_title.clone();
        let thread_contact_id_label = thread_contact_id_label.clone();
        let recipient_entry = recipient_entry.clone();
        let selected_contact_id_value = selected_contact_id_value.clone();
        let contact_alias_entry = contact_alias_entry.clone();
        let new_contact_id_entry = new_contact_id_entry.clone();
        let add_update_contact_button = add_update_contact_button.clone();
        let remove_contact_button = remove_contact_button.clone();
        let send_button = send_button.clone();
        let body_entry = body_entry.clone();
        let auto_scroll_locked = Rc::clone(&auto_scroll_locked);
        let picker_syncing = Rc::clone(&picker_syncing);
        compact_contact_picker.connect_changed(move |picker| {
            if picker_syncing.get() {
                return;
            }
            let Some(active_id) = picker.active_id() else {
                return;
            };
            let active_id = active_id.to_string();
            if !contact_order
                .borrow()
                .iter()
                .any(|contact| contact == &active_id)
            {
                return;
            }

            {
                let mut state = chat_state.borrow_mut();
                state.selected_contact = Some(active_id.clone());
                state.new_contacts.remove(&active_id);
                mark_contact_seen(&mut state, &active_id);
            }
            auto_scroll_locked.set(false);
            let _ = save_persisted_chat_state(&chat_state.borrow());
            recipient_entry.set_text(&active_id);
            sync_contact_form(
                &chat_state.borrow(),
                &selected_contact_id_value,
                &contact_alias_entry,
            );
            update_contact_action_buttons(
                &chat_state.borrow(),
                &new_contact_id_entry,
                &add_update_contact_button,
                &remove_contact_button,
            );
            render_messages(
                &chat_state.borrow(),
                MessageRenderUi {
                    messages_list: &messages_list,
                    messages_scroll: &messages_scroll,
                    thread_title: &thread_title,
                    thread_contact_id_label: &thread_contact_id_label,
                    send_button: &send_button,
                    body_entry: &body_entry,
                    auto_scroll_locked: &auto_scroll_locked,
                },
            );
        });
    }

    {
        let chat_state = Rc::clone(&chat_state);
        let contact_order = Rc::clone(&contact_order);
        let contacts_manage_order = Rc::clone(&contacts_manage_order);
        let contacts_list = contacts_list.clone();
        let contacts_manage_list = contacts_manage_list.clone();
        let compact_contact_picker = compact_contact_picker.clone();
        let selected_contact_id_value = selected_contact_id_value.clone();
        let new_contact_id_entry = new_contact_id_entry.clone();
        let contact_alias_entry = contact_alias_entry.clone();
        let chat_status_label = chat_status_label.clone();
        let picker_syncing = Rc::clone(&picker_syncing);
        let recipient_entry = recipient_entry.clone();
        let add_update_contact_button_for_click = add_update_contact_button.clone();
        let add_update_contact_button_state = add_update_contact_button_for_click.clone();
        let remove_contact_button_state = remove_contact_button.clone();
        add_update_contact_button_for_click.connect_clicked(move |_| {
            let typed_contact_id = new_contact_id_entry.text().trim().to_string();
            let contact_id = if typed_contact_id.is_empty() {
                match chat_state.borrow().selected_contact.clone() {
                    Some(selected) => selected,
                    None => {
                        chat_status_label
                            .set_text("Enter a contact ID to add, or select a contact to update");
                        return;
                    }
                }
            } else {
                typed_contact_id
            };
            if !is_valid_wayfarer_id(&contact_id) {
                chat_status_label
                    .set_text("invalid contact id: expected 64 lowercase hex characters");
                return;
            }

            let alias_input = contact_alias_entry.text().trim().to_string();
            let alias = if alias_input.is_empty() {
                contact_id.clone()
            } else {
                alias_input
            };
            {
                let mut state = chat_state.borrow_mut();
                state.contact_aliases.insert(contact_id.clone(), alias);
                state.selected_contact = Some(contact_id.clone());
                state.new_contacts.remove(&contact_id);
                mark_contact_seen(&mut state, &contact_id);

                if let Err(err) = save_contact_aliases(&state.contact_aliases) {
                    chat_status_label.set_text(&format!("failed to save contact name: {err}"));
                    return;
                }
            }
            if let Err(err) = save_persisted_chat_state(&chat_state.borrow()) {
                chat_status_label.set_text(&format!("failed to persist chat state: {err}"));
                return;
            }

            render_contacts(&chat_state.borrow(), &contacts_list, &contact_order);
            render_contacts_manager(
                &chat_state.borrow(),
                &contacts_manage_list,
                &contacts_manage_order,
            );
            picker_syncing.set(true);
            sync_contact_picker(
                &chat_state.borrow(),
                &compact_contact_picker,
                &contact_order,
            );
            picker_syncing.set(false);
            sync_contact_form(
                &chat_state.borrow(),
                &selected_contact_id_value,
                &contact_alias_entry,
            );
            new_contact_id_entry.set_text("");
            update_contact_action_buttons(
                &chat_state.borrow(),
                &new_contact_id_entry,
                &add_update_contact_button_state,
                &remove_contact_button_state,
            );
            if let Some(selected) = chat_state.borrow().selected_contact.as_ref() {
                recipient_entry.set_text(selected);
            }
            chat_status_label.set_text("Contact saved locally");
        });
    }

    {
        let chat_state = Rc::clone(&chat_state);
        let contact_order = Rc::clone(&contact_order);
        let contacts_manage_order = Rc::clone(&contacts_manage_order);
        let window = window.clone();
        let contacts_list = contacts_list.clone();
        let contacts_manage_list = contacts_manage_list.clone();
        let compact_contact_picker = compact_contact_picker.clone();
        let selected_contact_id_value = selected_contact_id_value.clone();
        let contact_alias_entry = contact_alias_entry.clone();
        let chat_status_label = chat_status_label.clone();
        let picker_syncing = Rc::clone(&picker_syncing);
        let recipient_entry = recipient_entry.clone();
        let new_contact_id_entry = new_contact_id_entry.clone();
        let remove_contact_button_for_click = remove_contact_button.clone();
        let add_update_contact_button_state = add_update_contact_button.clone();
        let remove_contact_button_state = remove_contact_button_for_click.clone();
        remove_contact_button_for_click.connect_clicked(move |_| {
            let contact_id = match chat_state.borrow().selected_contact.clone() {
                Some(selected) => selected,
                None => {
                    chat_status_label.set_text("Select a contact to remove");
                    return;
                }
            };

            let display_name = {
                let state = chat_state.borrow();
                contact_display_name(&state, &contact_id)
            };

            let dialog = Dialog::builder()
                .transient_for(&window)
                .modal(true)
                .title("Remove Contact?")
                .build();
            dialog.add_button("Cancel", ResponseType::Cancel);
            dialog.add_button("Remove Contact", ResponseType::Accept);

            let content = dialog.content_area();
            let warning = Label::new(Some(&format!(
                "Remove {display_name} ({}) and local thread history? This cannot be undone.",
                tiny_wayfarer(&contact_id)
            )));
            warning.set_wrap(true);
            warning.set_xalign(0.0);
            warning.add_css_class("warning");
            content.append(&warning);

            let chat_state = Rc::clone(&chat_state);
            let contact_order = Rc::clone(&contact_order);
            let contacts_manage_order = Rc::clone(&contacts_manage_order);
            let contacts_list = contacts_list.clone();
            let contacts_manage_list = contacts_manage_list.clone();
            let compact_contact_picker = compact_contact_picker.clone();
            let selected_contact_id_value = selected_contact_id_value.clone();
            let contact_alias_entry = contact_alias_entry.clone();
            let chat_status_label = chat_status_label.clone();
            let picker_syncing = Rc::clone(&picker_syncing);
            let recipient_entry = recipient_entry.clone();
            let new_contact_id_entry_for_dialog = new_contact_id_entry.clone();
            let add_update_contact_button_for_dialog = add_update_contact_button_state.clone();
            let remove_contact_button_for_dialog = remove_contact_button_state.clone();
            dialog.connect_response(move |dialog, response| {
                if response != ResponseType::Accept {
                    dialog.close();
                    return;
                }

                let contact_id = contact_id.clone();
                {
                    let mut state = chat_state.borrow_mut();
                    state.contact_aliases.remove(&contact_id);
                    state.threads.remove(&contact_id);
                    state.new_contacts.remove(&contact_id);
                    if state.selected_contact.as_deref() == Some(contact_id.as_str()) {
                        state.selected_contact = state.contact_aliases.keys().next().cloned();
                        if let Some(next_contact) = state.selected_contact.clone() {
                            mark_contact_seen(&mut state, &next_contact);
                        }
                    }
                    if let Err(err) = save_contact_aliases(&state.contact_aliases) {
                        chat_status_label.set_text(&format!("failed to remove contact: {err}"));
                        dialog.close();
                        return;
                    }
                }
                if let Err(err) = save_persisted_chat_state(&chat_state.borrow()) {
                    chat_status_label.set_text(&format!("failed to persist chat state: {err}"));
                    dialog.close();
                    return;
                }

                render_contacts(&chat_state.borrow(), &contacts_list, &contact_order);
                render_contacts_manager(
                    &chat_state.borrow(),
                    &contacts_manage_list,
                    &contacts_manage_order,
                );
                picker_syncing.set(true);
                sync_contact_picker(
                    &chat_state.borrow(),
                    &compact_contact_picker,
                    &contact_order,
                );
                picker_syncing.set(false);
                sync_contact_form(
                    &chat_state.borrow(),
                    &selected_contact_id_value,
                    &contact_alias_entry,
                );
                update_contact_action_buttons(
                    &chat_state.borrow(),
                    &new_contact_id_entry_for_dialog,
                    &add_update_contact_button_for_dialog,
                    &remove_contact_button_for_dialog,
                );
                if let Some(selected) = chat_state.borrow().selected_contact.as_ref() {
                    recipient_entry.set_text(selected);
                } else {
                    recipient_entry.set_text("");
                }
                chat_status_label.set_text("Contact removed locally");
                dialog.close();
            });
            dialog.present();
        });
    }

    {
        let add_update_contact_button = add_update_contact_button.clone();
        new_contact_id_entry.connect_activate(move |_| {
            add_update_contact_button.emit_clicked();
        });
    }

    {
        let add_update_contact_button = add_update_contact_button.clone();
        contact_alias_entry.connect_activate(move |_| {
            add_update_contact_button.emit_clicked();
        });
    }

    {
        let chat_state = Rc::clone(&chat_state);
        let contacts_manage_order = Rc::clone(&contacts_manage_order);
        let contact_order = Rc::clone(&contact_order);
        let contacts_list = contacts_list.clone();
        let selected_contact_id_value = selected_contact_id_value.clone();
        let contact_alias_entry = contact_alias_entry.clone();
        let new_contact_id_entry = new_contact_id_entry.clone();
        let add_update_contact_button = add_update_contact_button.clone();
        let remove_contact_button = remove_contact_button.clone();
        let recipient_entry = recipient_entry.clone();
        let compact_contact_picker = compact_contact_picker.clone();
        let picker_syncing = Rc::clone(&picker_syncing);
        let messages_list = messages_list.clone();
        let messages_scroll = messages_scroll.clone();
        let thread_title = thread_title.clone();
        let thread_contact_id_label = thread_contact_id_label.clone();
        let send_button = send_button.clone();
        let body_entry = body_entry.clone();
        let auto_scroll_locked = Rc::clone(&auto_scroll_locked);
        let contacts_manage_list = contacts_manage_list.clone();
        contacts_manage_list.connect_row_selected(move |_list, row| {
            let Some(row) = row else {
                return;
            };

            let idx = row.index();
            if idx < 0 {
                return;
            }

            if let Some(contact_id) = contacts_manage_order.borrow().get(idx as usize).cloned() {
                {
                    let mut state = chat_state.borrow_mut();
                    state.selected_contact = Some(contact_id.clone());
                    state.new_contacts.remove(&contact_id);
                    mark_contact_seen(&mut state, &contact_id);
                }
                auto_scroll_locked.set(false);
                let _ = save_persisted_chat_state(&chat_state.borrow());
                if let Some(selected) = chat_state.borrow().selected_contact.as_ref() {
                    recipient_entry.set_text(selected);
                }
                render_contacts(&chat_state.borrow(), &contacts_list, &contact_order);
                picker_syncing.set(true);
                sync_contact_picker(
                    &chat_state.borrow(),
                    &compact_contact_picker,
                    &contact_order,
                );
                picker_syncing.set(false);
                sync_contact_form(
                    &chat_state.borrow(),
                    &selected_contact_id_value,
                    &contact_alias_entry,
                );
                update_contact_action_buttons(
                    &chat_state.borrow(),
                    &new_contact_id_entry,
                    &add_update_contact_button,
                    &remove_contact_button,
                );
                render_messages(
                    &chat_state.borrow(),
                    MessageRenderUi {
                        messages_list: &messages_list,
                        messages_scroll: &messages_scroll,
                        thread_title: &thread_title,
                        thread_contact_id_label: &thread_contact_id_label,
                        send_button: &send_button,
                        body_entry: &body_entry,
                        auto_scroll_locked: &auto_scroll_locked,
                    },
                );
            }
        });
    }

    {
        let chat_state = Rc::clone(&chat_state);
        let messages_list = messages_list.clone();
        let messages_scroll = messages_scroll.clone();
        let thread_title = thread_title.clone();
        let thread_contact_id_label = thread_contact_id_label.clone();
        let id_toggle_button = id_toggle_button.clone();
        let send_button = send_button.clone();
        let body_entry = body_entry.clone();
        let auto_scroll_locked = Rc::clone(&auto_scroll_locked);
        id_toggle_button.connect_clicked(move |button| {
            {
                let mut state = chat_state.borrow_mut();
                state.show_full_contact_id = !state.show_full_contact_id;
                if state.show_full_contact_id {
                    button.set_label("Show Short ID");
                } else {
                    button.set_label("Show Full ID");
                }
            }
            render_messages(
                &chat_state.borrow(),
                MessageRenderUi {
                    messages_list: &messages_list,
                    messages_scroll: &messages_scroll,
                    thread_title: &thread_title,
                    thread_contact_id_label: &thread_contact_id_label,
                    send_button: &send_button,
                    body_entry: &body_entry,
                    auto_scroll_locked: &auto_scroll_locked,
                },
            );
        });
    }

    let (tx, rx) = channel::<RelayStatus>();

    {
        let window = window.clone();
        let wayfarer_id_entry = wayfarer_id_entry.clone();
        let identity_meta_label = identity_meta_label.clone();
        let onboarding_status = onboarding_status.clone();
        let share_wayfarer_entry = share_wayfarer_entry.clone();
        let share_qr_image = share_qr_image.clone();
        let share_status_label = share_status_label.clone();
        generate_button.connect_clicked(move |_| {
            let dialog = Dialog::builder()
                .transient_for(&window)
                .modal(true)
                .title("Rotate Wayfarer ID?")
                .build();
            dialog.add_button("Cancel", ResponseType::Cancel);
            dialog.add_button("Rotate ID", ResponseType::Accept);

            let content = dialog.content_area();
            let warning = Label::new(Some(
                "Rotating creates a new Wayfarer address. Existing contacts may not reach you until they update to your new ID.",
            ));
            warning.set_wrap(true);
            warning.set_xalign(0.0);
            warning.add_css_class("warning");
            content.append(&warning);

            let wayfarer_id_entry = wayfarer_id_entry.clone();
            let identity_meta_label = identity_meta_label.clone();
            let onboarding_status = onboarding_status.clone();
            let share_wayfarer_entry = share_wayfarer_entry.clone();
            let share_qr_image = share_qr_image.clone();
            let share_status_label = share_status_label.clone();
            dialog.connect_response(move |dialog, response| {
                if response == ResponseType::Accept {
                    match regenerate_local_identity() {
                        Ok(identity) => {
                            let key_preview: String =
                                identity.verifying_key_b64.chars().take(16).collect();
                            let device_preview: String = identity.device_id.chars().take(12).collect();
                            wayfarer_id_entry.set_text(&identity.wayfarer_id);
                            share_wayfarer_entry.set_text(&identity.wayfarer_id);
                            refresh_share_qr(
                                &identity.wayfarer_id,
                                &share_qr_image,
                                &share_status_label,
                            );
                            identity_meta_label.set_text(&format!(
                                "Identity metadata: device={} · device_id={}… · verify_key={}…",
                                identity.device_name, device_preview, key_preview
                            ));
                            onboarding_status.set_text(
                                "Step 2/2 · Identity rotated. Share your new Wayfarer ID with contacts.",
                            );
                        }
                        Err(err) => eprintln!("{err}"),
                    }
                }
                dialog.close();
            });
            dialog.present();
        });
    }

    {
        let window = window.clone();
        let wayfarer_id_entry = wayfarer_id_entry.clone();
        let identity_meta_label = identity_meta_label.clone();
        let onboarding_status = onboarding_status.clone();
        let share_wayfarer_entry = share_wayfarer_entry.clone();
        let share_qr_image = share_qr_image.clone();
        let share_status_label = share_status_label.clone();
        delete_button.connect_clicked(move |_| {
            let dialog = Dialog::builder()
                .transient_for(&window)
                .modal(true)
                .title("Reset Wayfarer ID?")
                .build();
            dialog.add_button("Cancel", ResponseType::Cancel);
            dialog.add_button("Reset ID", ResponseType::Accept);

            let content = dialog.content_area();
            let warning = Label::new(Some(
                "Your Wayfarer ID is your address. Resetting it is destructive: existing contacts will not know your new ID and may keep sending to the old one.",
            ));
            warning.set_wrap(true);
            warning.set_xalign(0.0);
            warning.add_css_class("warning");
            content.append(&warning);

            let wayfarer_id_entry = wayfarer_id_entry.clone();
            let identity_meta_label = identity_meta_label.clone();
            let onboarding_status = onboarding_status.clone();
            let share_wayfarer_entry = share_wayfarer_entry.clone();
            let share_qr_image = share_qr_image.clone();
            let share_status_label = share_status_label.clone();
            dialog.connect_response(move |dialog, response| {
                if response == ResponseType::Accept {
                    if let Err(err) = delete_wayfarer_id() {
                        eprintln!("{err}");
                    }

                    match regenerate_local_identity() {
                        Ok(identity) => {
                            let key_preview: String =
                                identity.verifying_key_b64.chars().take(16).collect();
                            let device_preview: String = identity.device_id.chars().take(12).collect();
                            wayfarer_id_entry.set_text(&identity.wayfarer_id);
                            share_wayfarer_entry.set_text(&identity.wayfarer_id);
                            refresh_share_qr(
                                &identity.wayfarer_id,
                                &share_qr_image,
                                &share_status_label,
                            );
                            identity_meta_label.set_text(&format!(
                                "Identity metadata: device={} · device_id={}… · verify_key={}…",
                                identity.device_name, device_preview, key_preview
                            ));
                            onboarding_status.set_text(
                                "Step 2/2 · Identity reset complete. Share your new Wayfarer ID with contacts.",
                            );
                        }
                        Err(err) => {
                            eprintln!("{err}");
                            wayfarer_id_entry.set_text("");
                            share_wayfarer_entry.set_text("");
                            share_qr_image.set_from_file(Option::<&str>::None);
                            share_status_label.set_text("QR unavailable: identity missing");
                            identity_meta_label.set_text("Identity metadata: unavailable");
                        }
                    }
                }
                dialog.close();
            });

            dialog.present();
        });
    }

    {
        let views = views.clone();
        proceed_button.connect_clicked(move |_| {
            views.set_visible_child_name("settings");
        });
    }

    {
        let chat_state = Rc::clone(&chat_state);
        let new_contact_id_entry = new_contact_id_entry.clone();
        let add_update_contact_button = add_update_contact_button.clone();
        let remove_contact_button = remove_contact_button.clone();
        let new_contact_id_entry_state = new_contact_id_entry.clone();
        new_contact_id_entry.connect_changed(move |_| {
            update_contact_action_buttons(
                &chat_state.borrow(),
                &new_contact_id_entry_state,
                &add_update_contact_button,
                &remove_contact_button,
            );
        });
    }

    {
        let chat_state = Rc::clone(&chat_state);
        let contacts_manage_list = contacts_manage_list.clone();
        let contacts_manage_order = Rc::clone(&contacts_manage_order);
        let contact_filter_entry = contact_filter_entry.clone();
        let contact_filter_entry_state = contact_filter_entry.clone();
        contact_filter_entry.connect_changed(move |_| {
            chat_state.borrow_mut().contact_filter = contact_filter_entry_state.text().to_string();
            render_contacts_manager(
                &chat_state.borrow(),
                &contacts_manage_list,
                &contacts_manage_order,
            );

            let selected_contact = chat_state.borrow().selected_contact.clone();
            if let Some(selected_contact) = selected_contact {
                let selected_index = contacts_manage_order
                    .borrow()
                    .iter()
                    .position(|contact| contact == &selected_contact);
                if let Some(index) = selected_index {
                    if let Some(row) = contacts_manage_list.row_at_index(index as i32) {
                        contacts_manage_list.select_row(Some(&row));
                    }
                } else {
                    contacts_manage_list.select_row(Option::<&ListBoxRow>::None);
                }
            } else {
                contacts_manage_list.select_row(Option::<&ListBoxRow>::None);
            }
        });
    }

    {
        let chat_state = Rc::clone(&chat_state);
        let contacts_manage_order = Rc::clone(&contacts_manage_order);
        let new_contact_id_entry = new_contact_id_entry.clone();
        let selected_contact_id_value = selected_contact_id_value.clone();
        let contact_alias_entry = contact_alias_entry.clone();
        let contacts_manage_list = contacts_manage_list.clone();
        let contact_filter_entry = contact_filter_entry.clone();
        let add_update_contact_button = add_update_contact_button.clone();
        let remove_contact_button = remove_contact_button.clone();
        views.connect_visible_child_name_notify(move |stack| {
            if stack.visible_child_name().as_deref() == Some("contacts") {
                new_contact_id_entry.set_text("");
                contact_filter_entry.set_text(&chat_state.borrow().contact_filter);
                sync_contact_form(
                    &chat_state.borrow(),
                    &selected_contact_id_value,
                    &contact_alias_entry,
                );

                let selected_contact = chat_state.borrow().selected_contact.clone();
                if let Some(selected_contact) = selected_contact {
                    let selected_index = contacts_manage_order
                        .borrow()
                        .iter()
                        .position(|contact| contact == &selected_contact);
                    if let Some(index) = selected_index {
                        if let Some(row) = contacts_manage_list.row_at_index(index as i32) {
                            contacts_manage_list.select_row(Some(&row));
                        }
                    } else {
                        contacts_manage_list.select_row(Option::<&ListBoxRow>::None);
                    }
                } else {
                    contacts_manage_list.select_row(Option::<&ListBoxRow>::None);
                }
                update_contact_action_buttons(
                    &chat_state.borrow(),
                    &new_contact_id_entry,
                    &add_update_contact_button,
                    &remove_contact_button,
                );
            }
        });
    }

    {
        let relay_secondary_label = relay_secondary_label.clone();
        open_logs_button.connect_clicked(move |_| match open_log_folder() {
            Ok(_) => {
                relay_secondary_label.set_text("Secondary relay status: opened logs folder");
            }
            Err(err) => {
                relay_secondary_label.set_text(&format!(
                    "Secondary relay status: failed to open log folder: {err}"
                ));
            }
        });
    }

    {
        let chat_status_label = chat_status_label.clone();
        open_chat_logs_button.connect_clicked(move |_| match open_log_folder() {
            Ok(_) => {
                chat_status_label.set_text("Opened log folder");
            }
            Err(err) => {
                chat_status_label.set_text(&format!("Failed to open log folder: {err}"));
            }
        });
    }

    {
        let app_settings = Rc::clone(&app_settings);
        let relay_sync_enabled = Arc::clone(&relay_sync_enabled);
        let relay_primary_label = relay_primary_label.clone();
        let relay_secondary_label = relay_secondary_label.clone();
        let relay_dot = relay_dot.clone();
        let relay_chip_text = relay_chip_text.clone();
        let relay_chip = relay_chip.clone();
        let connect_button = connect_button.clone();
        relay_toggle.connect_toggled(move |toggle| {
            let settings_snapshot =
                update_relay_sync_setting(&app_settings, &relay_sync_enabled, toggle.is_active());

            if settings_snapshot.relay_sync_enabled {
                relay_primary_label.set_text("Primary relay status: idle");
                relay_secondary_label.set_text("Secondary relay status: idle");
                update_relay_chip("idle", "idle", &relay_dot, &relay_chip_text, &relay_chip);
                connect_button.set_sensitive(true);
                connect_button.emit_clicked();
            } else {
                relay_primary_label
                    .set_text("Primary relay status: relay sync disabled in settings");
                relay_secondary_label
                    .set_text("Secondary relay status: relay sync disabled in settings");
                set_relay_chip_disabled(&relay_dot, &relay_chip_text, &relay_chip);
                connect_button.set_sensitive(false);
            }

            if let Err(err) = save_app_settings(&settings_snapshot) {
                append_local_log(&format!("save_settings_failed: {err}"));
            }
        });
    }

    {
        let app_settings = Rc::clone(&app_settings);
        let gossip_sync_enabled = Arc::clone(&gossip_sync_enabled);
        let gossip_force_announce = Arc::clone(&gossip_force_announce);
        let gossip_dot = gossip_dot.clone();
        let gossip_chip_text = gossip_chip_text.clone();
        let gossip_chip = gossip_chip.clone();
        gossip_toggle.connect_toggled(move |toggle| {
            let mut settings = app_settings.borrow_mut();
            settings.gossip_sync_enabled = toggle.is_active();
            gossip_sync_enabled.store(settings.gossip_sync_enabled, Ordering::SeqCst);
            if settings.gossip_sync_enabled {
                gossip_force_announce.store(true, Ordering::SeqCst);
                set_gossip_chip_listening(&gossip_dot, &gossip_chip_text, &gossip_chip);
            } else {
                set_gossip_chip_disabled(&gossip_dot, &gossip_chip_text, &gossip_chip);
            }
            if let Err(err) = save_app_settings(&settings) {
                append_local_log(&format!("save_settings_failed: {err}"));
            }
        });
    }

    {
        let gossip_sync_enabled = Arc::clone(&gossip_sync_enabled);
        let gossip_last_activity_ms = Arc::clone(&gossip_last_activity_ms);
        let gossip_dot = gossip_dot.clone();
        let gossip_chip_text = gossip_chip_text.clone();
        let gossip_chip = gossip_chip.clone();
        glib::timeout_add_local(Duration::from_millis(600), move || {
            if !gossip_sync_enabled.load(Ordering::SeqCst) {
                set_gossip_chip_disabled(&gossip_dot, &gossip_chip_text, &gossip_chip);
                return glib::ControlFlow::Continue;
            }

            let now = now_unix_ms();
            let last = gossip_last_activity_ms.load(Ordering::SeqCst);
            if last > 0 && now.saturating_sub(last) < 6_000 {
                set_gossip_chip_active(&gossip_dot, &gossip_chip_text, &gossip_chip);
            } else {
                set_gossip_chip_listening(&gossip_dot, &gossip_chip_text, &gossip_chip);
            }
            glib::ControlFlow::Continue
        });
    }

    {
        let relay_list_order = Rc::clone(&relay_list_order);
        let relay_entry = relay_entry.clone();
        let remove_relay_button = remove_relay_button.clone();
        relay_list.connect_row_selected(move |_list, row| {
            let Some(row) = row else {
                remove_relay_button.set_sensitive(false);
                return;
            };
            let idx = row.index();
            if idx >= 0 {
                if let Some(endpoint) = relay_list_order.borrow().get(idx as usize) {
                    relay_entry.set_text(endpoint);
                    remove_relay_button.set_sensitive(true);
                }
            }
        });
    }

    {
        let app_settings = Rc::clone(&app_settings);
        let relay_list = relay_list.clone();
        let relay_list_order = Rc::clone(&relay_list_order);
        let relay_entry = relay_entry.clone();
        let remove_relay_button = remove_relay_button.clone();
        add_relay_button.connect_clicked(move |_| {
            let candidate = normalize_http_endpoint(&relay_entry.text());
            if candidate.trim().is_empty() {
                return;
            }
            let mut settings = app_settings.borrow_mut();
            if !settings
                .relay_endpoints
                .iter()
                .any(|item| item == &candidate)
            {
                settings.relay_endpoints.push(candidate.clone());
            }
            settings.relay_endpoints.sort();
            settings.relay_endpoints.dedup();
            if let Err(err) = save_app_settings(&settings) {
                append_local_log(&format!("save_settings_failed: {err}"));
            }
            render_relay_list(&settings.relay_endpoints, &relay_list, &relay_list_order);
            select_relay_row_by_value(&relay_list, &relay_list_order, &candidate);
            remove_relay_button.set_sensitive(true);
        });
    }

    {
        let app_settings = Rc::clone(&app_settings);
        let relay_list = relay_list.clone();
        let relay_list_order = Rc::clone(&relay_list_order);
        let relay_entry = relay_entry.clone();
        let remove_relay_button_for_click = remove_relay_button.clone();
        let remove_relay_button_state = remove_relay_button_for_click.clone();
        remove_relay_button_for_click.connect_clicked(move |_| {
            let idx = relay_list
                .selected_row()
                .map(|row| row.index())
                .unwrap_or(-1);
            if idx < 0 {
                return;
            }
            let mut settings = app_settings.borrow_mut();
            if (idx as usize) < settings.relay_endpoints.len() {
                settings.relay_endpoints.remove(idx as usize);
            }
            if let Err(err) = save_app_settings(&settings) {
                append_local_log(&format!("save_settings_failed: {err}"));
            }
            render_relay_list(&settings.relay_endpoints, &relay_list, &relay_list_order);
            relay_entry.set_text("");
            remove_relay_button_state.set_sensitive(false);
        });
    }

    {
        let add_relay_button = add_relay_button.clone();
        relay_entry.connect_activate(move |_| {
            add_relay_button.emit_clicked();
        });
    }

    {
        let tx = tx.clone();
        let wayfarer_id_entry = wayfarer_id_entry.clone();
        let identity_meta_label = identity_meta_label.clone();
        let onboarding_status = onboarding_status.clone();
        let share_wayfarer_entry = share_wayfarer_entry.clone();
        let share_qr_image = share_qr_image.clone();
        let share_status_label = share_status_label.clone();
        let app_settings = Rc::clone(&app_settings);
        let relay_sync_enabled = Arc::clone(&relay_sync_enabled);
        let relay_secondary_label = relay_secondary_label.clone();
        connect_button.connect_clicked(move |button| {
            button.set_sensitive(false);

            if !relay_sync_enabled.load(Ordering::SeqCst) {
                relay_secondary_label
                    .set_text("Secondary relay status: relay sync disabled in settings");
                button.set_sensitive(true);
                return;
            }

            let identity = match ensure_local_identity() {
                Ok(identity) => identity,
                Err(err) => {
                    eprintln!("{err}");
                    button.set_sensitive(true);
                    return;
                }
            };

            let key_preview: String = identity.verifying_key_b64.chars().take(16).collect();
            let device_preview: String = identity.device_id.chars().take(12).collect();
            wayfarer_id_entry.set_text(&identity.wayfarer_id);
            share_wayfarer_entry.set_text(&identity.wayfarer_id);
            refresh_share_qr(&identity.wayfarer_id, &share_qr_image, &share_status_label);
            identity_meta_label.set_text(&format!(
                "Identity metadata: device={} · device_id={}… · verify_key={}…",
                identity.device_name, device_preview, key_preview
            ));
            onboarding_status
                .set_text("Step 2/2 · Identity provisioned automatically before diagnostics.");

            let relay_endpoints = app_settings.borrow().relay_endpoints.clone();
            if relay_endpoints.is_empty() {
                relay_secondary_label.set_text("Secondary relay status: no relay configured");
                button.set_sensitive(true);
                return;
            }

            spawn_relay_checks(
                relay_endpoints,
                &identity.wayfarer_id,
                &identity.device_id,
                tx.clone(),
            );
        });
    }

    let (session_tx, session_rx) = channel::<SessionStatus>();

    {
        let session_tx = session_tx.clone();
        let app_settings = Rc::clone(&app_settings);
        let relay_sync_enabled = Arc::clone(&relay_sync_enabled);
        let recipient_entry = recipient_entry.clone();
        let body_entry = body_entry.clone();
        let chat_state = Rc::clone(&chat_state);
        send_button.connect_clicked(move |button| {
            button.set_sensitive(false);

            if !relay_sync_enabled.load(Ordering::SeqCst) {
                let _ = session_tx.send(SessionStatus {
                    op: SessionOp::Send,
                    text: "send failed: relay sync disabled in settings".to_string(),
                    ack_msg_id: None,
                    outgoing_contact: None,
                    outgoing_text: None,
                    outgoing_manifest_id: None,
                    outgoing_local_id: None,
                    outgoing_error: None,
                    pulled_messages: Vec::new(),
                });
                return;
            }

            let identity = match ensure_local_identity() {
                Ok(identity) => identity,
                Err(err) => {
                    let _ = session_tx.send(SessionStatus {
                        op: SessionOp::Send,
                        text: format!("send failed: {err}"),
                        ack_msg_id: None,
                        outgoing_contact: None,
                        outgoing_text: None,
                        outgoing_manifest_id: None,
                        outgoing_local_id: None,
                        outgoing_error: None,
                        pulled_messages: Vec::new(),
                    });
                    return;
                }
            };

            let relay_http = match app_settings.borrow().relay_endpoints.first().cloned() {
                Some(endpoint) => endpoint,
                None => {
                    let _ = session_tx.send(SessionStatus {
                        op: SessionOp::Send,
                        text: "send failed: no relay configured".to_string(),
                        ack_msg_id: None,
                        outgoing_contact: None,
                        outgoing_text: None,
                        outgoing_manifest_id: None,
                        outgoing_local_id: None,
                        outgoing_error: None,
                        pulled_messages: Vec::new(),
                    });
                    return;
                }
            };
            let relay_ws = to_ws_endpoint(&normalize_http_endpoint(&relay_http));
            let to = match chat_state.borrow().selected_contact.clone() {
                Some(contact) => contact,
                None => {
                    let _ = session_tx.send(SessionStatus {
                        op: SessionOp::Send,
                        text: "send failed: select a contact in Contacts first".to_string(),
                        ack_msg_id: None,
                        outgoing_contact: None,
                        outgoing_text: None,
                        outgoing_manifest_id: None,
                        outgoing_local_id: None,
                        outgoing_error: None,
                        pulled_messages: Vec::new(),
                    });
                    return;
                }
            };
            if !chat_state.borrow().contact_aliases.contains_key(&to) {
                let _ = session_tx.send(SessionStatus {
                    op: SessionOp::Send,
                    text: "send failed: selected contact is no longer in Contacts".to_string(),
                    ack_msg_id: None,
                    outgoing_contact: None,
                    outgoing_text: None,
                    outgoing_manifest_id: None,
                    outgoing_local_id: None,
                    outgoing_error: None,
                    pulled_messages: Vec::new(),
                });
                return;
            }
            recipient_entry.set_text(&to);
            let outgoing_text = body_entry.text().to_string();
            if outgoing_text.trim().is_empty() {
                let _ = session_tx.send(SessionStatus {
                    op: SessionOp::Send,
                    text: "send failed: message is empty".to_string(),
                    ack_msg_id: None,
                    outgoing_contact: None,
                    outgoing_text: None,
                    outgoing_manifest_id: None,
                    outgoing_local_id: None,
                    outgoing_error: None,
                    pulled_messages: Vec::new(),
                });
                return;
            }
            let payload_b64 = match build_envelope_payload_b64_from_utf8(&to, &outgoing_text) {
                Ok(payload) => payload,
                Err(err) => {
                    let _ = session_tx.send(SessionStatus {
                        op: SessionOp::Send,
                        text: format!("send failed: payload compose failed: {err}"),
                        ack_msg_id: None,
                        outgoing_contact: None,
                        outgoing_text: None,
                        outgoing_manifest_id: None,
                        outgoing_local_id: None,
                        outgoing_error: None,
                        pulled_messages: Vec::new(),
                    });
                    return;
                }
            };
            let expires_at_unix_ms = (now_unix_secs().max(0) as u64)
                .saturating_mul(1000)
                .saturating_add(3600 * 1000);
            let _ = gossip_record_local_payload(&payload_b64, expires_at_unix_ms);
            body_entry.set_text("");
            let outgoing_manifest_id = decode_envelope_payload_b64(&payload_b64)
                .ok()
                .map(|decoded| decoded.manifest_id_hex);
            let local_outgoing_id = next_local_outgoing_id();

            let _ = session_tx.send(SessionStatus {
                op: SessionOp::Send,
                text: "sending message...".to_string(),
                ack_msg_id: None,
                outgoing_contact: Some(to.clone()),
                outgoing_text: Some(outgoing_text.clone()),
                outgoing_manifest_id: outgoing_manifest_id.clone(),
                outgoing_local_id: Some(local_outgoing_id.clone()),
                outgoing_error: None,
                pulled_messages: Vec::new(),
            });

            let auth = std::env::var("AETHOS_RELAY_AUTH_TOKEN").ok();
            let session_tx = session_tx.clone();
            thread::spawn(move || {
                let to_for_status = to.clone();
                let text_for_status = outgoing_text.clone();
                let manifest_for_status = outgoing_manifest_id.clone();
                let result = send_to_relay_v1_with_auth(
                    &relay_ws,
                    (&identity.wayfarer_id, &identity.device_id),
                    &to,
                    &payload_b64,
                    None,
                    Some(3600),
                    auth.as_deref(),
                );

                let status = match result {
                    Ok((msg_id, received_at, expires_at)) => SessionStatus {
                        op: SessionOp::Send,
                        text: format!(
                            "send_ok msg_id={} received_at={:?} expires_at={:?}",
                            msg_id, received_at, expires_at
                        ),
                        ack_msg_id: Some(msg_id),
                        outgoing_contact: Some(to),
                        outgoing_text: Some(outgoing_text),
                        outgoing_manifest_id,
                        outgoing_local_id: Some(local_outgoing_id),
                        outgoing_error: None,
                        pulled_messages: Vec::new(),
                    },
                    Err(err) => SessionStatus {
                        op: SessionOp::Send,
                        text: format!("send failed: {err}"),
                        ack_msg_id: None,
                        outgoing_contact: Some(to_for_status),
                        outgoing_text: Some(text_for_status),
                        outgoing_manifest_id: manifest_for_status,
                        outgoing_local_id: Some(local_outgoing_id),
                        outgoing_error: Some(err.clone()),
                        pulled_messages: Vec::new(),
                    },
                };
                let _ = session_tx.send(status);
            });
        });
    }

    {
        let send_button = send_button.clone();
        body_entry.connect_activate(move |_| {
            send_button.emit_clicked();
        });
    }

    attach_status_poller(
        rx,
        RelayStatusUi {
            connect_button,
            relay_primary_label,
            relay_secondary_label,
            diagnostics_text,
            relay_dot,
            relay_chip_text,
            relay_chip,
        },
    );

    attach_session_poller(
        session_rx,
        SessionPollerUi {
            send_button,
            body_entry,
            chat_status_label,
            chat_state: Rc::clone(&chat_state),
            contacts_list,
            contact_order: Rc::clone(&contact_order),
            selected_contact_id_value: selected_contact_id_value.clone(),
            contact_alias_entry: contact_alias_entry.clone(),
            messages_list,
            messages_scroll: messages_scroll.clone(),
            thread_title,
            thread_contact_id_label,
            compact_contact_picker: compact_contact_picker.clone(),
            picker_syncing: Rc::clone(&picker_syncing),
            wave_pending: Rc::clone(&wave_pending),
            wave_mode_label,
            auto_scroll_locked: Rc::clone(&auto_scroll_locked),
        },
    );

    attach_compact_adaptive_mode(
        window.clone(),
        chat_shell,
        contacts_revealer,
        compact_picker_revealer,
    );

    start_background_inbox_sync(
        Rc::clone(&app_settings),
        Arc::clone(&relay_sync_enabled),
        session_tx.clone(),
    );
    start_background_gossip_sync(
        session_tx.clone(),
        Arc::clone(&gossip_sync_enabled),
        Arc::clone(&gossip_last_activity_ms),
        Arc::clone(&gossip_force_announce),
    );

    window.present();
    force_initial_thread_bottom(messages_scroll, Rc::clone(&auto_scroll_locked));
}

fn spawn_relay_checks(
    relay_http_endpoints: Vec<String>,
    wayfarer_id: &str,
    device_id: &str,
    tx: Sender<RelayStatus>,
) {
    let wayfarer_id = wayfarer_id.to_string();
    let device_id = device_id.to_string();

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
                json!({
                    "wayfarer_id": wayfarer_id,
                    "device_id": device_id,
                    "relay_slot": selection.relay_slot
                }),
            );

            let state = match selection.auth_token.as_deref() {
                Some(token) => connect_to_relay_with_auth(
                    &selection.relay_ws,
                    &wayfarer_id,
                    &device_id,
                    Some(token),
                ),
                None => connect_to_relay(&selection.relay_ws, &wayfarer_id, &device_id),
            };

            if state.starts_with("connected + hello_ok") {
                session_manager.mark_success(selection.relay_slot);
            } else {
                session_manager.mark_failure(selection.relay_slot);
            }

            let response = RelayFrame {
                correlation_id: outbound.correlation_id,
                message_type: if state.starts_with("connected + hello_ok") {
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
                batch_total: relay_count,
                relay_http: selection.relay_http,
                relay_ws: selection.relay_ws,
                state,
                dispatch,
            });
            completed += 1;
        }
    });
}

fn attach_status_poller(rx: Receiver<RelayStatus>, ui: RelayStatusUi) {
    let RelayStatusUi {
        connect_button,
        relay_primary_label,
        relay_secondary_label,
        diagnostics_text,
        relay_dot,
        relay_chip_text,
        relay_chip,
    } = ui;
    let mut completed = 0;
    let mut expected_total = 2;

    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        while let Ok(status) = rx.try_recv() {
            expected_total = status.batch_total.max(1);
            completed += 1;
            let text = format!(
                "{} -> {} · {} · {}",
                status.relay_http, status.relay_ws, status.state, status.dispatch
            );
            append_local_log(&format!("relay_status: {text}"));

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
        }

        if completed >= expected_total {
            let primary = relay_primary_label.text().to_string();
            let secondary = relay_secondary_label.text().to_string();
            update_relay_chip(
                &primary,
                &secondary,
                &relay_dot,
                &relay_chip_text,
                &relay_chip,
            );
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

fn update_relay_chip(
    primary_status: &str,
    secondary_status: &str,
    relay_dot: &Label,
    relay_chip_text: &Label,
    relay_chip: &GtkBox,
) {
    relay_dot.remove_css_class("relay-dot-idle");
    relay_dot.remove_css_class("relay-dot-ok");
    relay_dot.remove_css_class("relay-dot-warn");
    relay_dot.remove_css_class("relay-dot-down");
    relay_dot.remove_css_class("relay-dot-disabled");
    relay_chip.remove_css_class("relay-chip-disabled");

    let primary_ok = primary_status.contains("connected + hello_ok");
    let secondary_ok = secondary_status.contains("connected + hello_ok");

    match (primary_ok, secondary_ok) {
        (true, true) => {
            relay_dot.add_css_class("relay-dot-ok");
            relay_chip_text.set_text("Relays: healthy (2/2)");
        }
        (true, false) | (false, true) => {
            relay_dot.add_css_class("relay-dot-warn");
            relay_chip_text.set_text("Relays: degraded (1/2)");
        }
        (false, false) => {
            let has_any_result = primary_status != "idle" || secondary_status != "idle";
            if has_any_result {
                relay_dot.add_css_class("relay-dot-down");
                relay_chip_text.set_text("Relays: unavailable (0/2)");
            } else {
                relay_dot.add_css_class("relay-dot-idle");
                relay_chip_text.set_text("Relays: idle");
            }
        }
    }

    relay_chip.set_tooltip_text(Some(&format!(
        "Primary: {}\nSecondary: {}",
        primary_status, secondary_status
    )));
}

fn set_relay_chip_disabled(relay_dot: &Label, relay_chip_text: &Label, relay_chip: &GtkBox) {
    relay_dot.remove_css_class("relay-dot-idle");
    relay_dot.remove_css_class("relay-dot-ok");
    relay_dot.remove_css_class("relay-dot-warn");
    relay_dot.remove_css_class("relay-dot-down");
    relay_dot.remove_css_class("relay-dot-disabled");
    relay_dot.add_css_class("relay-dot-disabled");
    relay_chip.add_css_class("relay-chip-disabled");
    relay_chip_text.set_text("Relays: disabled");
    relay_chip.set_tooltip_text(Some("Relay sync is disabled in Settings"));
}

fn reset_gossip_chip_classes(gossip_dot: &Label, gossip_chip: &GtkBox) {
    gossip_dot.remove_css_class("gossip-dot-idle");
    gossip_dot.remove_css_class("gossip-dot-active");
    gossip_dot.remove_css_class("gossip-dot-disabled");
    gossip_chip.remove_css_class("gossip-chip-active");
    gossip_chip.remove_css_class("gossip-chip-disabled");
}

fn set_gossip_chip_listening(gossip_dot: &Label, gossip_chip_text: &Label, gossip_chip: &GtkBox) {
    reset_gossip_chip_classes(gossip_dot, gossip_chip);
    gossip_dot.add_css_class("gossip-dot-idle");
    gossip_chip_text.set_text("LAN Gossip: listening");
    gossip_chip.set_tooltip_text(Some("LAN gossip is enabled and listening"));
}

fn set_gossip_chip_active(gossip_dot: &Label, gossip_chip_text: &Label, gossip_chip: &GtkBox) {
    reset_gossip_chip_classes(gossip_dot, gossip_chip);
    gossip_dot.add_css_class("gossip-dot-active");
    gossip_chip.add_css_class("gossip-chip-active");
    gossip_chip_text.set_text("LAN Gossip: active");
    gossip_chip.set_tooltip_text(Some("Recent LAN gossip activity detected"));
}

fn set_gossip_chip_disabled(gossip_dot: &Label, gossip_chip_text: &Label, gossip_chip: &GtkBox) {
    reset_gossip_chip_classes(gossip_dot, gossip_chip);
    gossip_dot.add_css_class("gossip-dot-disabled");
    gossip_chip.add_css_class("gossip-chip-disabled");
    gossip_chip_text.set_text("LAN Gossip: disabled");
    gossip_chip.set_tooltip_text(Some("LAN gossip is disabled in Settings"));
}

fn attach_session_poller(rx: Receiver<SessionStatus>, ui: SessionPollerUi) {
    let SessionPollerUi {
        send_button,
        body_entry,
        chat_status_label,
        chat_state,
        contacts_list,
        contact_order,
        selected_contact_id_value,
        contact_alias_entry,
        messages_list,
        messages_scroll,
        thread_title,
        thread_contact_id_label,
        compact_contact_picker,
        picker_syncing,
        wave_pending,
        wave_mode_label,
        auto_scroll_locked,
    } = ui;
    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        while let Ok(status) = rx.try_recv() {
            match status.op {
                SessionOp::Send => send_button.set_sensitive(true),
                SessionOp::Inbox => {}
            }

            chat_status_label.set_text(&status.text);
            append_local_log(&format!("session_status: {}", status.text));

            {
                let mut state = chat_state.borrow_mut();
                let mut aliases_changed = false;

                if let (Some(contact), Some(text)) = (
                    status.outgoing_contact.as_ref(),
                    status.outgoing_text.as_ref(),
                ) {
                    let local_id = status
                        .outgoing_local_id
                        .clone()
                        .unwrap_or_else(next_local_outgoing_id);
                    let now = now_unix_secs();
                    let thread = state.threads.entry(contact.clone()).or_default();

                    if !thread.iter().any(|item| item.msg_id == local_id) {
                        thread.push(ChatMessage {
                            msg_id: local_id.clone(),
                            text: text.clone(),
                            timestamp: format_timestamp_from_unix(now),
                            created_at_unix: now,
                            direction: ChatDirection::Outgoing,
                            seen: true,
                            manifest_id_hex: status.outgoing_manifest_id.clone(),
                            delivered_at: None,
                            outbound_state: Some(OutboundState::Sending),
                        });
                    }

                    if let Some(item) = thread.iter_mut().find(|item| item.msg_id == local_id) {
                        if let Some(error) = status.outgoing_error.as_ref() {
                            item.outbound_state = Some(OutboundState::Failed {
                                error: error.clone(),
                            });
                        } else if let Some(server_msg_id) = status.ack_msg_id.as_ref() {
                            item.msg_id = server_msg_id.clone();
                            item.outbound_state = Some(OutboundState::Sent);
                        }
                    }

                    state.selected_contact = Some(contact.clone());
                    mark_contact_seen(&mut state, contact);
                }

                for pulled in &status.pulled_messages {
                    if !state.contact_aliases.contains_key(&pulled.from_wayfarer_id) {
                        state.contact_aliases.insert(
                            pulled.from_wayfarer_id.clone(),
                            pulled.from_wayfarer_id.clone(),
                        );
                        state.new_contacts.insert(pulled.from_wayfarer_id.clone());
                        aliases_changed = true;
                    }

                    let is_seen_on_insert =
                        state.selected_contact.as_deref() == Some(pulled.from_wayfarer_id.as_str());

                    let thread = state
                        .threads
                        .entry(pulled.from_wayfarer_id.clone())
                        .or_default();

                    let already_present = thread.iter().any(|existing| {
                        existing.msg_id == pulled.msg_id
                            || (pulled.manifest_id_hex.is_some()
                                && existing.manifest_id_hex == pulled.manifest_id_hex)
                    });

                    if already_present {
                        continue;
                    }

                    thread.push(ChatMessage {
                        msg_id: pulled.msg_id.clone(),
                        text: pulled.text.clone(),
                        timestamp: format_timestamp_from_unix(pulled.received_at),
                        created_at_unix: pulled.received_at,
                        direction: ChatDirection::Incoming,
                        seen: is_seen_on_insert,
                        manifest_id_hex: pulled.manifest_id_hex.clone(),
                        delivered_at: None,
                        outbound_state: None,
                    });

                    if let Some(manifest) = pulled.receipt_manifest_id.as_ref() {
                        let delivered_time = pulled
                            .receipt_received_at_unix_ms
                            .map(format_timestamp_from_unix_ms)
                            .unwrap_or_else(|| format_timestamp_from_unix(pulled.received_at));
                        if let Some(thread) = state.threads.get_mut(&pulled.from_wayfarer_id) {
                            for item in thread.iter_mut().rev() {
                                if matches!(item.direction, ChatDirection::Outgoing)
                                    && item.manifest_id_hex.as_deref() == Some(manifest.as_str())
                                {
                                    item.delivered_at = Some(delivered_time.clone());
                                    item.outbound_state = Some(OutboundState::Sent);
                                    break;
                                }
                            }
                        }
                    }

                    if state.selected_contact.is_none() {
                        state.selected_contact = Some(pulled.from_wayfarer_id.clone());
                        mark_contact_seen(&mut state, &pulled.from_wayfarer_id);
                    }
                }

                if aliases_changed {
                    let _ = save_contact_aliases(&state.contact_aliases);
                }
            }

            if let Err(err) = save_persisted_chat_state(&chat_state.borrow()) {
                append_local_log(&format!("persist_chat_state_failed: {err}"));
            }
            let pending = has_pending_outbound(&chat_state.borrow());
            wave_pending.set(pending);
            if pending {
                wave_mode_label.set_text("Outbound flow active");
            } else {
                wave_mode_label.set_text("Ambient mesh active");
            }

            render_contacts(&chat_state.borrow(), &contacts_list, &contact_order);
            picker_syncing.set(true);
            sync_contact_picker(
                &chat_state.borrow(),
                &compact_contact_picker,
                &contact_order,
            );
            picker_syncing.set(false);
            sync_contact_form(
                &chat_state.borrow(),
                &selected_contact_id_value,
                &contact_alias_entry,
            );
            render_messages(
                &chat_state.borrow(),
                MessageRenderUi {
                    messages_list: &messages_list,
                    messages_scroll: &messages_scroll,
                    thread_title: &thread_title,
                    thread_contact_id_label: &thread_contact_id_label,
                    send_button: &send_button,
                    body_entry: &body_entry,
                    auto_scroll_locked: &auto_scroll_locked,
                },
            );

            if status.outgoing_contact.is_some() {
                pulse_widget(&send_button, "pulse-send");
            }
            if !status.pulled_messages.is_empty() {
                pulse_widget(&messages_list, "pulse-receive");
            }
        }

        glib::ControlFlow::Continue
    });
}

fn start_background_inbox_sync(
    app_settings: Rc<RefCell<AppSettings>>,
    relay_sync_enabled: Arc<AtomicBool>,
    session_tx: Sender<SessionStatus>,
) {
    let inflight = Arc::new(AtomicBool::new(false));
    glib::timeout_add_local(Duration::from_millis(2200), move || {
        if !relay_sync_enabled.load(Ordering::SeqCst) {
            return glib::ControlFlow::Continue;
        }

        if inflight.swap(true, Ordering::SeqCst) {
            return glib::ControlFlow::Continue;
        }

        let relay_http = match app_settings.borrow().relay_endpoints.first().cloned() {
            Some(endpoint) => normalize_http_endpoint(&endpoint),
            None => {
                inflight.store(false, Ordering::SeqCst);
                return glib::ControlFlow::Continue;
            }
        };
        let session_tx = session_tx.clone();
        let inflight_done = Arc::clone(&inflight);
        thread::spawn(move || {
            let result = sync_inbox_once(&relay_http);
            match result {
                Ok(previews) if !previews.is_empty() => {
                    let _ = session_tx.send(SessionStatus {
                        op: SessionOp::Inbox,
                        text: format!("received {} message(s)", previews.len()),
                        ack_msg_id: previews.first().map(|item| item.msg_id.clone()),
                        outgoing_contact: None,
                        outgoing_text: None,
                        outgoing_manifest_id: None,
                        outgoing_local_id: None,
                        outgoing_error: None,
                        pulled_messages: previews,
                    });
                }
                Ok(_) => {}
                Err(err) => {
                    append_local_log(&format!("inbox_sync_failed: {err}"));
                }
            }
            inflight_done.store(false, Ordering::SeqCst);
        });

        glib::ControlFlow::Continue
    });
}

fn sync_inbox_once(relay_http: &str) -> Result<Vec<PulledMessagePreview>, String> {
    let identity = ensure_local_identity()?;
    let relay_ws = to_ws_endpoint(relay_http);
    let auth = std::env::var("AETHOS_RELAY_AUTH_TOKEN").ok();
    let messages = pull_from_relay_v1_with_auth(
        &relay_ws,
        &identity.wayfarer_id,
        &identity.device_id,
        Some(50),
        auth.as_deref(),
    )?;

    let mut previews = Vec::with_capacity(messages.len());
    let expires_at_unix_ms = (now_unix_secs().max(0) as u64)
        .saturating_mul(1000)
        .saturating_add(24 * 60 * 60 * 1000);
    for message in &messages {
        let manifest_id_hex = decode_envelope_payload_b64(&message.payload_b64)
            .ok()
            .map(|decoded| decoded.manifest_id_hex);
        let _ = gossip_record_local_payload(&message.payload_b64, expires_at_unix_ms);
        let text = decode_message_text_for_display(&message.payload_b64);

        previews.push(PulledMessagePreview {
            from_wayfarer_id: message.from_wayfarer_id.clone(),
            msg_id: message.msg_id.clone(),
            text,
            received_at: message.received_at,
            manifest_id_hex,
            receipt_manifest_id: extract_receipt_manifest_id(&message.payload_b64),
            receipt_received_at_unix_ms: extract_receipt_received_at_ms(&message.payload_b64),
        });
    }

    for message in messages {
        let _ = ack_relay_message_v1_with_auth(
            &relay_ws,
            &identity.wayfarer_id,
            &identity.device_id,
            &message.msg_id,
            auth.as_deref(),
        );
    }

    Ok(previews)
}

fn start_background_gossip_sync(
    session_tx: Sender<SessionStatus>,
    gossip_sync_enabled: Arc<AtomicBool>,
    gossip_last_activity_ms: Arc<AtomicU64>,
    gossip_force_announce: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let socket = match UdpSocket::bind(("0.0.0.0", GOSSIP_LAN_PORT)) {
            Ok(socket) => socket,
            Err(err) => {
                append_local_log(&format!(
                    "gossip_sync_disabled: failed binding udp/{GOSSIP_LAN_PORT}: {err}"
                ));
                return;
            }
        };

        if let Err(err) = socket.set_nonblocking(true) {
            append_local_log(&format!(
                "gossip_sync_disabled: set_nonblocking failed: {err}"
            ));
            return;
        }
        if let Err(err) = socket.set_broadcast(true) {
            append_local_log(&format!(
                "gossip_sync_disabled: set_broadcast failed: {err}"
            ));
            return;
        }

        append_local_log(&format!("gossip_sync_started on udp/{GOSSIP_LAN_PORT}"));

        let mut seq: u64 = 0;
        let mut last_inventory_broadcast = Instant::now() - Duration::from_secs(10);

        loop {
            if !gossip_sync_enabled.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(120));
                continue;
            }

            let force_announce = gossip_force_announce.swap(false, Ordering::SeqCst);
            if force_announce || last_inventory_broadcast.elapsed() >= Duration::from_secs(3) {
                if let Ok(identity) = ensure_local_identity() {
                    seq = seq.saturating_add(1);
                    let session_id = gossip_id("sess", seq);
                    let inventory = gossip_inventory_entries(now_unix_ms()).unwrap_or_default();
                    let frame = GossipSyncFrame::InventorySummary(
                        crate::aethos_core::gossip_sync::InventorySummaryFrame {
                            sync_version: GOSSIP_SYNC_VERSION,
                            session_id,
                            sender_wayfarer_id: identity.wayfarer_id,
                            page: 1,
                            has_more: false,
                            inventory,
                        },
                    );
                    let _ = send_gossip_frame(&socket, "255.255.255.255", GOSSIP_LAN_PORT, &frame);
                    gossip_last_activity_ms.store(now_unix_ms(), Ordering::SeqCst);
                }
                last_inventory_broadcast = Instant::now();
            }

            let mut buf = [0u8; 65_535];
            match socket.recv_from(&mut buf) {
                Ok((len, source)) => {
                    gossip_last_activity_ms.store(now_unix_ms(), Ordering::SeqCst);
                    let frame = match parse_gossip_frame(&buf[..len]) {
                        Ok(frame) => frame,
                        Err(err) => {
                            append_local_log(&format!("gossip_parse_ignored from {source}: {err}"));
                            continue;
                        }
                    };

                    let identity = match ensure_local_identity() {
                        Ok(identity) => identity,
                        Err(err) => {
                            append_local_log(&format!("gossip_identity_unavailable: {err}"));
                            continue;
                        }
                    };

                    let local_wayfarer = identity.wayfarer_id;

                    match frame {
                        GossipSyncFrame::InventorySummary(inv)
                            if inv.sender_wayfarer_id != local_wayfarer =>
                        {
                            let mut missing_item_ids = inv
                                .inventory
                                .iter()
                                .filter(|entry| {
                                    entry.to_wayfarer_id == local_wayfarer
                                        && entry.expires_at_unix_ms > now_unix_ms()
                                })
                                .filter_map(|entry| {
                                    gossip_has_item(&entry.item_id).ok().and_then(|exists| {
                                        if exists {
                                            None
                                        } else {
                                            Some(entry.item_id.clone())
                                        }
                                    })
                                })
                                .collect::<Vec<_>>();
                            missing_item_ids.sort();

                            seq = seq.saturating_add(1);
                            let request = GossipSyncFrame::MissingRequest(
                                crate::aethos_core::gossip_sync::MissingRequestFrame {
                                    sync_version: GOSSIP_SYNC_VERSION,
                                    session_id: inv.session_id,
                                    sender_wayfarer_id: local_wayfarer,
                                    page: 1,
                                    has_more: false,
                                    request_id: gossip_id("req", seq),
                                    in_response_to_page: 1,
                                    missing_item_ids,
                                    max_transfer_items: 64,
                                    max_transfer_bytes: 2_097_152,
                                },
                            );
                            let _ = send_gossip_frame(
                                &socket,
                                &source.ip().to_string(),
                                source.port(),
                                &request,
                            );
                            gossip_last_activity_ms.store(now_unix_ms(), Ordering::SeqCst);
                        }
                        GossipSyncFrame::MissingRequest(req)
                            if req.sender_wayfarer_id != local_wayfarer =>
                        {
                            let items = gossip_transfer_items(
                                &req.missing_item_ids,
                                req.max_transfer_items.max(1),
                                req.max_transfer_bytes.max(1),
                                now_unix_ms(),
                            )
                            .unwrap_or_default();

                            seq = seq.saturating_add(1);
                            let transfer = GossipSyncFrame::Transfer(
                                crate::aethos_core::gossip_sync::TransferFrame {
                                    sync_version: GOSSIP_SYNC_VERSION,
                                    session_id: req.session_id,
                                    sender_wayfarer_id: local_wayfarer,
                                    page: 1,
                                    has_more: false,
                                    transfer_id: gossip_id("tx", seq),
                                    in_response_to_request_id: req.request_id,
                                    items,
                                },
                            );
                            let _ = send_gossip_frame(
                                &socket,
                                &source.ip().to_string(),
                                source.port(),
                                &transfer,
                            );
                            gossip_last_activity_ms.store(now_unix_ms(), Ordering::SeqCst);
                        }
                        GossipSyncFrame::Transfer(transfer)
                            if transfer.sender_wayfarer_id != local_wayfarer =>
                        {
                            let result = import_transfer_items(
                                &transfer.sender_wayfarer_id,
                                &local_wayfarer,
                                &transfer.items,
                                now_unix_ms(),
                            );

                            if let Ok(result) = result {
                                if !result.new_messages.is_empty() {
                                    let pulled_messages = result
                                        .new_messages
                                        .iter()
                                        .map(|item| PulledMessagePreview {
                                            from_wayfarer_id: item.from_wayfarer_id.clone(),
                                            msg_id: item.item_id.clone(),
                                            text: extract_chat_text_if_json(&item.text),
                                            received_at: item.received_at_unix,
                                            manifest_id_hex: item.manifest_id_hex.clone(),
                                            receipt_manifest_id: None,
                                            receipt_received_at_unix_ms: None,
                                        })
                                        .collect::<Vec<_>>();
                                    let _ = session_tx.send(SessionStatus {
                                        op: SessionOp::Inbox,
                                        text: format!(
                                            "LAN gossip received {} message(s)",
                                            pulled_messages.len()
                                        ),
                                        ack_msg_id: pulled_messages
                                            .first()
                                            .map(|item| item.msg_id.clone()),
                                        outgoing_contact: None,
                                        outgoing_text: None,
                                        outgoing_manifest_id: None,
                                        outgoing_local_id: None,
                                        outgoing_error: None,
                                        pulled_messages,
                                    });
                                }

                                seq = seq.saturating_add(1);
                                let status = if result.rejected_items.is_empty() {
                                    "accepted".to_string()
                                } else if result.accepted_item_ids.is_empty() {
                                    "rejected".to_string()
                                } else {
                                    "partial".to_string()
                                };
                                let receipt = GossipSyncFrame::Receipt(
                                    crate::aethos_core::gossip_sync::ReceiptFrame {
                                        sync_version: GOSSIP_SYNC_VERSION,
                                        session_id: transfer.session_id,
                                        sender_wayfarer_id: local_wayfarer,
                                        page: 1,
                                        has_more: false,
                                        receipt_id: gossip_id("rcpt", seq),
                                        in_response_to_transfer_id: transfer.transfer_id,
                                        status,
                                        accepted_item_ids: result.accepted_item_ids,
                                        rejected_items: result.rejected_items,
                                    },
                                );
                                let _ = send_gossip_frame(
                                    &socket,
                                    &source.ip().to_string(),
                                    source.port(),
                                    &receipt,
                                );
                                gossip_last_activity_ms.store(now_unix_ms(), Ordering::SeqCst);
                            }
                        }
                        GossipSyncFrame::Receipt(receipt)
                            if receipt.sender_wayfarer_id != local_wayfarer =>
                        {
                            append_local_log(&format!(
                                "gossip_receipt {} status={} accepted={} rejected={}",
                                receipt.receipt_id,
                                receipt.status,
                                receipt.accepted_item_ids.len(),
                                receipt.rejected_items.len()
                            ));
                        }
                        _ => {}
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(30));
                }
                Err(err) => {
                    append_local_log(&format!("gossip_recv_error: {err}"));
                    thread::sleep(Duration::from_millis(80));
                }
            }
        }
    });
}

fn send_gossip_frame(
    socket: &UdpSocket,
    host: &str,
    port: u16,
    frame: &GossipSyncFrame,
) -> Result<(), String> {
    let raw = serialize_gossip_frame(frame)?;
    let addr = format!("{host}:{port}");
    socket
        .send_to(&raw, &addr)
        .map(|_| ())
        .map_err(|err| format!("gossip send failed ({addr}): {err}"))
}

fn gossip_id(prefix: &str, seq: u64) -> String {
    format!("linux-{prefix}-{}-{seq}", now_unix_ms())
}

fn decode_message_text_for_display(payload_b64: &str) -> String {
    match decode_envelope_payload_utf8_preview(payload_b64) {
        Ok(text) => extract_chat_text_if_json(&text),
        Err(_) => match decode_envelope_payload_b64(payload_b64) {
            Ok(decoded) => {
                if let Ok(body_text) = String::from_utf8(decoded.body) {
                    return extract_chat_text_if_json(&body_text);
                }
                let manifest_preview: String = decoded.manifest_id_hex.chars().take(12).collect();
                format!("[binary body manifest={}…]", manifest_preview)
            }
            Err(err) => format!("[decode_error={err}]"),
        },
    }
}

fn extract_chat_text_if_json(input: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
        return input.to_string();
    };

    if let Some((manifest, ts)) = parse_receipt_like_json(&value) {
        let at = ts
            .map(format_timestamp_from_unix_ms)
            .unwrap_or_else(|| "unknown".to_string());
        let manifest_short: String = manifest.chars().take(10).collect();
        return format!("Delivered ✓ ({manifest_short}… at {at})");
    }

    if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
        return text.to_string();
    }

    input.to_string()
}

fn extract_receipt_manifest_id(payload_b64: &str) -> Option<String> {
    let text = decode_envelope_payload_utf8_preview(payload_b64).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    parse_receipt_like_json(&value).map(|tuple| tuple.0)
}

fn extract_receipt_received_at_ms(payload_b64: &str) -> Option<u64> {
    let text = decode_envelope_payload_utf8_preview(payload_b64).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    parse_receipt_like_json(&value).and_then(|tuple| tuple.1)
}

fn parse_receipt_like_json(value: &serde_json::Value) -> Option<(String, Option<u64>)> {
    if value
        .get("type")
        .and_then(|v| v.as_str())
        .map(|v| v == "receipt")
        .unwrap_or(false)
    {
        let manifest = value
            .get("manifestId")
            .or_else(|| value.get("manifest_id"))
            .and_then(|v| v.as_str())?
            .to_string();
        let ts = value
            .get("receivedAtUnixMs")
            .or_else(|| value.get("received_at_unix_ms"))
            .and_then(|v| v.as_u64());
        return Some((manifest, ts));
    }

    if value
        .get("receipt_scope")
        .and_then(|v| v.as_str())
        .is_some()
    {
        let receipt_b64 = value.get("receipt_v1_b64").and_then(|v| v.as_str())?;
        return decode_receipt_v1_b64(receipt_b64).ok();
    }

    None
}

fn decode_receipt_v1_b64(receipt_b64: &str) -> Result<(String, Option<u64>), String> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(receipt_b64)
        .map_err(|err| format!("failed decoding receipt_v1_b64: {err}"))?;
    let mut cursor = if raw.len() >= 2 && raw[0] == 1 && raw[1] == 4 {
        2usize
    } else {
        0usize
    };

    let mut manifest: Option<String> = None;
    let mut received_ms: Option<u64> = None;

    while cursor + 5 <= raw.len() {
        let field_id = raw[cursor];
        cursor += 1;
        let len = u32::from_be_bytes(
            raw[cursor..cursor + 4]
                .try_into()
                .map_err(|_| "invalid receipt field length".to_string())?,
        ) as usize;
        cursor += 4;
        if cursor + len > raw.len() {
            return Err("truncated receipt field payload".to_string());
        }
        let value = &raw[cursor..cursor + len];
        cursor += len;

        match field_id {
            2 => manifest = Some(bytes_to_hex_lower(value)),
            3 if len == 8 => {
                received_ms =
                    Some(u64::from_be_bytes(value.try_into().map_err(|_| {
                        "invalid receipt timestamp bytes".to_string()
                    })?))
            }
            _ => {}
        }
    }

    let manifest = manifest.ok_or_else(|| "receipt missing manifestId".to_string())?;
    Ok((manifest, received_ms))
}

fn render_contacts(
    state: &ChatState,
    contacts_list: &ListBox,
    contact_order: &Rc<RefCell<Vec<String>>>,
) {
    clear_listbox(contacts_list);

    let contacts = state.contact_aliases.keys().cloned().collect::<Vec<_>>();
    *contact_order.borrow_mut() = contacts.clone();

    for contact in contacts {
        let row = ListBoxRow::new();
        row.add_css_class("contact-row");

        let row_box = GtkBox::new(Orientation::Horizontal, 10);
        let avatar = Label::new(Some(&avatar_glyph(&contact)));
        avatar.add_css_class("contact-avatar");
        avatar.set_size_request(36, 36);

        let label_column = GtkBox::new(Orientation::Vertical, 1);
        let title = Label::new(Some(&contact_display_name(state, &contact)));
        title.set_xalign(0.0);
        title.set_ellipsize(EllipsizeMode::End);
        title.set_width_chars(20);
        title.set_max_width_chars(24);
        title.set_single_line_mode(true);
        title.add_css_class("contact-title");

        let subtitle_text = tiny_wayfarer(&contact);
        let subtitle = Label::new(Some(&subtitle_text));
        subtitle.set_xalign(0.0);
        subtitle.set_ellipsize(EllipsizeMode::End);
        subtitle.set_width_chars(20);
        subtitle.set_max_width_chars(24);
        subtitle.set_single_line_mode(true);
        subtitle.add_css_class("contact-subtitle");

        label_column.append(&title);
        label_column.append(&subtitle);

        let unread_count = state
            .threads
            .get(&contact)
            .map(|messages| {
                messages
                    .iter()
                    .filter(|msg| matches!(msg.direction, ChatDirection::Incoming) && !msg.seen)
                    .count()
            })
            .unwrap_or(0);

        let is_new = state.new_contacts.contains(&contact);
        if is_new && state.selected_contact.as_deref() != Some(contact.as_str()) {
            let badge = Label::new(Some("NEW"));
            badge.add_css_class("new-badge");
            row_box.append(&avatar);
            row_box.append(&label_column);
            row_box.append(&badge);
        } else if unread_count > 0 && state.selected_contact.as_deref() != Some(contact.as_str()) {
            let badge = Label::new(Some(&unread_count.to_string()));
            badge.add_css_class("unread-badge");
            row_box.append(&avatar);
            row_box.append(&label_column);
            row_box.append(&badge);
        } else {
            row_box.append(&avatar);
            row_box.append(&label_column);
        }

        row.set_child(Some(&row_box));
        contacts_list.append(&row);
    }
}

fn render_contacts_manager(
    state: &ChatState,
    contacts_list: &ListBox,
    contact_order: &Rc<RefCell<Vec<String>>>,
) {
    clear_listbox(contacts_list);

    let contacts = state
        .contact_aliases
        .keys()
        .filter(|contact| contact_matches_filter(state, contact, &state.contact_filter))
        .cloned()
        .collect::<Vec<_>>();
    *contact_order.borrow_mut() = contacts.clone();

    for contact in contacts {
        let row = ListBoxRow::new();
        row.add_css_class("contact-row");

        let marker = if state.new_contacts.contains(&contact) {
            " [NEW]"
        } else {
            ""
        };

        let label = Label::new(Some(&format!(
            "{}{} · {}",
            contact_display_name(state, &contact),
            marker,
            tiny_wayfarer(&contact)
        )));
        label.set_xalign(0.0);
        label.set_ellipsize(EllipsizeMode::End);
        label.set_width_chars(28);
        label.set_max_width_chars(38);
        label.set_single_line_mode(true);
        row.set_child(Some(&label));
        contacts_list.append(&row);
    }
}

fn contact_matches_filter(state: &ChatState, wayfarer_id: &str, filter: &str) -> bool {
    let query = filter.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }

    if wayfarer_id.contains(query.as_str()) {
        return true;
    }

    state
        .contact_aliases
        .get(wayfarer_id)
        .map(|alias| alias.to_ascii_lowercase().contains(query.as_str()))
        .unwrap_or(false)
}

fn sync_contact_picker(
    state: &ChatState,
    picker: &ComboBoxText,
    contact_order: &Rc<RefCell<Vec<String>>>,
) {
    let contacts = contact_order.borrow();
    picker.remove_all();
    for contact in contacts.iter() {
        picker.append(Some(contact), &contact_display_name(state, contact));
    }

    if let Some(selected) = state.selected_contact.as_ref() {
        picker.set_active_id(Some(selected));
    } else if !contacts.is_empty() {
        picker.set_active(Some(0));
    }
}

fn sync_contact_form(
    state: &ChatState,
    selected_contact_id_value: &Label,
    contact_alias_entry: &Entry,
) {
    let Some(selected_contact) = state.selected_contact.as_ref() else {
        selected_contact_id_value.set_text("No contact selected");
        contact_alias_entry.set_text("");
        return;
    };

    selected_contact_id_value.set_text(selected_contact);
    if let Some(alias) = state.contact_aliases.get(selected_contact) {
        contact_alias_entry.set_text(alias);
    } else {
        contact_alias_entry.set_text("");
    }
}

fn update_contact_action_buttons(
    state: &ChatState,
    new_contact_id_entry: &Entry,
    add_update_contact_button: &Button,
    remove_contact_button: &Button,
) {
    let has_new_contact_id = !new_contact_id_entry.text().trim().is_empty();
    if has_new_contact_id {
        add_update_contact_button.set_label("Add Contact");
    } else {
        add_update_contact_button.set_label("Update Contact");
    }
    remove_contact_button.set_sensitive(state.selected_contact.is_some());
}

fn attach_compact_adaptive_mode(
    window: ApplicationWindow,
    chat_shell: Paned,
    contacts_revealer: Revealer,
    compact_picker_revealer: Revealer,
) {
    let last_compact = Cell::new(None::<bool>);
    glib::timeout_add_local(Duration::from_millis(180), move || {
        let width = window.width();
        let compact_mode = width > 0 && width < 900;

        if compact_mode {
            let suggested = (width / 3).clamp(150, 260);
            chat_shell.set_position(suggested);
        }

        if last_compact.get() != Some(compact_mode) {
            contacts_revealer.set_reveal_child(true);
            compact_picker_revealer.set_reveal_child(compact_mode);
            if !compact_mode {
                chat_shell.set_position(300);
            }
            last_compact.set(Some(compact_mode));
        }

        glib::ControlFlow::Continue
    });
}

fn render_messages(state: &ChatState, ui: MessageRenderUi<'_>) {
    let MessageRenderUi {
        messages_list,
        messages_scroll,
        thread_title,
        thread_contact_id_label,
        send_button,
        body_entry,
        auto_scroll_locked,
    } = ui;
    clear_listbox(messages_list);

    let Some(selected_contact) = state.selected_contact.as_ref() else {
        thread_title.set_text("Thread");
        thread_contact_id_label.set_text("");
        return;
    };

    thread_title.set_text(&format!(
        "Thread · {}",
        contact_display_name(state, selected_contact)
    ));
    if state.show_full_contact_id {
        thread_contact_id_label.set_text(selected_contact);
    } else {
        thread_contact_id_label.set_text(&tiny_wayfarer(selected_contact));
    }

    if let Some(messages) = state.threads.get(selected_contact) {
        let mut last_day_key: Option<String> = None;
        let mut last_direction: Option<ChatDirection> = None;

        for message in messages {
            let (day_key, day_label) = day_key_and_label(message.created_at_unix);
            let is_new_day = last_day_key.as_deref() != Some(day_key.as_str());
            if is_new_day {
                let day_row = ListBoxRow::new();
                day_row.set_selectable(false);
                let day_label_widget = Label::new(Some(&day_label));
                day_label_widget.add_css_class("day-separator");
                day_label_widget.set_xalign(0.5);
                day_row.set_child(Some(&day_label_widget));
                messages_list.append(&day_row);
            }

            let grouped = !is_new_day
                && last_direction.as_ref() == Some(&message.direction)
                && matches!(
                    message.direction,
                    ChatDirection::Incoming | ChatDirection::Outgoing
                );

            let row = ListBoxRow::new();
            row.set_selectable(false);

            let bubble_wrap = GtkBox::new(Orientation::Horizontal, 8);
            let bubble = Label::new(Some(&message.text));
            bubble.set_wrap(true);
            bubble.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
            bubble.set_max_width_chars(54);
            bubble.set_xalign(0.0);
            bubble.add_css_class("chat-bubble");
            bubble.set_tooltip_text(Some("Click to copy message"));

            let copy_text = message.text.clone();
            let bubble_for_click = bubble.clone();
            let click = gtk4::GestureClick::new();
            click.set_button(0);
            click.connect_released(move |_, _, _, _| {
                if let Some(display) = Display::default() {
                    display.clipboard().set_text(&copy_text);
                }
                show_copied_popover(&bubble_for_click);
            });
            bubble.add_controller(click);

            let metadata = Label::new(Some(&format!("id={}", message.msg_id)));
            let delivery_suffix = match (&message.direction, &message.outbound_state) {
                (ChatDirection::Outgoing, Some(OutboundState::Sending)) => " · sending".to_string(),
                (ChatDirection::Outgoing, Some(OutboundState::Failed { .. })) => {
                    " · failed (retry)".to_string()
                }
                (ChatDirection::Outgoing, _) => message
                    .delivered_at
                    .as_ref()
                    .map(|at| format!(" · delivered {at}"))
                    .unwrap_or_else(|| " · sent".to_string()),
                (ChatDirection::Incoming, _) => String::new(),
            };
            metadata.set_text(&format!(
                "{} · {}{}",
                message.timestamp,
                short_msg_id(&message.msg_id),
                delivery_suffix
            ));
            metadata.add_css_class("bubble-meta");
            match (
                &message.direction,
                &message.outbound_state,
                &message.delivered_at,
            ) {
                (ChatDirection::Outgoing, Some(OutboundState::Failed { .. }), _) => {
                    metadata.add_css_class("bubble-meta-failed");
                }
                (ChatDirection::Outgoing, Some(OutboundState::Sending), _) => {
                    metadata.add_css_class("bubble-meta-sending");
                }
                (ChatDirection::Outgoing, _, Some(_)) => {
                    metadata.add_css_class("bubble-meta-delivered");
                }
                (ChatDirection::Outgoing, _, None) => {
                    metadata.add_css_class("bubble-meta-sent");
                }
                _ => {}
            }
            metadata.set_xalign(0.0);

            let bubble_column = GtkBox::new(Orientation::Vertical, 2);
            bubble_column.append(&bubble);
            if !grouped {
                let meta_row = GtkBox::new(Orientation::Horizontal, 6);
                meta_row.append(&metadata);

                if matches!(message.outbound_state, Some(OutboundState::Failed { .. })) {
                    let retry_button = Button::with_label("Retry");
                    retry_button.add_css_class("compact");
                    let send_button = send_button.clone();
                    let body_entry = body_entry.clone();
                    let retry_text = message.text.clone();
                    retry_button.connect_clicked(move |_| {
                        body_entry.set_text(&retry_text);
                        send_button.emit_clicked();
                    });
                    meta_row.append(&retry_button);
                }

                bubble_column.append(&meta_row);
            } else {
                bubble.add_css_class("chat-bubble-grouped");
            }

            match message.direction {
                ChatDirection::Outgoing => {
                    if message.delivered_at.is_some() {
                        bubble.add_css_class("chat-bubble-outgoing");
                    } else {
                        bubble.add_css_class("chat-bubble-pending");
                    }
                    bubble_wrap.set_halign(gtk4::Align::End);
                }
                ChatDirection::Incoming => {
                    bubble.add_css_class("chat-bubble-incoming");
                    bubble_wrap.set_halign(gtk4::Align::Start);
                }
            }

            bubble_wrap.append(&bubble_column);
            row.set_child(Some(&bubble_wrap));
            messages_list.append(&row);

            last_day_key = Some(day_key);
            last_direction = Some(message.direction.clone());
        }
    }

    if !auto_scroll_locked.get() {
        scroll_thread_to_bottom(messages_scroll.clone());
    }
}

fn clear_listbox(list: &ListBox) {
    while let Some(child) = list.first_child() {
        if let Ok(row) = child.downcast::<ListBoxRow>() {
            list.remove(&row);
        } else {
            break;
        }
    }
}

fn render_relay_list(
    relays: &[String],
    relay_list: &ListBox,
    relay_order: &Rc<RefCell<Vec<String>>>,
) {
    clear_listbox(relay_list);
    let mut normalized = relays
        .iter()
        .map(|item| normalize_http_endpoint(item))
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    *relay_order.borrow_mut() = normalized.clone();

    for relay in normalized {
        let row = ListBoxRow::new();
        row.add_css_class("contact-row");
        let label = Label::new(Some(&relay));
        label.set_xalign(0.0);
        label.set_ellipsize(EllipsizeMode::End);
        label.set_single_line_mode(true);
        row.set_child(Some(&label));
        relay_list.append(&row);
    }
}

fn select_relay_row_by_value(
    relay_list: &ListBox,
    relay_order: &Rc<RefCell<Vec<String>>>,
    relay_value: &str,
) {
    if let Some(idx) = relay_order
        .borrow()
        .iter()
        .position(|item| item == relay_value)
    {
        if let Some(row) = relay_list.row_at_index(idx as i32) {
            relay_list.select_row(Some(&row));
        }
    }
}

fn scroll_thread_to_bottom(messages_scroll: ScrolledWindow) {
    glib::timeout_add_local(Duration::from_millis(20), move || {
        let adj = messages_scroll.vadjustment();
        let bottom = (adj.upper() - adj.page_size()).max(adj.lower());
        adj.set_value(bottom);
        glib::ControlFlow::Break
    });
}

fn force_initial_thread_bottom(
    messages_scroll: ScrolledWindow,
    auto_scroll_locked: Rc<Cell<bool>>,
) {
    let attempts = Rc::new(Cell::new(0u8));
    glib::timeout_add_local(Duration::from_millis(70), move || {
        let next_attempt = attempts.get().saturating_add(1);
        attempts.set(next_attempt);

        let adj = messages_scroll.vadjustment();
        let has_layout = adj.upper() > adj.page_size() + 1.0;
        if has_layout {
            auto_scroll_locked.set(false);
            let bottom = (adj.upper() - adj.page_size()).max(adj.lower());
            adj.set_value(bottom);
            if next_attempt >= 3 {
                return glib::ControlFlow::Break;
            }
        }

        if next_attempt >= 10 {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

fn mark_contact_seen(state: &mut ChatState, contact_id: &str) {
    if let Some(messages) = state.threads.get_mut(contact_id) {
        for message in messages {
            if matches!(message.direction, ChatDirection::Incoming) {
                message.seen = true;
            }
        }
    }
}

fn short_wayfarer(value: &str) -> String {
    if value.len() <= 14 {
        return value.to_string();
    }
    format!("{}…{}", &value[0..8], &value[value.len() - 6..])
}

fn contact_display_name(state: &ChatState, wayfarer_id: &str) -> String {
    let raw = state
        .contact_aliases
        .get(wayfarer_id)
        .cloned()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| wayfarer_id.to_string());

    if raw == wayfarer_id {
        short_wayfarer(wayfarer_id)
    } else if raw.chars().count() > 28 {
        let head: String = raw.chars().take(18).collect();
        let tail: String = raw
            .chars()
            .rev()
            .take(6)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("{}…{}", head, tail)
    } else {
        raw
    }
}

fn tiny_wayfarer(value: &str) -> String {
    if value.len() <= 24 {
        return value.to_string();
    }
    format!("{}…{}", &value[0..12], &value[value.len() - 8..])
}

fn avatar_glyph(wayfarer_id: &str) -> String {
    let tail = wayfarer_id.chars().last().unwrap_or('0');
    match tail {
        '0' | '1' | '2' => "◉".to_string(),
        '3' | '4' | '5' => "◆".to_string(),
        '6' | '7' | '8' => "▲".to_string(),
        _ => "●".to_string(),
    }
}

fn short_msg_id(value: &str) -> String {
    if value.len() <= 14 {
        return value.to_string();
    }
    format!("{}…{}", &value[0..6], &value[value.len() - 6..])
}

fn format_timestamp_from_unix(unix_secs: i64) -> String {
    if let Ok(dt) = glib::DateTime::from_unix_local(unix_secs) {
        return dt
            .format("%I:%M %p")
            .map(|s| s.to_string())
            .unwrap_or_else(|_| "--:--".to_string());
    }
    "--:--".to_string()
}

fn format_timestamp_from_unix_ms(unix_ms: u64) -> String {
    format_timestamp_from_unix((unix_ms / 1000) as i64)
}

fn day_key_and_label(unix_secs: i64) -> (String, String) {
    if let Ok(dt) = glib::DateTime::from_unix_local(unix_secs.max(0)) {
        let key = dt
            .format("%Y-%m-%d")
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "unknown-day".to_string());

        if let Ok(now) = glib::DateTime::now_local() {
            if now
                .format("%Y-%m-%d")
                .map(|v| v.to_string())
                .ok()
                .as_deref()
                == Some(key.as_str())
            {
                return (key, "Today".to_string());
            }

            if let Ok(yesterday) = now.add_days(-1) {
                if yesterday
                    .format("%Y-%m-%d")
                    .map(|v| v.to_string())
                    .ok()
                    .as_deref()
                    == Some(key.as_str())
                {
                    return (key, "Yesterday".to_string());
                }
            }
        }

        let label = dt
            .format("%b %d, %Y")
            .map(|v| v.to_string())
            .unwrap_or_else(|_| key.clone());
        return (key, label);
    }

    ("unknown-day".to_string(), "Earlier".to_string())
}

fn bytes_to_hex_lower(input: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(input.len() * 2);
    for byte in input {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn now_unix_secs() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as i64,
        Err(_) => 0,
    }
}

fn now_unix_ms() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(_) => 0,
    }
}

fn next_local_outgoing_id() -> String {
    let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos(),
        Err(_) => 0,
    };
    format!("local-{nanos}")
}

fn pulse_widget<W>(widget: &W, class_name: &'static str)
where
    W: IsA<gtk4::Widget> + Clone + 'static,
{
    let widget = widget.clone().upcast::<gtk4::Widget>();
    widget.add_css_class(class_name);
    glib::timeout_add_local(Duration::from_millis(240), move || {
        widget.remove_css_class(class_name);
        glib::ControlFlow::Break
    });
}

fn show_copied_popover<W>(widget: &W)
where
    W: IsA<gtk4::Widget> + Clone + 'static,
{
    let popover = Popover::new();
    popover.set_has_arrow(true);
    popover.set_autohide(true);
    popover.set_position(PositionType::Top);
    popover.set_parent(widget);

    let label = Label::new(Some("Copied"));
    label.add_css_class("compact");
    popover.set_child(Some(&label));
    popover.popup();

    glib::timeout_add_local(Duration::from_millis(800), move || {
        popover.popdown();
        popover.unparent();
        glib::ControlFlow::Break
    });
}

fn ensure_linux_desktop_integration() -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME")
            .map_err(|_| "HOME not set for desktop integration".to_string())?;

        let applications_dir = Path::new(&home).join(".local/share/applications");
        let icon_root = Path::new(&home).join(".local/share/icons/hicolor");

        fs::create_dir_all(&applications_dir)
            .map_err(|err| format!("failed creating applications dir: {err}"))?;

        let icon_source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/img/logo.png");
        if icon_source.exists() {
            let image = image::open(&icon_source)
                .map_err(|err| format!("failed to decode app icon source: {err}"))?;
            let icon_sizes: [u32; 9] = [16, 24, 32, 48, 64, 96, 128, 256, 512];

            for size in icon_sizes {
                let icon_dir = icon_root.join(format!("{size}x{size}/apps"));
                fs::create_dir_all(&icon_dir).map_err(|err| {
                    format!("failed creating icon dir {}: {err}", icon_dir.display())
                })?;
                let icon_target = icon_dir.join(format!("{}.png", APP_ID));
                let resized = image.resize_exact(size, size, FilterType::Lanczos3);
                resized.save(&icon_target).map_err(|err| {
                    format!(
                        "failed writing resized icon {}: {err}",
                        icon_target.display()
                    )
                })?;
            }
        }

        let desktop_path = applications_dir.join(format!("{}.desktop", APP_ID));
        let exec = std::env::current_exe()
            .map_err(|err| format!("failed to determine executable path: {err}"))?;

        let desktop = format!(
            "[Desktop Entry]\nType=Application\nName=Aethos Linux\nExec={}\nIcon={}\nTerminal=false\nCategories=Network;Chat;\nStartupNotify=true\nStartupWMClass={}\n",
            shell_escape(exec.as_os_str().to_string_lossy().as_ref()),
            APP_ID,
            APP_ID,
        );

        fs::write(&desktop_path, desktop)
            .map_err(|err| format!("failed to write desktop entry: {err}"))?;
    }

    Ok(())
}

fn shell_escape(value: &str) -> String {
    if !value.contains([' ', '\'', '"']) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn build_version_text() -> String {
    let git_sha = option_env!("AETHOS_GIT_SHA").unwrap_or("dev");
    format!("Aethos Linux v{APP_VERSION} (build {git_sha})")
}

fn append_local_log(message: &str) {
    if let Err(err) = append_local_log_inner(message) {
        eprintln!("local log warning: {err}");
    }
}

fn append_local_log_inner(message: &str) -> Result<(), String> {
    let path = app_log_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed creating app log directory: {err}"))?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| format!("failed opening app log file at {}: {err}", path.display()))?;

    let now = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(_) => 0,
    };

    writeln!(file, "[{now}] {message}")
        .map_err(|err| format!("failed writing app log file at {}: {err}", path.display()))
}

fn app_log_file_path() -> PathBuf {
    if let Ok(xdg_state_home) = std::env::var("XDG_STATE_HOME") {
        if !xdg_state_home.trim().is_empty() {
            return Path::new(&xdg_state_home)
                .join("aethos-linux")
                .join(APP_LOG_FILE_NAME);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        return Path::new(&home)
            .join(".local")
            .join("state")
            .join("aethos-linux")
            .join(APP_LOG_FILE_NAME);
    }

    std::env::temp_dir().join(APP_LOG_FILE_NAME)
}

fn open_log_folder() -> Result<(), String> {
    let log_dir = app_log_file_path()
        .parent()
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    Command::new("xdg-open")
        .arg(&log_dir)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("{err}"))
}

fn setup_wave_strip(wave_strip: &DrawingArea, phase: Rc<Cell<f64>>, pending: Rc<Cell<bool>>) {
    let phase_draw = Rc::clone(&phase);
    let pending_draw = Rc::clone(&pending);
    wave_strip.set_draw_func(move |_area, cr, width, height| {
        let w = width as f64;
        let h = height as f64;
        let t = phase_draw.get();
        let active = pending_draw.get();

        cr.set_source_rgba(0.31, 0.43, 0.86, if active { 0.12 } else { 0.08 });
        cr.rectangle(0.0, 0.0, w, h);
        let _ = cr.fill();

        let base_alpha = if active { 0.58 } else { 0.30 };
        let amp = if active { 5.0 } else { 3.0 };
        let speed_scale = if active { 1.0 } else { 0.5 };

        for idx in 0..3 {
            let y = 10.0 + (idx as f64 * 10.0);
            let freq = 0.022 + (idx as f64 * 0.004);
            let phase_offset = (idx as f64) * 1.15;
            let dir = if idx == 1 { -1.0 } else { 1.0 };
            let drift = ((t * 0.18) + (idx as f64 * 0.7)).sin() * 0.18;

            let (r, g, b) = if idx == 0 {
                (0.33, 0.67, 0.98)
            } else if idx == 1 {
                (0.49, 0.48, 0.98)
            } else {
                (0.67, 0.42, 0.98)
            };
            let rr = (r + drift * 0.35).clamp(0.0, 1.0);
            let gg = (g - drift * 0.22).clamp(0.0, 1.0);
            let bb = (b + drift * 0.27).clamp(0.0, 1.0);
            cr.set_source_rgba(rr, gg, bb, (base_alpha - (idx as f64 * 0.08)).max(0.08));
            cr.set_line_width(if active { 1.8 } else { 1.3 });

            let mut x = 0.0;
            cr.move_to(x, y);
            while x <= w {
                let wave = (x * freq + (t * speed_scale * dir) + phase_offset).sin() * amp;
                cr.line_to(x, y + wave);
                x += 6.0;
            }
            let _ = cr.stroke();

            let bead_count = if active { 4 } else { 3 };
            for bead_idx in 0..bead_count {
                let lane_speed = if idx == 0 {
                    10.2
                } else if idx == 1 {
                    8.2
                } else {
                    6.9
                };
                let bead_speed = lane_speed + (bead_idx as f64 * 0.85);
                let travel = ((t * speed_scale * bead_speed * dir)
                    + (bead_idx as f64 * (w / bead_count as f64))
                    + (idx as f64 * 21.0))
                    .rem_euclid(w + 32.0);
                let bx = travel - 16.0;
                let by = y + (bx * freq + (t * speed_scale * dir) + phase_offset).sin() * amp;
                let radius = if active { 1.55 } else { 1.3 };
                let pulse =
                    0.74 + 0.26 * ((t * 0.55) + (idx as f64 * 0.9) + (bead_idx as f64 * 1.1)).sin();
                let bead_alpha = (if active { 0.56 } else { 0.40 }) * pulse;

                cr.set_source_rgba(0.97, 0.98, 1.0, bead_alpha.clamp(0.24, 0.62));
                cr.arc(bx, by, radius, 0.0, std::f64::consts::TAU);
                let _ = cr.fill();
            }
        }
    });

    let wave_strip_tick = wave_strip.clone();
    glib::timeout_add_local(Duration::from_millis(45), move || {
        let speed = if pending.get() { 0.20 } else { 0.08 };
        phase.set(phase.get() + speed);
        wave_strip_tick.queue_draw();
        glib::ControlFlow::Continue
    });
}

fn has_pending_outbound(state: &ChatState) -> bool {
    state.threads.values().any(|messages| {
        messages.iter().any(|message| {
            matches!(message.direction, ChatDirection::Outgoing) && message.delivered_at.is_none()
        })
    })
}

fn load_persisted_chat_state() -> Result<Option<PersistedChatState>, String> {
    let path = chat_history_file_path();
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path).map_err(|err| {
        format!(
            "failed to read chat history file at {}: {err}",
            path.display()
        )
    })?;
    let data: PersistedChatState = serde_json::from_str(&content).map_err(|err| {
        format!(
            "failed to parse chat history file at {}: {err}",
            path.display()
        )
    })?;
    Ok(Some(data))
}

fn save_persisted_chat_state(state: &ChatState) -> Result<(), String> {
    let path = chat_history_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed creating chat history directory: {err}"))?;
    }

    let payload = PersistedChatState {
        selected_contact: state.selected_contact.clone(),
        threads: state.threads.clone(),
    };
    let serialized = serde_json::to_string_pretty(&payload)
        .map_err(|err| format!("failed to serialize chat history: {err}"))?;
    fs::write(&path, serialized).map_err(|err| {
        format!(
            "failed to write chat history file at {}: {err}",
            path.display()
        )
    })
}

fn chat_history_file_path() -> PathBuf {
    if let Ok(xdg_state_home) = std::env::var("XDG_STATE_HOME") {
        if !xdg_state_home.trim().is_empty() {
            return Path::new(&xdg_state_home)
                .join("aethos-linux")
                .join(CHAT_HISTORY_FILE_NAME);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        return Path::new(&home)
            .join(".local")
            .join("state")
            .join("aethos-linux")
            .join(CHAT_HISTORY_FILE_NAME);
    }

    std::env::temp_dir().join(CHAT_HISTORY_FILE_NAME)
}

fn load_app_settings() -> Result<AppSettings, String> {
    let path = app_settings_file_path();
    if !path.exists() {
        return Ok(AppSettings::default());
    }

    let content = fs::read_to_string(&path)
        .map_err(|err| format!("failed reading app settings at {}: {err}", path.display()))?;
    let mut settings: AppSettings = serde_json::from_str(&content)
        .map_err(|err| format!("failed parsing app settings at {}: {err}", path.display()))?;
    if settings.relay_endpoints.is_empty() {
        settings.relay_endpoints = AppSettings::default().relay_endpoints;
    }
    settings
        .relay_endpoints
        .iter_mut()
        .for_each(|endpoint| *endpoint = normalize_http_endpoint(endpoint));
    settings.relay_endpoints.sort();
    settings.relay_endpoints.dedup();
    Ok(settings)
}

fn update_relay_sync_setting(
    app_settings: &Rc<RefCell<AppSettings>>,
    relay_sync_enabled: &Arc<AtomicBool>,
    enabled: bool,
) -> AppSettings {
    let mut settings = app_settings.borrow_mut();
    settings.relay_sync_enabled = enabled;
    relay_sync_enabled.store(enabled, Ordering::SeqCst);
    settings.clone()
}

fn save_app_settings(settings: &AppSettings) -> Result<(), String> {
    let path = app_settings_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed creating app settings directory: {err}"))?;
    }

    let mut normalized = settings.clone();
    normalized
        .relay_endpoints
        .iter_mut()
        .for_each(|endpoint| *endpoint = normalize_http_endpoint(endpoint));
    normalized.relay_endpoints.sort();
    normalized.relay_endpoints.dedup();

    let serialized = serde_json::to_string_pretty(&normalized)
        .map_err(|err| format!("failed serializing app settings: {err}"))?;
    fs::write(&path, serialized)
        .map_err(|err| format!("failed writing app settings at {}: {err}", path.display()))
}

fn app_settings_file_path() -> PathBuf {
    if let Ok(xdg_state_home) = std::env::var("XDG_STATE_HOME") {
        if !xdg_state_home.trim().is_empty() {
            return Path::new(&xdg_state_home)
                .join("aethos-linux")
                .join(APP_SETTINGS_FILE_NAME);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        return Path::new(&home)
            .join(".local")
            .join("state")
            .join("aethos-linux")
            .join(APP_SETTINGS_FILE_NAME);
    }

    std::env::temp_dir().join(APP_SETTINGS_FILE_NAME)
}

fn share_qr_file_path() -> PathBuf {
    if let Ok(xdg_state_home) = std::env::var("XDG_STATE_HOME") {
        if !xdg_state_home.trim().is_empty() {
            return Path::new(&xdg_state_home)
                .join("aethos-linux")
                .join(SHARE_QR_FILE_NAME);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        return Path::new(&home)
            .join(".local")
            .join("state")
            .join("aethos-linux")
            .join(SHARE_QR_FILE_NAME);
    }

    std::env::temp_dir().join(SHARE_QR_FILE_NAME)
}

fn refresh_share_qr(wayfarer_id: &str, share_qr_image: &Image, share_status_label: &Label) {
    match generate_share_qr_png(wayfarer_id) {
        Ok(path) => {
            share_qr_image.set_from_file(path.to_str());
            share_status_label.set_text("QR ready to share");
        }
        Err(err) => {
            share_qr_image.set_from_file(Option::<&str>::None);
            share_status_label.set_text(&format!("QR generation failed: {err}"));
        }
    }
}

fn generate_share_qr_png(wayfarer_id: &str) -> Result<PathBuf, String> {
    let code = QrCode::new(wayfarer_id.as_bytes())
        .map_err(|err| format!("failed generating QR payload: {err}"))?;
    let scale: u32 = 8;
    let border: u32 = 4;
    let luma: ImageBuffer<Luma<u8>, Vec<u8>> = code
        .render::<Luma<u8>>()
        .quiet_zone(false)
        .module_dimensions(scale, scale)
        .build();

    let inner_w = luma.width();
    let inner_h = luma.height();
    let width = inner_w + border * scale * 2;
    let height = inner_h + border * scale * 2;
    let mut rgba = RgbaImage::from_pixel(width, height, Rgba([255, 255, 255, 255]));

    for y in 0..inner_h {
        for x in 0..inner_w {
            let px = luma.get_pixel(x, y).0[0];
            let color = if px < 128 {
                Rgba([16, 18, 28, 255])
            } else {
                Rgba([255, 255, 255, 255])
            };
            rgba.put_pixel(x + border * scale, y + border * scale, color);
        }
    }

    let monogram = a_monogram_icon_rgba((width / 6).max(36));
    overlay_center(&mut rgba, &monogram);

    let path = share_qr_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("failed creating share qr dir: {err}"))?;
    }
    rgba.save(&path)
        .map_err(|err| format!("failed saving share QR image: {err}"))?;
    Ok(path)
}

fn overlay_center(base: &mut RgbaImage, overlay: &RgbaImage) {
    let offset_x = base.width().saturating_sub(overlay.width()) / 2;
    let offset_y = base.height().saturating_sub(overlay.height()) / 2;

    for y in 0..overlay.height() {
        for x in 0..overlay.width() {
            let src = overlay.get_pixel(x, y);
            let alpha = src[3] as f32 / 255.0;
            if alpha <= 0.0 {
                continue;
            }

            let dst = base.get_pixel_mut(x + offset_x, y + offset_y);
            for i in 0..3 {
                let blended = (src[i] as f32 * alpha) + (dst[i] as f32 * (1.0 - alpha));
                dst[i] = blended.round() as u8;
            }
            dst[3] = 255;
        }
    }
}

fn a_monogram_icon_rgba(size: u32) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(size, size, Rgba([255, 255, 255, 0]));
    let cx = size as f32 * 0.5;
    let cy = size as f32 * 0.5;
    let radius = size as f32 * 0.44;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= radius {
                img.put_pixel(x, y, Rgba([250, 252, 255, 245]));
            }
        }
    }

    let stroke = Rgba([33, 79, 188, 255]);
    let left_x = (size as f32 * 0.32) as i32;
    let right_x = (size as f32 * 0.68) as i32;
    let top_y = (size as f32 * 0.28) as i32;
    let bottom_y = (size as f32 * 0.74) as i32;
    let cross_y = (size as f32 * 0.54) as i32;

    draw_line(
        &mut img,
        left_x,
        bottom_y,
        (size as f32 * 0.5) as i32,
        top_y,
        stroke,
    );
    draw_line(
        &mut img,
        right_x,
        bottom_y,
        (size as f32 * 0.5) as i32,
        top_y,
        stroke,
    );
    draw_line(
        &mut img,
        (size as f32 * 0.39) as i32,
        cross_y,
        (size as f32 * 0.61) as i32,
        cross_y,
        stroke,
    );

    img
}

fn draw_line(img: &mut RgbaImage, mut x0: i32, mut y0: i32, x1: i32, y1: i32, color: Rgba<u8>) {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        for oy in -1..=1 {
            for ox in -1..=1 {
                let px = x0 + ox;
                let py = y0 + oy;
                if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                    img.put_pixel(px as u32, py as u32, color);
                }
            }
        }

        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

fn apply_styles() {
    let provider = CssProvider::new();
    provider.load_from_data(
        "
        window {
            background: radial-gradient(circle at 70% -20%, rgba(51, 167, 255, 0.18), rgba(16, 27, 61, 0) 36%),
                        radial-gradient(circle at 15% 20%, rgba(137, 92, 255, 0.18), rgba(16, 27, 61, 0) 40%),
                        linear-gradient(180deg, #060814 0%, #050612 100%);
            color: #f1f3ff;
            font-family: \"SF Pro Display\", \"Inter\", \"Noto Sans\", sans-serif;
        }

        .root {
            background: transparent;
        }

        .thread-contact-id {
            font-size: 11px;
            color: rgba(149, 159, 191, 0.95);
        }

        .footer-bar {
            border-top: 1px solid rgba(107, 120, 172, 0.28);
            margin-top: 6px;
            padding-top: 8px;
        }

        .footer-note {
            font-size: 11px;
            color: rgba(139, 149, 181, 0.9);
        }

        .footer-logo {
            border-radius: 6px;
            opacity: 0.65;
        }

        .relay-chip {
            border-radius: 12px;
            padding: 4px 10px;
            border: 1px solid rgba(98, 111, 166, 0.36);
            background: rgba(19, 24, 45, 0.84);
        }

        .gossip-chip {
            border-radius: 12px;
            padding: 4px 10px;
            border: 1px solid rgba(84, 127, 160, 0.34);
            background: rgba(16, 28, 40, 0.82);
        }

        .gossip-chip-active {
            border-color: rgba(116, 149, 255, 0.56);
            background: rgba(29, 39, 78, 0.86);
        }

        .gossip-chip-disabled {
            border-color: rgba(118, 124, 145, 0.32);
            background: rgba(29, 31, 41, 0.8);
        }

        .relay-chip-disabled {
            border-color: rgba(118, 124, 145, 0.32);
            background: rgba(29, 31, 41, 0.8);
        }

        .relay-dot {
            font-size: 12px;
            font-weight: 800;
        }

        .relay-dot-idle {
            color: rgba(140, 151, 185, 0.9);
        }

        .relay-dot-ok {
            color: rgba(116, 184, 255, 0.96);
        }

        .relay-dot-warn {
            color: rgba(255, 193, 74, 0.95);
        }

        .relay-dot-down {
            color: rgba(251, 97, 124, 0.95);
        }

        .relay-dot-disabled {
            color: rgba(168, 173, 191, 0.9);
        }

        .gossip-dot {
            font-size: 12px;
            font-weight: 800;
        }

        .gossip-dot-idle {
            color: rgba(108, 179, 228, 0.9);
        }

        .gossip-dot-active {
            color: rgba(170, 154, 255, 0.96);
        }

        .gossip-dot-disabled {
            color: rgba(168, 173, 191, 0.9);
        }

        .relay-chip-text {
            font-size: 11px;
            color: rgba(198, 207, 239, 0.94);
        }

        .wave-strip {
            border-radius: 12px;
            margin-top: 2px;
            margin-bottom: 2px;
            border: 1px solid rgba(116, 129, 213, 0.26);
            background: linear-gradient(90deg, rgba(48, 74, 174, 0.22), rgba(97, 66, 201, 0.2));
        }

        .wave-mode-label {
            font-size: 11px;
            color: rgba(162, 174, 223, 0.92);
            margin-bottom: 6px;
        }

        .settings-shell {
            padding: 8px;
        }

        .settings-group-title {
            font-size: 13px;
            font-weight: 700;
            color: rgba(182, 193, 231, 0.95);
            letter-spacing: 0.02em;
            margin-top: 6px;
        }

        .settings-group-hint {
            font-size: 12px;
            color: rgba(150, 161, 197, 0.9);
            margin-bottom: 2px;
        }

        .field-label {
            font-size: 12px;
            font-weight: 700;
            color: rgba(192, 203, 238, 0.96);
            margin-top: 4px;
        }

        .field-hint {
            font-size: 11px;
            color: rgba(143, 155, 192, 0.9);
        }

        .settings-card {
            background: rgba(25, 29, 50, 0.78);
            border: 1px solid rgba(113, 128, 186, 0.3);
            border-radius: 14px;
            padding: 12px;
        }

        .settings-scroll {
            border-radius: 12px;
        }

        .section-title {
            font-size: 17px;
            font-weight: 700;
            color: #c2b2ff;
            letter-spacing: 0.04em;
        }

        .glass-panel {
            border-radius: 18px;
            padding: 14px;
            border: 1px solid rgba(140, 154, 216, 0.26);
            background: linear-gradient(180deg, rgba(31, 35, 56, 0.9), rgba(18, 21, 39, 0.9));
            box-shadow: 0 12px 26px rgba(4, 6, 20, 0.55);
        }

        entry, textview, list {
            border-radius: 10px;
            border: 1px solid rgba(114, 126, 180, 0.42);
            background: rgba(17, 21, 41, 0.82);
            color: #f1f3ff;
            padding: 8px;
        }

        entry placeholder {
            color: rgba(149, 156, 182, 0.75);
        }

        stackswitcher button {
            border-radius: 10px;
            margin-right: 6px;
            border: 1px solid rgba(103, 115, 171, 0.46);
            background: rgba(19, 23, 45, 0.84);
            color: #8f98b4;
            padding: 7px 12px;
        }

        stackswitcher button:checked {
            color: #e7ebff;
            background: linear-gradient(90deg, rgba(33, 119, 214, 0.65), rgba(65, 89, 213, 0.65));
            border-color: rgba(100, 171, 255, 0.7);
        }

        button.action {
            border-radius: 10px;
            border: 1px solid rgba(88, 165, 255, 0.58);
            background: linear-gradient(90deg, rgba(20, 117, 231, 0.88), rgba(49, 137, 247, 0.88));
            color: #f3f7ff;
            font-weight: 700;
            padding: 8px 12px;
        }

        button.compact {
            border-radius: 9px;
            border: 1px solid rgba(108, 126, 194, 0.42);
            background: rgba(23, 28, 53, 0.86);
            color: rgba(198, 208, 242, 0.96);
            padding: 5px 9px;
        }

        button.danger {
            border-radius: 10px;
            border: 1px solid rgba(251, 121, 150, 0.52);
            background: linear-gradient(90deg, rgba(145, 44, 74, 0.74), rgba(119, 35, 64, 0.74));
            color: #ffe7ef;
            font-weight: 700;
            padding: 8px 12px;
        }

        .warning {
            color: rgba(255, 194, 205, 0.95);
            font-size: 13px;
        }

        .chat-shell {
            min-height: 300px;
        }

        .contacts-pane {
            min-width: 230px;
        }

        .composer-bar {
            border-radius: 18px;
            padding: 7px;
            border: 1px solid rgba(95, 109, 169, 0.38);
            background: rgba(20, 24, 43, 0.86);
        }

        .message-entry {
            border-radius: 15px;
            min-height: 42px;
        }

        .recipient-entry {
            font-size: 12px;
        }

        .send-fab {
            border-radius: 19px;
            min-width: 38px;
            min-height: 38px;
            padding: 0;
            font-size: 18px;
        }

        .pulse-send {
            box-shadow: 0 0 0 5px rgba(56, 167, 255, 0.22);
            border-color: rgba(131, 208, 255, 0.92);
        }

        .pulse-receive {
            box-shadow: inset 0 0 0 1px rgba(153, 187, 255, 0.52);
            background: rgba(35, 43, 79, 0.42);
        }

        .contact-list row {
            border-radius: 12px;
            margin-bottom: 5px;
            background: rgba(21, 26, 49, 0.7);
            border: 1px solid rgba(101, 111, 162, 0.26);
            padding: 6px;
        }

        .contact-list row:selected {
            background: rgba(63, 74, 124, 0.82);
            border-color: rgba(113, 150, 236, 0.7);
        }

        .contact-avatar {
            border-radius: 18px;
            min-width: 36px;
            min-height: 36px;
            background: linear-gradient(180deg, rgba(34, 111, 182, 0.9), rgba(28, 74, 138, 0.9));
            color: #a995ff;
            font-size: 14px;
            font-weight: 700;
            padding: 7px;
        }

        .contact-title {
            font-size: 15px;
            font-weight: 700;
            color: #eef2ff;
        }

        .contact-subtitle {
            font-size: 11px;
            color: rgba(157, 166, 200, 0.9);
        }

        .unread-badge {
            border-radius: 10px;
            background: rgba(17, 145, 245, 0.9);
            color: #f6fbff;
            font-size: 11px;
            font-weight: 700;
            padding: 2px 7px;
        }

        .new-badge {
            border-radius: 10px;
            background: rgba(103, 84, 235, 0.92);
            color: #f8f5ff;
            font-size: 10px;
            font-weight: 800;
            padding: 2px 7px;
        }

        .messages-list row {
            background: transparent;
            border: none;
            margin: 2px 0;
        }

        .day-separator {
            font-size: 11px;
            color: rgba(151, 162, 198, 0.9);
            margin: 8px 0 4px 0;
        }

        .chat-bubble {
            border-radius: 16px;
            padding: 8px 11px;
            line-height: 1.3;
            font-size: 14px;
        }

        .chat-bubble-grouped {
            margin-top: -2px;
        }

        .chat-bubble-incoming {
            background: rgba(54, 60, 84, 0.92);
            color: #eff2ff;
        }

        .chat-bubble-outgoing {
            background: rgba(67, 60, 115, 0.94);
            color: #eff2ff;
        }

        .chat-bubble-pending {
            background: rgba(67, 60, 115, 0.58);
            color: rgba(239, 242, 255, 0.88);
            border: 1px dashed rgba(164, 170, 210, 0.45);
        }

        .bubble-meta {
            font-size: 11px;
            color: rgba(155, 164, 196, 0.8);
        }

        .bubble-meta-sending {
            color: rgba(172, 181, 208, 0.92);
        }

        .bubble-meta-sent {
            color: rgba(134, 188, 245, 0.92);
        }

        .bubble-meta-delivered {
            color: rgba(156, 198, 255, 0.94);
        }

        .bubble-meta-failed {
            color: rgba(255, 146, 162, 0.95);
        }

        expander > title {
            color: rgba(162, 173, 213, 0.95);
            font-weight: 600;
        }

        expander > title > arrow {
            color: rgba(133, 147, 201, 0.95);
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

#[cfg(test)]
mod tests {
    use super::{update_relay_sync_setting, AppSettings};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn relay_toggle_helper_updates_state_and_atomic_flag() {
        let app_settings = Rc::new(RefCell::new(AppSettings::default()));
        let relay_sync_enabled = Arc::new(AtomicBool::new(true));

        let snapshot = update_relay_sync_setting(&app_settings, &relay_sync_enabled, false);

        assert!(!snapshot.relay_sync_enabled);
        assert!(!app_settings.borrow().relay_sync_enabled);
        assert!(!relay_sync_enabled.load(Ordering::SeqCst));
    }

    #[test]
    fn relay_toggle_helper_drops_mut_borrow_before_followup_work() {
        let app_settings = Rc::new(RefCell::new(AppSettings::default()));
        let relay_sync_enabled = Arc::new(AtomicBool::new(false));

        let _snapshot = update_relay_sync_setting(&app_settings, &relay_sync_enabled, true);

        let mut writable = app_settings.borrow_mut();
        writable.relay_sync_enabled = false;
        drop(writable);

        assert!(!app_settings.borrow().relay_sync_enabled);
    }
}
