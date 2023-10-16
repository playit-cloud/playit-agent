use std::time::Duration;

use clap::ArgMatches;
use playit_agent_core::api::{
    api::*,
    PlayitApi,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::{claim_exchange, claim_generate, ui::UI, CliError, API_BASE};

pub struct PlayitSecret {
    secret: RwLock<Option<String>>,
    path: Option<String>,
    allow_path_read: bool,
    wait_for_path: bool,
}

impl PlayitSecret {
    pub async fn create_api(&self) -> Result<PlayitApi, CliError> {
        let secret: String = self.get().await?;
        Ok(PlayitApi::create(API_BASE.to_string(), Some(secret)))
    }

    pub async fn with_default_path(&mut self) -> &mut Self {
        if self.path.is_some() {
            return self;
        }

        let config_path = dirs::config_local_dir();

        if config_path.is_none() || tokio::fs::try_exists("playit.toml").await.unwrap_or(false) {
            self.path = Some("playit.toml".to_string());
            return self;
        }

        /* old versions for linux used /etc/playit/playit.toml */
        #[cfg(target_os = "linux")] {
            let old_path = "/etc/playit/playit.toml";
            if tokio::fs::try_exists(&old_path).await.unwrap_or(false) {
                self.path = Some(old_path.to_string());
                return self;
            }
        }

        let config_root = config_path.as_ref().unwrap().to_string_lossy();
        let config_folder = format!("{}/playit_gg", config_root);
        if let Err(error) = tokio::fs::create_dir_all(&config_folder).await {
            tracing::error!(?error, "failed to create configuration folder");
            self.path = Some("playit.toml".to_string());
            return self;
        }

        self.path = Some(format!("{}/playit.toml", config_folder));
        self
    }

    pub async fn ensure_valid(&mut self, ui: &mut UI) -> Result<&mut Self, CliError> {
        let api = match self.create_api().await {
            Ok(v) => v,
            Err(_) => {
                {
                    let mut secret = self.secret.write().await;
                    let _ = secret.take();
                }
                return Ok(self);
            }
        };

        ui.write_screen("checking if secret key is valid");
        tokio::time::sleep(Duration::from_secs(1)).await;

        loop {
            match api.agents_rundata().await {
                Ok(data) => {
                    ui.write_screen(format!(
                        "secret key valid, agent has {} tunnels",
                        data.tunnels.len()
                    ));
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    break;
                }
                Err(ApiErrorNoFail::ClientError(error)) => {
                    ui.write_error("Failed to load data from api\nretrying in 3 seconds", error);
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                Err(ApiErrorNoFail::ApiError(ApiResponseError::Auth(AuthError::InvalidAgentKey))) => {
                    if !self.path.is_some() {
                        return Err(CliError::InvalidSecret);
                    }

                    let reset =
                        ui.yn_question("Invalid secret, do you want to reset", Some(true))?;

                    if reset {
                        self.allow_path_read = false;

                        let mut locked = self.secret.write().await;
                        let _ = locked.take();
                        break;
                    }
                }
                Err(ApiErrorNoFail::ApiError(error)) => {
                    ui.write_error("unexpected error checking if secret is valid", &error);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    return Err(CliError::ApiError(error));
                }
            }
        }

        Ok(self)
    }

    pub async fn get_or_setup(&mut self, ui: &mut UI) -> Result<String, CliError> {
        loop {
            match self.get().await {
                Ok(secret) => return Ok(secret),
                Err(CliError::SecretFileLoadError) if self.wait_for_path => {
                    tracing::info!(path = ?self.path, "waiting for secret to be populated (run `playit setup`)");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                _ => break,
            }
        }

        if self.path.is_none() {
            return Err(CliError::SecretFilePathMissing);
        }

        let claim_code = claim_generate();
        let secret = claim_exchange(ui, &claim_code, AgentType::Assignable, 0).await?;

        {
            let mut lock = self.secret.write().await;
            lock.replace(secret.clone());
        }

        self.write_secret(ui, secret.clone()).await?;
        Ok(secret)
    }

    async fn write_secret(&mut self, ui: &mut UI, secret: String) -> Result<(), CliError> {
        let path = self
            .path
            .as_ref()
            .ok_or(CliError::SecretFilePathMissing)?
            .trim();

        let content = if path.ends_with(".toml") {
            toml::to_string(&OldConfig { secret_key: secret }).unwrap()
        } else {
            secret
        };

        if let Err(error) = tokio::fs::write(path, &content).await {
            ui.write_error(format!("failed to save secret, path: {}", path), &error);
            tokio::time::sleep(Duration::from_secs(5)).await;
            return Err(CliError::SecretFileWriteError(error));
        }

        self.allow_path_read = true;
        Ok(())
    }

    pub async fn get(&self) -> Result<String, CliError> {
        {
            let lock = self.secret.read().await;
            if let Some(value) = &*lock {
                let trimmed = value.trim();
                if hex::decode(trimmed).is_err() {
                    return Err(CliError::MalformedSecret);
                }
                return Ok(trimmed.to_string());
            }
        }

        if !self.allow_path_read {
            return Err(CliError::MissingSecret);
        }

        let file_path = self.path.as_ref().ok_or(CliError::MissingSecret)?;
        tracing::info!(%file_path, "loading secret");

        let mut lock = self.secret.write().await;

        let content = tokio::fs::read_to_string(file_path)
            .await
            .map_err(|_| CliError::SecretFileLoadError)?;
        let trimmed = content.trim();

        if hex::decode(trimmed).is_err() {
            let config =
                toml::from_str::<OldConfig>(&content).map_err(|_| CliError::MalformedSecret)?;
            let trimmed = config.secret_key.trim();

            hex::decode(trimmed).map_err(|_| CliError::MalformedSecret)?;

            lock.replace(trimmed.to_string());
            Ok(trimmed.to_string())
        } else {
            lock.replace(trimmed.to_string());
            Ok(trimmed.to_string())
        }
    }

    pub async fn from_args(matches: &ArgMatches) -> Self {
        let mut secret = matches.get_one::<String>("secret").cloned();
        let mut path = matches.get_one::<String>("secret_path").cloned();

        if secret.is_none() && path.is_none() {
            if let Some(secret_env) = option_env!("PLAYIT_SECRET") {
                secret.replace(secret_env.to_string());
            }
        }

        if path.is_none() {
            if let Some(path_env) = option_env!("PLAYIT_SECRET_PATH") {
                path.replace(path_env.to_string());
            }
        }

        PlayitSecret {
            secret: RwLock::new(secret),
            path,
            allow_path_read: true,
            wait_for_path: matches.get_flag("secret_wait"),
        }
    }

    #[cfg(target_os = "linux")]
    pub fn linux_service() -> Self {
        PlayitSecret {
            secret: RwLock::new(None),
            path: Some("/etc/playit/playit.toml".to_string()),
            allow_path_read: true,
            wait_for_path: false,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct OldConfig {
    secret_key: String,
}
