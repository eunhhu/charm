use crate::providers::factory::ProviderAuth;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize, Serialize)]
struct CharmAuthFile {
    #[serde(default)]
    providers: BTreeMap<String, StoredProviderAuth>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct StoredProviderAuth {
    token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
}

impl From<ProviderAuth> for StoredProviderAuth {
    fn from(auth: ProviderAuth) -> Self {
        Self {
            token: auth.token,
            account_id: auth.account_id,
        }
    }
}

impl From<StoredProviderAuth> for ProviderAuth {
    fn from(auth: StoredProviderAuth) -> Self {
        Self {
            token: auth.token,
            account_id: auth.account_id,
        }
    }
}

pub fn charm_home_path() -> Option<PathBuf> {
    if let Some(path) = non_empty_env_path("CHARM_HOME") {
        return Some(path);
    }
    non_empty_env_path("HOME").map(|home| home.join(".charm"))
}

pub fn auth_file_path() -> Option<PathBuf> {
    Some(charm_home_path()?.join("auth.json"))
}

pub fn load_provider_auth(provider_id: &str) -> anyhow::Result<Option<ProviderAuth>> {
    let Some(path) = auth_file_path() else {
        return Ok(None);
    };
    load_provider_auth_from_path(&path, provider_id)
}

pub fn save_provider_auth(provider_id: &str, auth: ProviderAuth) -> anyhow::Result<PathBuf> {
    let path = auth_file_path().context("HOME or CHARM_HOME must be set to save provider auth")?;
    save_provider_auth_to_path(&path, provider_id, auth)?;
    Ok(path)
}

pub(crate) fn load_provider_auth_from_path(
    path: &Path,
    provider_id: &str,
) -> anyhow::Result<Option<ProviderAuth>> {
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let parsed = serde_json::from_str::<CharmAuthFile>(&body)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(parsed
        .providers
        .get(provider_id)
        .filter(|auth| !auth.token.trim().is_empty())
        .cloned()
        .map(ProviderAuth::from))
}

pub(crate) fn save_provider_auth_to_path(
    path: &Path,
    provider_id: &str,
    auth: ProviderAuth,
) -> anyhow::Result<()> {
    let mut parsed = if path.exists() {
        let body =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str::<CharmAuthFile>(&body)
            .with_context(|| format!("parse {}", path.display()))?
    } else {
        CharmAuthFile::default()
    };
    parsed
        .providers
        .insert(provider_id.to_string(), StoredProviderAuth::from(auth));

    let parent = path
        .parent()
        .context("provider auth path must have a parent directory")?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let body = serde_json::to_string_pretty(&parsed)?;
    write_secret_file(path, &body).with_context(|| format!("write {}", path.display()))
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    let value = std::env::var_os(key)?;
    let path = PathBuf::from(value);
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

#[cfg(unix)]
fn write_secret_file(path: &Path, body: &str) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(body.as_bytes())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret_file(path: &Path, body: &str) -> anyhow::Result<()> {
    std::fs::write(path, body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn saves_and_loads_provider_auth() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("auth.json");

        save_provider_auth_to_path(
            &path,
            "openrouter",
            ProviderAuth {
                token: "sk-or-test".to_string(),
                account_id: None,
            },
        )
        .expect("save auth");

        let loaded = load_provider_auth_from_path(&path, "openrouter")
            .expect("load auth")
            .expect("auth exists");
        assert_eq!(loaded.token, "sk-or-test");
        assert_eq!(loaded.account_id, None);
    }

    #[test]
    fn missing_provider_returns_none() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("auth.json");
        std::fs::write(&path, r#"{"providers":{}}"#).expect("write auth");

        let loaded = load_provider_auth_from_path(&path, "anthropic").expect("load auth");

        assert!(loaded.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn auth_file_is_written_user_read_write_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("auth.json");

        save_provider_auth_to_path(
            &path,
            "openai",
            ProviderAuth {
                token: "sk-test".to_string(),
                account_id: None,
            },
        )
        .expect("save auth");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
