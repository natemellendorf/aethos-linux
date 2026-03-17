use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::relay::client::normalize_http_endpoint;

const CHAT_HISTORY_FILE_NAME: &str = "chat-history.json";
const APP_SETTINGS_FILE_NAME: &str = "settings.json";
const DEFAULT_RELAY_HTTP_PRIMARY: &str = "http://192.168.1.200:8082";
const DEFAULT_RELAY_HTTP_SECONDARY: &str = "http://192.168.1.200:9082";
const DEFAULT_MESSAGE_TTL_SECONDS: u64 = 3600;
const MIN_MESSAGE_TTL_SECONDS: u64 = 60;
const MAX_MESSAGE_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
const DEFAULT_ENTER_TO_SEND: bool = true;

fn default_enter_to_send() -> bool {
    DEFAULT_ENTER_TO_SEND
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ChatDirection {
    Incoming,
    Outgoing,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutboundState {
    #[serde(alias = "Sending")]
    Sending,
    #[serde(alias = "Sent")]
    Sent,
    #[serde(alias = "Failed")]
    Failed { error: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    #[serde(alias = "msg_id")]
    pub msg_id: String,
    pub text: String,
    pub timestamp: String,
    #[serde(alias = "created_at_unix")]
    #[serde(default)]
    pub created_at_unix: i64,
    pub direction: ChatDirection,
    #[serde(default)]
    pub seen: bool,
    #[serde(default)]
    #[serde(alias = "manifest_id_hex")]
    pub manifest_id_hex: Option<String>,
    #[serde(alias = "delivered_at")]
    #[serde(default)]
    pub delivered_at: Option<String>,
    #[serde(alias = "outbound_state")]
    #[serde(default)]
    pub outbound_state: Option<OutboundState>,
    #[serde(alias = "expires_at_unix_ms")]
    #[serde(default)]
    pub expires_at_unix_ms: Option<u64>,
    #[serde(alias = "last_sync_attempt_unix_ms")]
    #[serde(default)]
    pub last_sync_attempt_unix_ms: Option<u64>,
    #[serde(alias = "last_sync_error")]
    #[serde(default)]
    pub last_sync_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedChatState {
    #[serde(alias = "selected_contact")]
    pub selected_contact: Option<String>,
    #[serde(alias = "new_contacts")]
    #[serde(default)]
    pub new_contacts: Vec<String>,
    pub threads: BTreeMap<String, Vec<ChatMessage>>,
}

impl Default for PersistedChatState {
    fn default() -> Self {
        Self {
            selected_contact: None,
            new_contacts: Vec::new(),
            threads: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(alias = "relay_sync_enabled")]
    pub relay_sync_enabled: bool,
    #[serde(alias = "gossip_sync_enabled")]
    pub gossip_sync_enabled: bool,
    #[serde(alias = "verbose_logging_enabled")]
    #[serde(default)]
    pub verbose_logging_enabled: bool,
    #[serde(alias = "relay_endpoints")]
    pub relay_endpoints: Vec<String>,
    #[serde(alias = "message_ttl_seconds")]
    #[serde(default = "default_message_ttl_seconds")]
    pub message_ttl_seconds: u64,
    #[serde(alias = "enter_to_send")]
    #[serde(default = "default_enter_to_send")]
    pub enter_to_send: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            relay_sync_enabled: true,
            gossip_sync_enabled: true,
            verbose_logging_enabled: false,
            relay_endpoints: vec![
                DEFAULT_RELAY_HTTP_PRIMARY.to_string(),
                DEFAULT_RELAY_HTTP_SECONDARY.to_string(),
            ],
            message_ttl_seconds: DEFAULT_MESSAGE_TTL_SECONDS,
            enter_to_send: DEFAULT_ENTER_TO_SEND,
        }
    }
}

pub fn load_chat_state() -> Result<PersistedChatState, String> {
    let path = chat_history_file_path();
    if !path.exists() {
        return Ok(PersistedChatState::default());
    }

    let content = fs::read_to_string(&path).map_err(|err| {
        format!(
            "failed to read chat history file at {}: {err}",
            path.display()
        )
    })?;
    let mut state: PersistedChatState = match serde_json::from_str(&content) {
        Ok(parsed) => parsed,
        Err(first_err) => {
            let migrated = content
                .replace("\"Sending\"", "\"sending\"")
                .replace("\"Sent\"", "\"sent\"")
                .replace("\"Failed\"", "\"failed\"");
            serde_json::from_str(&migrated).map_err(|err| {
                format!(
                    "failed to parse chat history file at {}: {first_err}; retry after legacy migration failed: {err}",
                    path.display()
                )
            })?
        }
    };
    normalize_chat_state(&mut state);
    Ok(state)
}

pub fn save_chat_state(state: &PersistedChatState) -> Result<(), String> {
    let mut normalized = state.clone();
    normalize_chat_state(&mut normalized);
    let path = chat_history_file_path();
    ensure_parent_dir(&path)?;
    let serialized = serde_json::to_string_pretty(&normalized)
        .map_err(|err| format!("failed to serialize chat history: {err}"))?;
    fs::write(&path, serialized).map_err(|err| {
        format!(
            "failed to write chat history file at {}: {err}",
            path.display()
        )
    })
}

pub fn load_app_settings() -> Result<AppSettings, String> {
    let path = app_settings_file_path();
    if !path.exists() {
        return Ok(AppSettings::default());
    }

    let content = fs::read_to_string(&path)
        .map_err(|err| format!("failed reading app settings at {}: {err}", path.display()))?;
    let mut settings: AppSettings = serde_json::from_str(&content)
        .map_err(|err| format!("failed parsing app settings at {}: {err}", path.display()))?;
    normalize_settings(&mut settings);
    Ok(settings)
}

pub fn save_app_settings(settings: &AppSettings) -> Result<AppSettings, String> {
    let mut normalized = settings.clone();
    normalize_settings(&mut normalized);
    let path = app_settings_file_path();
    ensure_parent_dir(&path)?;
    let serialized = serde_json::to_string_pretty(&normalized)
        .map_err(|err| format!("failed serializing app settings: {err}"))?;
    fs::write(&path, serialized)
        .map_err(|err| format!("failed writing app settings at {}: {err}", path.display()))?;
    Ok(normalized)
}

fn normalize_settings(settings: &mut AppSettings) {
    if settings.relay_endpoints.is_empty() {
        settings.relay_endpoints = AppSettings::default().relay_endpoints;
    }
    settings
        .relay_endpoints
        .iter_mut()
        .for_each(|endpoint| *endpoint = normalize_http_endpoint(endpoint));
    settings.relay_endpoints.sort();
    settings.relay_endpoints.dedup();
    settings.message_ttl_seconds = settings
        .message_ttl_seconds
        .clamp(MIN_MESSAGE_TTL_SECONDS, MAX_MESSAGE_TTL_SECONDS);
}

pub fn normalize_chat_state(chat: &mut PersistedChatState) {
    chat.new_contacts.sort();
    chat.new_contacts.dedup();
}

fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed creating directory {}: {err}", parent.display()))?;
    }
    Ok(())
}

fn app_state_base_dir() -> PathBuf {
    if let Ok(override_dir) = std::env::var("AETHOS_STATE_DIR") {
        if !override_dir.trim().is_empty() {
            return PathBuf::from(override_dir);
        }
    }

    if let Ok(xdg_state_home) = std::env::var("XDG_STATE_HOME") {
        if !xdg_state_home.trim().is_empty() {
            return Path::new(&xdg_state_home).join("aethos-linux");
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(app_data) = std::env::var("APPDATA") {
            if !app_data.trim().is_empty() {
                return Path::new(&app_data).join("aethos-linux");
            }
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            #[cfg(target_os = "macos")]
            {
                return Path::new(&home)
                    .join("Library")
                    .join("Application Support")
                    .join("aethos-linux");
            }

            #[cfg(not(target_os = "macos"))]
            {
                return Path::new(&home)
                    .join(".local")
                    .join("state")
                    .join("aethos-linux");
            }
        }
    }

    std::env::temp_dir().join("aethos-linux")
}

fn chat_history_file_path() -> PathBuf {
    app_state_base_dir().join(CHAT_HISTORY_FILE_NAME)
}

fn app_settings_file_path() -> PathBuf {
    app_state_base_dir().join(APP_SETTINGS_FILE_NAME)
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn default_message_ttl_seconds() -> u64 {
    DEFAULT_MESSAGE_TTL_SECONDS
}
