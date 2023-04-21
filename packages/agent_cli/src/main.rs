use std::fmt::Display;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{RwLock, RwLockMappedWriteGuard, RwLockWriteGuard};
use tracing::Level;

use graphics::{Connected, GraphicInterface, GraphicState};
use playit_agent_common::Proto;
use playit_agent_common::agent_config::AgentConfigBuilder;
use playit_agent_core::agent_state::AgentState;
use playit_agent_core::agent_updater::AgentUpdater;
use playit_agent_core::api_client::ApiClient;
use playit_agent_core::control_lookup::get_working_io;
use playit_agent_core::ping_task::PingTask;
use playit_agent_core::setup_config::{AgentConfigStatus, prepare_config};
use playit_agent_core::tcp_client::{TcpClients, TcpConnection};
use playit_agent_core::tunnel_api::TunnelApi;

use crate::graphics::{ConnectedElement, Notice};
use crate::logging::{LoggingBuffer, LogReader};
use crate::start_settings::StartSettings;

mod graphics;
mod logging;
mod start_settings;
mod tray;

struct GraphicWrapper {
    inner: Option<Arc<RwLock<GraphicState>>>,
    log_reader: Option<LogReader>,
}

impl GraphicWrapper {
    pub fn is_setup(&self) -> bool {
        self.inner.is_some()
    }

    pub async fn connected_mut(&mut self) -> Option<RwLockMappedWriteGuard<Connected>> {
        let mut lock = self.inner.as_ref()?.write().await;

        match &mut *lock {
            GraphicState::Connected(_) => {}
            other => {
                *other = GraphicState::Connected(Connected {
                    focused: ConnectedElement::Overview,
                    ping_samples: Default::default(),
                    config: Arc::new(AgentConfigBuilder::default().build()),
                    log_reader: self.log_reader.take().unwrap(),
                    logs: Default::default(),
                    selected_tunnel_pos: 0,
                    agent_state: Arc::new(Default::default()),
                    tcp_clients: Arc::new(Default::default()),
                });
            }
        }

        Some(RwLockWriteGuard::map(lock, |locked| {
            locked.connected_mut().unwrap()
        }))
    }

    pub async fn set_loading<S: ToString + Display>(&mut self, msg: S) {
        if !self.set(GraphicState::Loading { message: msg.to_string() }).await {
            tracing::info!(%msg, "loading");
        }
    }

    pub async fn set_activate_link<S: ToString + Display>(&mut self, link: S) {
        if !self.set(GraphicState::LinkAgent { url: link.to_string() }).await {
            tracing::info!(%link, "visit link to setup playit agent");
        }
    }

    async fn set(&mut self, updated: GraphicState) -> bool {
        if let Some(graphics) = &self.inner {
            let mut state = graphics.write().await;
            match std::mem::replace(&mut *state, updated) {
                GraphicState::Connected(connected) => {
                    self.log_reader = Some(connected.log_reader);
                }
                _ => {}
            }
            true
        } else {
            false
        }
    }
}

