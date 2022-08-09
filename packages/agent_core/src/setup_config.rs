use std::sync::Arc;
use std::time::Duration;
use rand::Rng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;
use playit_agent_common::agent_config::{AgentConfigBuilder, DEFAULT_API};
use playit_agent_common::api::AgentAccountStatus;
use super::api_client::{ApiClient, ApiError};

pub async fn prepare_config(config_path: &str, prepare_status: &RwLock<AgentConfigStatus>) -> Result<AgentConfigBuilder, std::io::Error> {
    *prepare_status.write().await = AgentConfigStatus::ReadingConfigFile;

    let config = match load_or_create_config_file(config_path).await {
        Ok(Some(config)) => {
            if config.valid_secret_key() {
                let api = ApiClient::new(config.get_api_url(), Some(config.secret_key.clone()));

                *prepare_status.write().await = AgentConfigStatus::LoadingAccountStatus;

                let status = loop {
                    match api.get_agent_account_status().await {
                        Ok(v) => break v,
                        Err(error) => {
                            tracing::error!(?error, "failed to load account status, retrying in 5s");
                            *prepare_status.write().await = AgentConfigStatus::ErrorLoadingAccountStatus;
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        }
                    }
                };

                match status {
                    /* continue to account setup logic */
                    AgentAccountStatus::NoAccount { .. } => {}

                    /* use config */
                    AgentAccountStatus::VerifiedAccount { .. } => {
                        *prepare_status.write().await = AgentConfigStatus::AccountVerified;
                        return Ok(config);
                    }
                    AgentAccountStatus::UnverifiedAccount { account_id } => {
                        let verify_url = format!("https://playit.gg/login/verify-account/{}", account_id);
                        tracing::info!(%verify_url, "generated verify account url");

                        *prepare_status.write().await = AgentConfigStatus::PleaseVerifyAccount { url: Arc::new(verify_url) };
                        return Ok(config);
                    }
                    AgentAccountStatus::GuestAccount { web_session_key, .. } => {
                        let guest_login_url = format!("https://playit.gg/login/guest-account/{}", web_session_key);
                        tracing::info!(%guest_login_url, "generated guest login url");

                        *prepare_status.write().await = AgentConfigStatus::PleaseCreateAccount { url: Arc::new(guest_login_url) };
                        return Ok(config);
                    }
                    AgentAccountStatus::UserNotice { notice_url, message, important, prevent_usage } => {
                        let url = Arc::new(notice_url);

                        *prepare_status.write().await = AgentConfigStatus::UserNotice {
                            message: Arc::new(message),
                            url: url.clone(),
                            important,
                        };

                        if !prevent_usage {
                            tokio::time::sleep(Duration::from_secs(10)).await;
                            return Ok(config);
                        }
                    }
                }
            }

            Some(config)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::error!(?error, "failed to load / create config file");
            *prepare_status.write().await = AgentConfigStatus::ReadingConfigFile;
            return Err(error);
        }
    };

    tracing::info!("generating claim key to setup playit program");

    let mut buffer = [0u8; 5];
    rand::thread_rng().fill(&mut buffer);
    let claim_key = hex::encode(&buffer);

    let claim_url = format!("https://playit.gg/claim/{}", claim_key);
    tracing::info!(%claim_url, "generated claim url");

    *prepare_status.write().await = AgentConfigStatus::PleaseActiveProgram { url: Arc::new(claim_url) };

    let api_url = config.as_ref().map(|v| v.get_api_url()).unwrap_or_else(|| DEFAULT_API.to_string());
    let api = ApiClient::new(api_url, None);

    /*
     * Keep polling api till secret key has been generated. For the secret
     * to be generated the user must interact with the website using the
     * claim URL.
     */
    let secret_key = loop {
        match api.try_exchange_claim_for_secret(&claim_key).await {
            Ok(Some(secret_key)) => break secret_key,
            Ok(None) => {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(ApiError::HttpError(401, message)) => {
                tracing::info!(%message, "still waiting for claim");
                tokio::time::sleep(Duration::from_secs(8)).await;
            }
            Err(error) => {
                tracing::error!(?error, "failed to exchange claim key for secret key");
                tokio::time::sleep(Duration::from_secs(8)).await;
            }
        }
    };

    tracing::info!("agent setup, got secret key");
    *prepare_status.write().await = AgentConfigStatus::ProgramActivated;

    let config = match config {
        Some(mut config) => {
            config.secret_key = secret_key;
            config
        }
        None => {
            AgentConfigBuilder {
                last_update: None,
                api_url: None,
                ping_targets: None,
                ping_target_addresses: None,
                control_address: None,
                refresh_from_api: Some(true),
                api_refresh_rate: None,
                secret_key,
                mappings: vec![],
            }
        }
    };

    if let Err(error) = tokio::fs::write(config_path, toml::to_string_pretty(&config).unwrap()).await {
        tracing::error!(?error, config_path, "failed to write config file");
    } else {
        tracing::info!(config_path, "config file updated");
    }

    Ok(config)
}


#[derive(Debug, Clone)]
pub enum AgentConfigStatus {
    Staring,
    ReadingConfigFile,
    FileReadFailed,
    LoadingAccountStatus,
    ErrorLoadingAccountStatus,
    AccountVerified,
    PleaseVerifyAccount {
        url: Arc<String>,
    },
    PleaseCreateAccount {
        url: Arc<String>,
    },
    PleaseActiveProgram {
        url: Arc<String>,
    },
    UserNotice {
        message: Arc<String>,
        url: Arc<String>,
        important: bool,
    },
    ProgramActivated,
}

impl Default for AgentConfigStatus {
    fn default() -> Self {
        AgentConfigStatus::Staring
    }
}

async fn load_or_create_config_file(config_path: &str) -> std::io::Result<Option<AgentConfigBuilder>> {
    match tokio::fs::File::open(config_path).await {
        Ok(mut file) => {
            let mut data = Vec::new();
            file.read_to_end(&mut data).await?;

            let config = match toml::from_slice::<AgentConfigBuilder>(&data) {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, config_path, "failed to parse agent config");
                    return Ok(None);
                }
            };

            Ok(Some(config))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut file = tokio::fs::File::create(config_path).await?;

            file.write_all(
                toml::to_string(&AgentConfigBuilder {
                    last_update: None,
                    api_url: None,
                    ping_targets: None,
                    ping_target_addresses: None,
                    control_address: None,
                    refresh_from_api: Some(true),
                    api_refresh_rate: None,
                    secret_key: "put-secret-here".to_string(),
                    mappings: vec![],
                }).unwrap().as_bytes(),
            ).await?;

            Ok(None)
        }
        Err(e) => Err(e),
    }
}
