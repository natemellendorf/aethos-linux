use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const APP_DIR_NAME: &str = "aethos-linux";
const DEFAULT_PROFILE: &str = "default";
const IDENTITY_FILE_PREFIX: &str = "identity";
const SESSION_CACHE_FILE_PREFIX: &str = "session-cache";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredIdentity {
    wayfair_id: String,
    signing_key_b64: String,
    device_name: String,
    platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedEnvelope {
    nonce_b64: String,
    ciphertext_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelaySessionCache {
    pub primary_status: String,
    pub secondary_status: String,
}

#[derive(Debug, Clone)]
pub struct LocalIdentitySummary {
    pub wayfair_id: String,
    pub verifying_key_b64: String,
    pub device_name: String,
}

pub fn ensure_local_identity_for_profile(profile: &str) -> Result<LocalIdentitySummary, String> {
    match load_identity(profile)? {
        Some(identity) => summary_from_identity(&identity),
        None => regenerate_local_identity_for_profile(profile),
    }
}

pub fn regenerate_local_identity_for_profile(
    profile: &str,
) -> Result<LocalIdentitySummary, String> {
    let mut csprng = OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let signing_key_b64 = base64::engine::general_purpose::STANDARD.encode(signing_key.to_bytes());

    let wayfair_id = uuid::Uuid::new_v4().to_string();
    let identity = StoredIdentity {
        wayfair_id,
        signing_key_b64,
        device_name: infer_device_name(),
        platform: "linux".to_string(),
    };

    persist_identity(profile, &identity)?;
    summary_from_identity(&identity)
}

pub fn delete_wayfair_id_for_profile(profile: &str) -> Result<(), String> {
    let identity_path = identity_file_path(profile);
    if identity_path.exists() {
        fs::remove_file(&identity_path).map_err(|err| {
            format!(
                "failed to delete identity file at {}: {err}",
                identity_path.display()
            )
        })?;
    }

    let cache_path = session_cache_file_path(profile);
    if cache_path.exists() {
        fs::remove_file(&cache_path).map_err(|err| {
            format!(
                "failed to delete session cache file at {}: {err}",
                cache_path.display()
            )
        })?;
    }

    Ok(())
}

pub fn save_relay_session_cache_for_profile(
    profile: &str,
    cache: &RelaySessionCache,
) -> Result<(), String> {
    let identity = ensure_stored_identity(profile)?;
    let cipher = cipher_from_identity(&identity)?;
    let serialized = serde_json::to_vec(cache)
        .map_err(|err| format!("failed to serialize session cache payload: {err}"))?;

    let mut nonce_bytes = [0u8; 12];
    use chacha20poly1305::aead::rand_core::RngCore;
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, serialized.as_ref())
        .map_err(|err| format!("failed to encrypt session cache payload: {err}"))?;

    let envelope = EncryptedEnvelope {
        nonce_b64: base64::engine::general_purpose::STANDARD.encode(nonce_bytes),
        ciphertext_b64: base64::engine::general_purpose::STANDARD.encode(ciphertext),
    };

    let serialized_envelope = serde_json::to_string_pretty(&envelope)
        .map_err(|err| format!("failed to serialize encrypted cache envelope: {err}"))?;

    let path = session_cache_file_path(profile);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create identity directory at {}: {err}",
                parent.display()
            )
        })?;
    }

    write_secure_file(&path, serialized_envelope.as_bytes())
}

pub fn load_relay_session_cache_for_profile(
    profile: &str,
) -> Result<Option<RelaySessionCache>, String> {
    let path = session_cache_file_path(profile);
    if !path.exists() {
        return Ok(None);
    }

    let identity = ensure_stored_identity(profile)?;
    let cipher = cipher_from_identity(&identity)?;

    let content = fs::read_to_string(&path).map_err(|err| {
        format!(
            "failed to read encrypted cache file at {}: {err}",
            path.display()
        )
    })?;

    let envelope: EncryptedEnvelope = serde_json::from_str(&content).map_err(|err| {
        format!(
            "failed to parse encrypted cache file at {}: {err}",
            path.display()
        )
    })?;

    let nonce_bytes = base64::engine::general_purpose::STANDARD
        .decode(envelope.nonce_b64)
        .map_err(|err| format!("failed to decode cache nonce: {err}"))?;
    if nonce_bytes.len() != 12 {
        return Err("cache nonce had invalid length".to_string());
    }
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(envelope.ciphertext_b64)
        .map_err(|err| format!("failed to decode encrypted cache payload: {err}"))?;

    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|err| format!("failed to decrypt session cache payload: {err}"))?;

    let cache: RelaySessionCache = serde_json::from_slice(&plaintext)
        .map_err(|err| format!("failed to parse session cache payload: {err}"))?;

    Ok(Some(cache))
}

fn persist_identity(profile: &str, identity: &StoredIdentity) -> Result<(), String> {
    let path = identity_file_path(profile);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create identity directory at {}: {err}",
                parent.display()
            )
        })?;
    }

    let serialized = serde_json::to_string_pretty(identity)
        .map_err(|err| format!("failed to serialize identity payload: {err}"))?;

    write_secure_file(&path, serialized.as_bytes())
}

