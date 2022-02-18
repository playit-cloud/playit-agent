use std::net::{IpAddr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

use agent_common::agent_config::{AgentConfig, DEFAULT_API};
use agent_common::api::AgentAccountStatus;
use agent_common::Proto;
use crate::api_client::{ApiClient, ApiError};
use crate::events::{PlayitEventDetails, PlayitEvents};
use crate::now_milli;

#[derive(Clone)]
pub struct ManagedAgentConfig {
    pub version: Arc<AtomicUsize>,
    pub config: Arc<RwLock<AgentConfig>>,
    pub status: Arc<RwLock<AgentConfigStatus>>,
    pub events: PlayitEvents,
}

impl ManagedAgentConfig {
    pub fn new(events: PlayitEvents) -> Self {
        ManagedAgentConfig {
            version: Arc::new(AtomicUsize::new(0)),
            config: Arc::new(RwLock::new(AgentConfig {
                last_update: None,
                api_url: None,
                refresh_from_api: false,
                secret_key: "".to_string(),
                mappings: vec![]
            })),
            status: Arc::new(Default::default()),
            events,
        }
    }

    pub async fn into_local_lookup(
        self,
        addr: SocketAddrV4,
        proto: Proto,
    ) -> Option<(Option<IpAddr>, SocketAddr)> {
        self.with_config(|v| v.find_local_addr(addr, proto)).await
    }

    pub async fn prepare_config(&self) -> Result<(), std::io::Error> {
        let mut guard = self.config.write().await;
        *guard = prepare_config(&self.status).await?;
        Ok(())
    }

    pub async fn load_latest(&self) -> Result<bool, ApiError> {
        let api = {
            let c = self.config.read().await;
            ApiClient::new(c.get_api_url(), Some(c.secret_key.clone()))
        };

        let mut api_config = match api.get_agent_config().await {
            Ok(config) => config,
            Err(error) => {
                tracing::error!(?error, "failed to load config from API");
                return Err(error);
            }
        };

        let config_updated = {
            let current = self.config.read().await;

            if let Some(ref api_url) = current.api_url {
                api_config.api_url = Some(api_url.clone());
            }
            api_config.last_update = current.last_update;

            !api_config.eq(&current)
        };

        if config_updated {
            tracing::info!("updating config");
            api_config.last_update = Some(now_milli());

            std::mem::replace(&mut *self.config.write().await, api_config.clone());
            self.version.fetch_add(1, Ordering::SeqCst);
            self.events.add_event(PlayitEventDetails::AgentConfigUpdated).await;

            if let Err(error) = tokio::fs::write("playit.toml", toml::to_string_pretty(&api_config).unwrap()).await {
                tracing::error!(?error, "failed to write updated configuration to playit.toml");
            }
        }

        Ok(config_updated)
    }

    pub fn get_status_blocking(&self) -> AgentConfigStatus {
        let status = futures::executor::block_on(self.status.read());
        (&*status).clone()
    }

    pub fn with_config_blocking<T, F: Fn(&AgentConfig) -> T>(&self, handle: F) -> T {
        futures::executor::block_on(self.with_config(handle))
    }

    pub async fn with_config<T, F: Fn(&AgentConfig) -> T>(&self, handle: F) -> T {
        let config = self.config.read().await;
        handle(&config)
    }
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
    ProgramActivated,
}

impl Default for AgentConfigStatus {
    fn default() -> Self {
        AgentConfigStatus::Staring
    }
}

pub async fn prepare_config(prepare_status: &RwLock<AgentConfigStatus>) -> Result<AgentConfig, std::io::Error> {
    *prepare_status.write().await = AgentConfigStatus::ReadingConfigFile;

    let config = match load_or_create().await {
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
                        let verify_url = format!("https://new.playit.gg/login/verify-account/{}", account_id);
                        tracing::info!(%verify_url, "generated verify account url");

                        if let Err(error) = webbrowser::open(&verify_url) {
                            tracing::error!(?error, url = %verify_url, "failed to open verify URL in web browser");
                        }
                        *prepare_status.write().await = AgentConfigStatus::PleaseVerifyAccount { url: Arc::new(verify_url) };
                        return Ok(config);
                    }
                    AgentAccountStatus::GuestAccount { web_session_key, .. } => {
                        let guest_login_url = format!("https://new.playit.gg/login/guest-account/{}", web_session_key);
                        tracing::info!(%guest_login_url, "generated guest login url");

                        if let Err(error) = webbrowser::open(&guest_login_url) {
                            tracing::error!(?error, url = %guest_login_url, "failed to open guest login URL in web browser");
                        }
                        *prepare_status.write().await = AgentConfigStatus::PleaseCreateAccount { url: Arc::new(guest_login_url) };
                        return Ok(config);
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

    let mut buffer = [0u8; 32];
    SystemRandom::new().fill(&mut buffer).unwrap();
    let claim_key = hex::encode(&buffer);

    let claim_url = format!("https://new.playit.gg/claim/{}", claim_key);
    tracing::info!(%claim_url, "generated claim url");

    if let Err(error) = webbrowser::open(&claim_url) {
        tracing::error!(?error, "failed to open claim URL in web browser");
    }

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
            AgentConfig {
                last_update: None,
                api_url: None,
                refresh_from_api: true,
                secret_key,
                mappings: vec![],
            }
        }
    };

    if let Err(error) = tokio::fs::write("playit.toml", toml::to_string_pretty(&config).unwrap()).await {
        tracing::error!(?error, "failed to write playit.toml config");
    } else {
        tracing::info!("playit.toml updated");
    }

    Ok(config)
}

async fn load_or_create() -> std::io::Result<Option<AgentConfig>> {
    match tokio::fs::File::open("./playit.toml").await {
        Ok(mut file) => {
            let mut data = Vec::new();
            file.read_to_end(&mut data).await?;

            let config = match toml::from_slice::<AgentConfig>(&data) {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "failed to parse ./playit.toml");
                    return Ok(None);
                }
            };

            Ok(Some(config))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut file = tokio::fs::File::create("./playit.toml").await?;

            file.write_all(
                toml::to_string(&AgentConfig {
                    last_update: None,
                    api_url: None,
                    refresh_from_api: true,
                    secret_key: "put-secret-here".to_string(),
                    mappings: vec![],
                }).unwrap().as_bytes(),
            ).await?;

            Ok(None)
        }
        Err(e) => Err(e),
    }
}
