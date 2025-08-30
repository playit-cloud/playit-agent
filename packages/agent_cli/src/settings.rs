use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::cli::Cli;

#[derive(Serialize, Deserialize, Default)]
pub struct PlayitCliSettings {
    #[serde(default)]
    pub secret_key: Option<String>,
    #[serde(default)]
    pub secret_key_path: Option<String>,
    #[serde(default)]
    pub override_variant_id: Option<Uuid>,
    #[serde(default)]
    pub override_variant_version: Option<String>,
    #[serde(default)]
    pub override_variant_path: Option<String>,
}

impl PlayitCliSettings {
    pub async fn load(path: Option<String>) -> Result<(Self, String), LoadSettingsError> {
        let path = match path {
            Some(v) => v,
            None => Self::get_default_path().await,
        };

        let settings = filetype_load(&path).await?;
        Ok((settings, path))
    }

    pub fn and_to_missing(&self, cli: &mut Cli) {
        if cli.secret_key.is_none() && cli.secret_key_path.is_none() {
            cli.secret_key = self.secret_key.clone();
            cli.secret_key_path = self.secret_key_path.clone();
        } else if self.secret_key.is_some() || self.secret_key_path.is_some() {
            tracing::warn!("using secret key data from cli args not settings file");
        }

        if cli.override_variant_id.is_none() && cli.override_variant_path.is_none() {
            cli.override_variant_id = self.override_variant_id;
            cli.override_variant_version = self.override_variant_version.clone();
            cli.override_variant_path = self.override_variant_path.clone();
        } else if self.override_variant_id.is_some() || self.override_variant_version.is_some() {
            tracing::warn!("using override variant data from cli args not settings file");
        }
    }

    pub async fn get_default_path() -> String {
        let config_path = dirs::config_local_dir();

        if tokio::fs::try_exists("playit.toml").await.unwrap_or(false) {
            return "playit.toml".to_string();
        }

        /* old versions for linux used /etc/playit/playit.toml */
        #[cfg(target_os = "linux")]
        {
            let old_path = "/etc/playit/playit.toml";
            if tokio::fs::try_exists(&old_path).await.unwrap_or(false) {
                return old_path.to_string();
            }
        }

        let Some(config_path) = config_path else {
            return "playit.toml".to_string();
        };

        let config_root = config_path.to_string_lossy();
        let config_folder = format!("{}/playit_gg", config_root);
        if let Err(error) = tokio::fs::create_dir_all(&config_folder).await {
            tracing::error!(?error, "failed to create configuration folder");
            return "playit.toml".to_string();
        }

        return format!("{}/playit.toml", config_folder);
    }
}

async fn filetype_load<T: DeserializeOwned>(path: &str) -> Result<T, LoadSettingsError> {
    let data = tokio::fs::read_to_string(path)
        .await
        .map_err(LoadSettingsError::IoError)?;

    if path.ends_with(".toml") {
        return toml::from_str(&data).map_err(|_| LoadSettingsError::ParseError);
    }

    if path.ends_with(".yaml") || path.ends_with(".yml") {
        return serde_yaml::from_str(&data).map_err(|_| LoadSettingsError::ParseError);
    }

    return serde_json::from_str(&data).map_err(|_| {
        if path.ends_with(".json") {
            LoadSettingsError::ParseError
        } else {
            LoadSettingsError::UnknownFiletype(path.to_string())
        }
    });
}

#[derive(Debug)]
pub enum LoadSettingsError {
    IoError(std::io::Error),
    ParseError,
    UnknownFiletype(String),
}
