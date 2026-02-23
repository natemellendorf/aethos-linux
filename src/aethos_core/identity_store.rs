use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const APP_DIR_NAME: &str = "aethos-linux";
const IDENTITY_FILE_NAME: &str = "identity.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredIdentity {
    wayfair_id: String,
}

pub fn load_wayfair_id() -> Result<Option<String>, String> {
    let path = identity_file_path();
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)
        .map_err(|err| format!("failed to read identity file at {}: {err}", path.display()))?;
    let identity: StoredIdentity = serde_json::from_str(&content)
        .map_err(|err| format!("failed to parse identity file at {}: {err}", path.display()))?;

    Ok(Some(identity.wayfair_id))
}

pub fn save_wayfair_id(wayfair_id: &str) -> Result<(), String> {
    let path = identity_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create identity directory at {}: {err}",
                parent.display()
            )
        })?;
    }

    let payload = StoredIdentity {
        wayfair_id: wayfair_id.to_string(),
    };

    let serialized = serde_json::to_string_pretty(&payload)
        .map_err(|err| format!("failed to serialize identity payload: {err}"))?;

    fs::write(&path, serialized)
        .map_err(|err| format!("failed to write identity file at {}: {err}", path.display()))
}

pub fn delete_wayfair_id() -> Result<(), String> {
    let path = identity_file_path();
    if !path.exists() {
        return Ok(());
    }

    fs::remove_file(&path).map_err(|err| {
        format!(
            "failed to delete identity file at {}: {err}",
            path.display()
        )
    })
}

fn identity_file_path() -> PathBuf {
    identity_file_path_for(base_data_dir())
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

#[cfg(test)]
mod tests {
    use super::identity_file_path_for;
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
}