fn load_identity(profile: &str) -> Result<Option<StoredIdentity>, String> {
    let path = identity_file_path(profile);
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)
        .map_err(|err| format!("failed to read identity file at {}: {err}", path.display()))?;

    let identity: StoredIdentity = serde_json::from_str(&content)
        .map_err(|err| format!("failed to parse identity file at {}: {err}", path.display()))?;

    Ok(Some(identity))
}

fn ensure_stored_identity(profile: &str) -> Result<StoredIdentity, String> {
    match load_identity(profile)? {
        Some(identity) => Ok(identity),
        None => {
            regenerate_local_identity_for_profile(profile)?;
            load_identity(profile)?.ok_or_else(|| "failed to load regenerated identity".to_string())
        }
    }
}

fn summary_from_identity(identity: &StoredIdentity) -> Result<LocalIdentitySummary, String> {
    let signing_bytes = base64::engine::general_purpose::STANDARD
        .decode(&identity.signing_key_b64)
        .map_err(|err| format!("failed to decode signing key: {err}"))?;

    let signing_arr: [u8; 32] = signing_bytes
        .try_into()
        .map_err(|_| "decoded signing key had invalid length".to_string())?;

    let verifying: VerifyingKey = SigningKey::from_bytes(&signing_arr).verifying_key();

    Ok(LocalIdentitySummary {
        wayfair_id: identity.wayfair_id.clone(),
        verifying_key_b64: base64::engine::general_purpose::STANDARD.encode(verifying.to_bytes()),
        device_name: identity.device_name.clone(),
    })
}

fn cipher_from_identity(identity: &StoredIdentity) -> Result<ChaCha20Poly1305, String> {
    let signing_seed = base64::engine::general_purpose::STANDARD
        .decode(&identity.signing_key_b64)
        .map_err(|err| format!("failed to decode signing key for cache cipher: {err}"))?;

    let mut hasher = Sha256::new();
    hasher.update(signing_seed);
    hasher.update(identity.wayfair_id.as_bytes());
    let hash = hasher.finalize();
    let key = Key::from_slice(&hash[..32]);
    Ok(ChaCha20Poly1305::new(key))
}

fn infer_device_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "linux-device".to_string())
}

fn write_secure_file(path: &Path, content: &[u8]) -> Result<(), String> {
    fs::write(path, content)
        .map_err(|err| format!("failed to write file at {}: {err}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions).map_err(|err| {
            format!(
                "failed to set secure permissions on file {}: {err}",
                path.display()
            )
        })?;
    }

    Ok(())
}

fn identity_file_path(profile: &str) -> PathBuf {
    identity_file_path_for(base_data_dir(), profile)
}

fn session_cache_file_path(profile: &str) -> PathBuf {
    session_cache_file_path_for(base_data_dir(), profile)
}

fn base_data_dir() -> PathBuf {
    if let Ok(xdg_data_home) = std::env::var("XDG_DATA_HOME") {
        if !xdg_data_home.trim().is_empty() {
            return PathBuf::from(xdg_data_home);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        return Path::new(&home).join(".local").join("share");
    }

    std::env::temp_dir()
}

fn identity_file_path_for(base_dir: PathBuf, profile: &str) -> PathBuf {
    let safe = sanitize_profile(profile);
    base_dir
        .join(APP_DIR_NAME)
        .join(format!("{IDENTITY_FILE_PREFIX}-{safe}.json"))
}

fn session_cache_file_path_for(base_dir: PathBuf, profile: &str) -> PathBuf {
    let safe = sanitize_profile(profile);
    base_dir
        .join(APP_DIR_NAME)
        .join(format!("{SESSION_CACHE_FILE_PREFIX}-{safe}.enc.json"))
}

fn sanitize_profile(profile: &str) -> String {
    let trimmed = profile.trim();
    if trimmed.is_empty() {
        return DEFAULT_PROFILE.to_string();
    }

    trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{identity_file_path_for, sanitize_profile, session_cache_file_path_for};
    use std::path::PathBuf;

    #[test]
    fn identity_file_path_uses_profile_suffix() {
        let base = PathBuf::from("/tmp/test-data");
        let path = identity_file_path_for(base, "alpha");
        assert_eq!(
            path,
            PathBuf::from("/tmp/test-data/aethos-linux/identity-alpha.json")
        );
    }

    #[test]
    fn session_cache_path_uses_profile_suffix() {
        let base = PathBuf::from("/tmp/test-data");
        let path = session_cache_file_path_for(base, "default");
        assert_eq!(
            path,
            PathBuf::from("/tmp/test-data/aethos-linux/session-cache-default.enc.json")
        );
    }

    #[test]
    fn sanitize_profile_normalizes_invalid_chars() {
        assert_eq!(sanitize_profile("team blue"), "team_blue");
    }
}
