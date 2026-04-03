use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const APP_DIR_NAME: &str = "aethos-linux";
const IDENTITY_FILE_NAME: &str = "identity.json";
const SESSION_CACHE_FILE_NAME: &str = "session-cache.enc.json";
const CONTACT_ALIASES_FILE_NAME: &str = "contact-aliases.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredIdentity {
    #[serde(alias = "wayfair_id")]
    wayfarer_id: String,
    #[serde(default)]
    device_id: String,
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
    pub wayfarer_id: String,
    pub device_id: String,
    pub verifying_key_b64: String,
    pub device_name: String,
}

pub fn ensure_local_identity() -> Result<LocalIdentitySummary, String> {
    match load_identity()? {
        Some(identity) => {
            let reconciled = reconcile_identity(identity)?;
            Ok(summary_from_identity(&reconciled)?)
        }
        None => regenerate_local_identity(),
    }
}

pub fn regenerate_local_identity() -> Result<LocalIdentitySummary, String> {
    let mut csprng = OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let signing_key_b64 = base64::engine::general_purpose::STANDARD.encode(signing_key.to_bytes());
    let verifying = signing_key.verifying_key();
    let wayfarer_id = sha256_hex_lower(&verifying.to_bytes());
    let device_id = sha256_hex_lower(&verifying.to_bytes());

    let identity = StoredIdentity {
        wayfarer_id,
        device_id,
        signing_key_b64,
        device_name: infer_device_name(),
        platform: "linux".to_string(),
    };

    persist_identity(&identity)?;
    summary_from_identity(&identity)
}

pub fn load_local_signing_key_seed() -> Result<[u8; 32], String> {
    let identity = ensure_stored_identity()?;
    let signing_bytes = base64::engine::general_purpose::STANDARD
        .decode(&identity.signing_key_b64)
        .map_err(|err| format!("failed to decode signing key: {err}"))?;

    signing_bytes
        .try_into()
        .map_err(|_| "decoded signing key had invalid length".to_string())
}

pub fn delete_wayfarer_id() -> Result<(), String> {
    let identity_path = identity_file_path();
    if identity_path.exists() {
        fs::remove_file(&identity_path).map_err(|err| {
            format!(
                "failed to delete identity file at {}: {err}",
                identity_path.display()
            )
        })?;
    }

    let cache_path = session_cache_file_path();
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

#[allow(dead_code)]
pub fn delete_wayfair_id() -> Result<(), String> {
    delete_wayfarer_id()
}

pub fn save_relay_session_cache(cache: &RelaySessionCache) -> Result<(), String> {
    let identity = ensure_stored_identity()?;
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

    let path = session_cache_file_path();
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

pub fn load_relay_session_cache() -> Result<Option<RelaySessionCache>, String> {
    let path = session_cache_file_path();
    if !path.exists() {
        return Ok(None);
    }

    let identity = ensure_stored_identity()?;
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

pub fn load_contact_aliases() -> Result<BTreeMap<String, String>, String> {
    let path = contact_aliases_file_path();
    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    let content = fs::read_to_string(&path).map_err(|err| {
        format!(
            "failed to read contact aliases file at {}: {err}",
            path.display()
        )
    })?;

    let aliases: BTreeMap<String, String> = serde_json::from_str(&content).map_err(|err| {
        format!(
            "failed to parse contact aliases file at {}: {err}",
            path.display()
        )
    })?;

    Ok(aliases)
}

pub fn save_contact_aliases(aliases: &BTreeMap<String, String>) -> Result<(), String> {
    let path = contact_aliases_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create aliases directory at {}: {err}",
                parent.display()
            )
        })?;
    }

    let serialized = serde_json::to_string_pretty(aliases)
        .map_err(|err| format!("failed to serialize contact aliases payload: {err}"))?;

    write_secure_file(&path, serialized.as_bytes())
}

fn persist_identity(identity: &StoredIdentity) -> Result<(), String> {
    let path = identity_file_path();
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

fn load_identity() -> Result<Option<StoredIdentity>, String> {
    let path = identity_file_path();
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)
        .map_err(|err| format!("failed to read identity file at {}: {err}", path.display()))?;

    let identity: StoredIdentity = serde_json::from_str(&content)
        .map_err(|err| format!("failed to parse identity file at {}: {err}", path.display()))?;

    Ok(Some(identity))
}

fn ensure_stored_identity() -> Result<StoredIdentity, String> {
    match load_identity()? {
        Some(identity) => reconcile_identity(identity),
        None => {
            regenerate_local_identity()?;
            load_identity()?.ok_or_else(|| "failed to load regenerated identity".to_string())
        }
    }
}

fn summary_from_identity(identity: &StoredIdentity) -> Result<LocalIdentitySummary, String> {
    let verifying = decode_verifying_key_from_identity(identity)?;

    Ok(LocalIdentitySummary {
        wayfarer_id: identity.wayfarer_id.clone(),
        device_id: identity.device_id.clone(),
        verifying_key_b64: base64::engine::general_purpose::STANDARD.encode(verifying.to_bytes()),
        device_name: identity.device_name.clone(),
    })
}

