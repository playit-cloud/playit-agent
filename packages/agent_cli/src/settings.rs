use std::str::FromStr;

use playit_api_client::api::AgentVersion;
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
    pub fn from_cli(cli: &Cli) -> Self {
        PlayitCliSettings {
            secret_key: cli.secret_key.clone(),
            secret_key_path: cli.secret_key_path.clone(),
            override_variant_id: cli.override_variant_id.clone(),
            override_variant_version: cli.override_variant_version.clone(),
            override_variant_path: cli.override_variant_path.clone(),
        }
    }

    pub fn load_from_env() -> Self {
        PlayitCliSettings {
            secret_key: dotenv::var("PLAYIT_SECRET_KEY").ok(),
            secret_key_path: dotenv::var("PLAYIT_SECRET_KEY_PATH").ok(),
            override_variant_id: dotenv::var("PLAYIT_VARIANT_ID")
                .ok()
                .and_then(|v| Uuid::from_str(&v).ok()),
            override_variant_version: dotenv::var("PLAYIT_VARIANT_VERSION").ok(),
            override_variant_path: dotenv::var("PLAYIT_VARIANT_PATH").ok(),
        }
    }

    pub async fn load_from_file(path: Option<String>) -> Result<(Option<Self>, String), LoadSettingsError> {
        let path = match path {
            Some(v) => v,
            None => Self::get_default_path().await,
        };

        if !tokio::fs::try_exists(&path).await.map_err(LoadSettingsError::IoError)? {
            return Ok((None, path));
        }

        let settings = filetype_load(&path).await?;
        Ok((Some(settings), path))
    }

    pub fn add_to_missing(&self, target: &mut Self) {
        if target.secret_key.is_none() && target.secret_key_path.is_none() {
            target.secret_key = self.secret_key.clone();
            target.secret_key_path = self.secret_key_path.clone();
        } else if self.secret_key.is_some() || self.secret_key_path.is_some() {
            tracing::warn!("using secret key data from cli args not settings file");
        }

        if target.override_variant_id.is_none() && target.override_variant_path.is_none() {
            target.override_variant_id = self.override_variant_id;
            target.override_variant_version = self.override_variant_version.clone();
            target.override_variant_path = self.override_variant_path.clone();
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

#[derive(Debug)]
pub struct CliAgentVersionDetails {
    pub agent_version: AgentVersion,
}

impl CliAgentVersionDetails {
    pub fn extract(settings: &PlayitCliSettings) -> Result<Option<Self>, CliError> {
        if let Some(variant) = settings.override_variant_id {
            let Some(semvar) = &settings.override_variant_version else {
                return Err(CliError::ConflictingArgs {
                    first: "override_variant_id".to_string(),
                    second: "override_variant_version".to_string(),
                    reason: "variant_version must be supplied if overriding variant id".to_string(),
                });
            };

            if settings.override_variant_path.is_some() {
                return Err(CliError::ConflictingArgs {
                    first: "override_variant_id".to_string(),
                    second: "override_variant_path".to_string(),
                    reason: "both cannot be defined".to_string(),
                });
            }

            let mut version_parts = semvar.split(".");
            let a = version_parts.next().and_then(|s| u32::from_str(s).ok());
            let b = version_parts.next().and_then(|s| u32::from_str(s).ok());
            let c = version_parts.next().and_then(|s| u32::from_str(s).ok());

            let agent_version = match (a, b, c) {
                (Some(a), Some(b), Some(c)) => AgentVersion {
                    variant_id: variant,
                    version_major: a,
                    version_minor: b,
                    version_patch: c,
                },
                _ => {
                    return Err(CliError::InvalidArgFormat {
                        resource_name: "override_variant_version".to_string(),
                        expected_format: "<major:u32>.<minor:u32>.<patch:u32>".to_string(),
                    });
                }
            };

            return Ok(Some(CliAgentVersionDetails { agent_version }));
        }

        if let Some(path) = &settings.override_variant_path {
            let json = std::fs::read_to_string(path).map_err(|io_error| {
                CliError::FailedToLoadFileContent {
                    resource_name: "override_variant_path".to_string(),
                    file_path: path.to_string(),
                    io_error,
                }
            })?;

            let agent_version = serde_json::from_str::<AgentVersion>(&json).map_err(|_| {
                CliError::InvalidArgFormat {
                    resource_name: "override_variant_path".to_string(),
                    expected_format: "<agent_version:json>".to_string(),
                }
            })?;

            return Ok(Some(CliAgentVersionDetails { agent_version }));
        }

        Ok(None)
    }
}

#[derive(Debug)]
pub struct CliSecretDetails {
    pub secret_key: String,
}

impl CliSecretDetails {
    pub async fn extract(settings: &PlayitCliSettings) -> Result<Option<Self>, CliError> {
        if let Some(secret_key) = &settings.secret_key {
            if settings.secret_key_path.is_some() {
                return Err(CliError::ConflictingArgs {
                    first: "secret_key".to_string(),
                    second: "secret_key_path".to_string(),
                    reason: "both cannot be defined".to_string(),
                });
            }

            return Ok(Some(Self {
                secret_key: secret_key.clone(),
            }));
        }

        if let Some(path) = &settings.secret_key_path {
            return match tokio::fs::read_to_string(path.as_str()).await {
                Err(error) => Err(CliError::FailedToLoadFileContent {
                    resource_name: "secret_key_path".to_string(),
                    file_path: path.to_string(),
                    io_error: error,
                }),
                Ok(content) => Ok(Some(Self {
                    secret_key: content,
                })),
            };
        }

        Ok(None)
    }
}

#[derive(Debug)]
pub enum CliError {
    ConflictingArgs {
        first: String,
        second: String,
        reason: String,
    },
    FailedToLoadFileContent {
        resource_name: String,
        file_path: String,
        io_error: std::io::Error,
    },
    InvalidArgFormat {
        resource_name: String,
        expected_format: String,
    },
}

impl CliError {
    pub fn print_and_exit(self) -> ! {
        match self {
            Self::ConflictingArgs {
                first,
                second,
                reason,
            } => {
                eprintln!("Conflicting cli arguments / settings");
                eprintln!("{first} & {second}\n{reason}");
            }
            Self::FailedToLoadFileContent {
                resource_name,
                file_path,
                io_error,
            } => {
                eprintln!("Failed to load file {file_path:?}");
                eprintln!("Loading file for {resource_name}, error:\n{io_error:?}");
            }
            Self::InvalidArgFormat {
                resource_name,
                expected_format,
            } => {
                eprintln!("Invalid cli argument / setting");
                eprintln!("{resource_name} should be in format: {expected_format}");
            }
        }
        std::process::exit(1)
    }
}