#[tokio::main]
async fn main() {
    let settings = StartSettings::parse();
    let mut background_task_handles = Vec::new();

    
    let mut graphics = if settings.try_ui {
        match GraphicInterface::new() {
            Ok(graphics) => {
                let state = graphics.state();
                let task = tokio::spawn(graphics.run());
                background_task_handles.push(task);

                let log_reader = {
                    let mut log = LoggingBuffer::new();
                    let log_reader = log.reader().unwrap();
                    tracing_subscriber::fmt().with_ansi(false).with_max_level(Level::INFO).with_writer(log).init();
                    log_reader
                };

                GraphicWrapper { inner: Some(state), log_reader: Some(log_reader) }
            }
            Err(_) => {
                tracing_subscriber::fmt().with_ansi(false).with_max_level(Level::INFO).init();
                tracing::warn!("failed to start graphical UI, running in 'stdout_logs' mode");
                GraphicWrapper { inner: None, log_reader: None }
            }
        }
    } else {
        tracing_subscriber::fmt().with_ansi(false).with_max_level(Level::INFO).init();
        tracing::info!("starting without trying UI");
        GraphicWrapper { inner: None, log_reader: None }
    };

    tracing::info!(?settings, "starting playit agent, version: {}", env!("CARGO_PKG_VERSION"));
    graphics.set(GraphicState::Loading { message: format!("loading playit.toml ({})", settings.config_file_path) }).await;

    let status = Arc::new(RwLock::new(AgentConfigStatus::default()));

    let prepare_config_task = {
        let status = status.clone();

        let config_path = settings.config_file_path.clone();
        tokio::spawn(async move {
            prepare_config(&config_path, &status).await
        })
    };

    let agent_config_res = loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let status = status.read().await;

        match &*status {
            AgentConfigStatus::Staring => {
                graphics.set_loading(format!("loading playit.toml ({})", settings.config_file_path)).await;
            }
            AgentConfigStatus::ReadingConfigFile => {
                graphics.set_loading(format!("reading playit.toml ({})", settings.config_file_path)).await;
            }
            AgentConfigStatus::FileReadFailed => {
                graphics.set_loading(format!("failed to read playit.toml (delete \"{}\" and restart)", settings.config_file_path)).await;
            }
            AgentConfigStatus::LoadingAccountStatus => {
                graphics.set_loading("loading account status").await;
            }
            AgentConfigStatus::ErrorLoadingAccountStatus => {
                graphics.set_loading(format!("failed to load account status (delete \"{}\" and restart)", settings.config_file_path)).await;
            }
            AgentConfigStatus::AccountVerified => {
                graphics.set_loading("account verified").await;
            }
            AgentConfigStatus::PleaseActiveProgram { url } => {
                graphics.set_activate_link(url).await;
            }
            AgentConfigStatus::ProgramActivated => {
                graphics.set_loading("playit.toml loaded").await;
            }
            AgentConfigStatus::PleaseVerifyAccount { url } => {
                graphics.set(GraphicState::Notice(Notice {
                    message: "Please verify your account".to_string(),
                    url: (**url).clone(),
                    important: true,
                })).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            AgentConfigStatus::PleaseCreateAccount { url } => {
                graphics.set(GraphicState::Notice(Notice {
                    message: "Please create an account".to_string(),
                    url: (**url).clone(),
                    important: true,
                })).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            AgentConfigStatus::UserNotice { message, url, important } => {
                graphics.set(GraphicState::Notice(Notice {
                    message: (**message).to_string(),
                    url: (**url).clone(),
                    important: *important,
                })).await;

                let wait = if *important {
                    10
                } else {
                    5
                };

                tokio::time::sleep(Duration::from_secs(wait)).await;
            }
        }

        if prepare_config_task.is_finished() {
            break prepare_config_task.await.unwrap();
        }
    };

    let mut agent_config = match agent_config_res {
        Ok(config) => config.build(),
        Err(error) => {
            tracing::error!(?error, "failed to prepare config");
            tokio::time::sleep(Duration::from_secs(2)).await;
            graphics.set_loading("failed to prepare config, maybe delete the playit.toml file").await;
            tokio::time::sleep(Duration::from_secs(5)).await;
            return;
        }
    };

    if agent_config.api_refresh_rate.is_some() {
        graphics.set_loading("loading latest configuration").await;

        let api_client = ApiClient::new(agent_config.api_url.clone(), Some(agent_config.secret_key.clone()));
        agent_config = match api_client.get_agent_config().await {
            Ok(updated) => agent_config.to_updated(updated.build()),
            Err(error) => {
                tracing::error!(?error, "failed to load latest config");
                graphics.set_loading("failed to load latest config").await;
                tokio::time::sleep(Duration::from_secs(5)).await;
                return;
            }
        };
    } else {
        tracing::warn!("refresh_from_api set to false, will not update configuration from API");
    }

    graphics.set_loading(format!("preparing connection to {:?}", agent_config.control_address)).await;

    let tunnel_io = match get_working_io(&agent_config.control_address).await {
        Some(v) => v,
        None => {
            graphics.set_loading("failed to connect to tunnel").await;
            tokio::time::sleep(Duration::from_secs(5)).await;
            return;
        }
    };

    tracing::info!("established connection to tunnel");
    graphics.set_loading("authenticating connection").await;

    let api_client = ApiClient::new(
        agent_config.api_url.clone(),
        Some(agent_config.secret_key.clone()),
    );

    let tunnel_api = TunnelApi::new(api_client, tunnel_io);
    let agent_updater = Arc::new(AgentUpdater::new(tunnel_api, AgentState {
        agent_config: RwLock::new(Arc::new(agent_config)),
        agent_config_save_path: Some(settings.config_file_path),
        ..AgentState::default()
    }));

    let agent_update_loop = {
        let agent_updater = agent_updater.clone();

        tokio::spawn(async move {
            loop {
                let wait = match agent_updater.update().await {
                    Ok(wait) => wait,
                    Err(error) => {
                        tracing::error!(?error, "failed to update agent");
                        1000
                    }
                };

                tokio::time::sleep(Duration::from_millis(wait)).await;
            }
        })
    };

    let _ping_task_loop = {
        let ping_task = PingTask::new(agent_updater.state());
        tokio::spawn(ping_task.run())
    };

    let tcp_clients = Arc::new(TcpClients::default());

    /* process messages from tunnel server */
    let _message_process_task = {
        let agent_updater = agent_updater.clone();
        let tcp_clients = tcp_clients.clone();

        tokio::spawn(async move {
            loop {
                match agent_updater.process_tunnel_feed().await {
                    Ok(Some(client)) => {
                        tracing::info!(?client, "got new client");

                        let agent_updater = agent_updater.clone();
                        let tcp_clients = tcp_clients.clone();

                        tokio::spawn(async move {
                            let (_bind_ip, local_addr) = {
                                let state = agent_updater.state();
                                let config = state.agent_config.read().await;

                                match config.find_local_addr(client.connect_addr, Proto::Tcp) {
                                    Some(v) => v,
                                    None => {
                                        tracing::info!(connect_addr = %client.connect_addr, "could not find tunnel for new connection");
                                        return;
                                    }
                                }
                            };

                            let conn_res = TcpConnection::spawn(
                                client,
                                local_addr,
                                tcp_clients,
                            ).await;

                            let connection = match conn_res {
                                Ok(connection) => connection,
                                Err(error) => {
                                    tracing::error!(?error, "failed to setup connection");
                                    return;
                                }
                            };

                            connection.wait().await;
                        });
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::error!(?error, "got error processing tunnel feed");
                    }
                }
            }
        })
    };

    let mut i = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        if agent_updater.state().authenticate_times.has_ack() {
            break;
        }

        i += 1;
        if i % 2 == 0 {
            graphics.set_loading("authenticating connection...").await;
        } else {
            graphics.set_loading("authenticating connection..").await;
        }
    }

    graphics.set_loading(format!("connection authenticated")).await;

    /* setup connected UI and set agent_state */
    if let Some(mut connected) = graphics.connected_mut().await {
        connected.agent_state = agent_updater.state();
        connected.tcp_clients = tcp_clients;
    }

    if graphics.is_setup() {
        let agent_updater = agent_updater.clone();

        /* task for updating UI state */
        tokio::spawn(async move {
            let mut last_ping_received = 0;

            loop {
                let ping_value = {
                    let _ = agent_updater.send_ping().await;
                    let last_ping_time = agent_updater.state().latency_update.load(std::sync::atomic::Ordering::SeqCst);

                    if last_ping_received != last_ping_time {
                        last_ping_received = last_ping_time;
                        Some(agent_updater.state().latency.load(std::sync::atomic::Ordering::SeqCst))
                    } else {
                        None
                    }
                };

                if let Some(mut connected) = graphics.connected_mut().await {
                    let state = agent_updater.state();
                    let agent_config = Arc::clone(&*state.agent_config.read().await);
                    connected.config = agent_config;

                    if let Some(ping) = ping_value {
                        connected.ping_samples.push_front(ping);
                    }
                } else {
                    break;
                }

                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }

    if let Err(error) = agent_update_loop.await {
        tracing::error!(?error, "update loop error");
    }
}