fn reconcile_identity(mut identity: StoredIdentity) -> Result<StoredIdentity, String> {
    let verifying = decode_verifying_key_from_identity(&identity)?;
    let expected_wayfarer_id = sha256_hex_lower(&verifying.to_bytes());
    let expected_device_id = sha256_hex_lower(&verifying.to_bytes());

    let needs_migration = identity.wayfarer_id != expected_wayfarer_id
        || identity.device_id != expected_device_id
        || identity.platform != "linux";

    if needs_migration {
        identity.wayfarer_id = expected_wayfarer_id;
        identity.device_id = expected_device_id;
        identity.platform = "linux".to_string();
        persist_identity(&identity)?;
    }

    Ok(identity)
}

fn decode_verifying_key_from_identity(identity: &StoredIdentity) -> Result<VerifyingKey, String> {
    let signing_bytes = base64::engine::general_purpose::STANDARD
        .decode(&identity.signing_key_b64)
        .map_err(|err| format!("failed to decode signing key: {err}"))?;

    let signing_arr: [u8; 32] = signing_bytes
        .try_into()
        .map_err(|_| "decoded signing key had invalid length".to_string())?;

    Ok(SigningKey::from_bytes(&signing_arr).verifying_key())
}

fn sha256_hex_lower(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn cipher_from_identity(identity: &StoredIdentity) -> Result<ChaCha20Poly1305, String> {
    let signing_seed = base64::engine::general_purpose::STANDARD
        .decode(&identity.signing_key_b64)
        .map_err(|err| format!("failed to decode signing key for cache cipher: {err}"))?;

    let mut hasher = Sha256::new();
    hasher.update(signing_seed);
    hasher.update(identity.wayfarer_id.as_bytes());
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

fn identity_file_path() -> PathBuf {
    identity_file_path_for(base_data_dir())
}

fn session_cache_file_path() -> PathBuf {
    session_cache_file_path_for(base_data_dir())
}

fn contact_aliases_file_path() -> PathBuf {
    contact_aliases_file_path_for(base_data_dir())
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

fn identity_file_path_for(base_dir: PathBuf) -> PathBuf {
    base_dir.join(APP_DIR_NAME).join(IDENTITY_FILE_NAME)
}

fn session_cache_file_path_for(base_dir: PathBuf) -> PathBuf {
    base_dir.join(APP_DIR_NAME).join(SESSION_CACHE_FILE_NAME)
}

fn contact_aliases_file_path_for(base_dir: PathBuf) -> PathBuf {
    base_dir.join(APP_DIR_NAME).join(CONTACT_ALIASES_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::{
        cipher_from_identity, contact_aliases_file_path_for, decode_verifying_key_from_identity,
        identity_file_path_for, session_cache_file_path_for, sha256_hex_lower, StoredIdentity,
    };
    use base64::Engine;
    use chacha20poly1305::aead::Aead;
    use chacha20poly1305::{ChaCha20Poly1305, Nonce};
    use ed25519_dalek::SigningKey;
    use std::path::PathBuf;

    #[test]
    fn identity_file_path_uses_app_subdir() {
        let base = PathBuf::from("/tmp/test-data");
        let path = identity_file_path_for(base);
        assert_eq!(
            path,
            PathBuf::from("/tmp/test-data/aethos-linux/identity.json")
        );
    }

    #[test]
    fn session_cache_path_uses_app_subdir() {
        let base = PathBuf::from("/tmp/test-data");
        let path = session_cache_file_path_for(base);
        assert_eq!(
            path,
            PathBuf::from("/tmp/test-data/aethos-linux/session-cache.enc.json")
        );
    }

    #[test]
    fn contact_aliases_path_uses_app_subdir() {
        let base = PathBuf::from("/tmp/test-data");
        let path = contact_aliases_file_path_for(base);
        assert_eq!(
            path,
            PathBuf::from("/tmp/test-data/aethos-linux/contact-aliases.json")
        );
    }

    #[test]
    fn sha256_hex_encoding_is_lowercase_and_fixed_width() {
        let digest = sha256_hex_lower(b"abc");
        assert_eq!(digest.len(), 64);
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn fixture_identity() -> StoredIdentity {
        let seed = [7u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let verifying = signing_key.verifying_key();
        let digest = sha256_hex_lower(&verifying.to_bytes());
        StoredIdentity {
            wayfarer_id: digest.clone(),
            device_id: digest,
            signing_key_b64: base64::engine::general_purpose::STANDARD.encode(seed),
            device_name: "fixture-device".to_string(),
            platform: "linux".to_string(),
        }
    }

    #[test]
    fn decode_verifying_key_rejects_wrong_seed_length() {
        let mut identity = fixture_identity();
        identity.signing_key_b64 = base64::engine::general_purpose::STANDARD.encode([1u8; 16]);
        let err = decode_verifying_key_from_identity(&identity).unwrap_err();
        assert!(err.contains("invalid length"));
    }

    #[test]
    fn cipher_derivation_is_stable_for_same_identity() {
        let identity = fixture_identity();
        let cipher_a = cipher_from_identity(&identity).unwrap();
        let cipher_b = cipher_from_identity(&identity).unwrap();

        let nonce = Nonce::from_slice(&[0u8; 12]);
        let plaintext = b"relay-session-cache";
        let encrypted = cipher_a.encrypt(nonce, plaintext.as_slice()).unwrap();
        let decrypted = cipher_b.decrypt(nonce, encrypted.as_slice()).unwrap();
        assert_eq!(decrypted, plaintext);

        let _type_check: &ChaCha20Poly1305 = &cipher_a;
    }
}
