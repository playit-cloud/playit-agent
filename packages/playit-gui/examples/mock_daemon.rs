//! Serves canned agent states over the playitd IPC protocol so the GUI can be developed
//! without a real agent secret.
//!
//!     cargo run -p playit-gui --example mock_daemon -- ./mock.sock running
//!     cargo run -p playit-gui -- --socket-path ./mock.sock
//!
//! Scenarios: running, empty, setup, invalid-secret, over-limit, error, starting.

#[cfg(unix)]
#[tokio::main]
async fn main() {
    unix::run().await;
}

#[cfg(not(unix))]
fn main() {
    eprintln!("mock_daemon only supports Unix sockets");
}

#[cfg(unix)]
mod unix {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use playit_ipc::ipc::{
        EventEnvelope, HelloEnvelope, IPC_VERSION, IpcTransport, RequestEnvelope, ResponseEnvelope,
        ServerEnvelope, ServiceRequest, ServiceResponse, protocol_info,
    };
    use playit_ipc::model::{
        AccountLoginUrlResponse, AccountStatus, AgentLifecycle, AgentState, CommandResponse,
        ConnectionStats, LogEntry, LogLevel, NoticeState, PendingTunnelState, SecretPathResponse,
        ServiceError, ServiceErrorCode, ServicePhase, ServiceStatus, ServiceUpdate,
        SubscribeResponse, SubscriptionSnapshot, TunnelState,
    };
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::broadcast;

    struct Mock {
        lifecycle: AgentLifecycle,
        stats: ConnectionStats,
        socket_path: String,
    }

    type Shared = Arc<Mutex<Mock>>;

    pub async fn run() {
        let mut args = std::env::args().skip(1);
        let socket_path = args.next().unwrap_or_else(|| "./mock.sock".to_string());
        let scenario = args.next().unwrap_or_else(|| "running".to_string());

        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("failed to bind mock socket");
        println!("mock playitd ({scenario}) listening on {socket_path}");

        let mock = Arc::new(Mutex::new(Mock {
            lifecycle: scenario_lifecycle(&scenario),
            stats: ConnectionStats::default(),
            socket_path: socket_path.clone(),
        }));
        let (events, _) = broadcast::channel(256);
        tokio::spawn(simulate_traffic(mock.clone(), events.clone()));

        loop {
            let (stream, _) = listener.accept().await.expect("accept failed");
            tokio::spawn(serve(stream, mock.clone(), events.clone()));
        }
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    fn running_state(scenario: &str) -> AgentState {
        let mut state = AgentState {
            version: "1.2.0".into(),
            account_status: AccountStatus::Verified,
            agent_id: "a3f1c9e2-5b7d-4e8a-9c21-7f0d6b4e2a18".into(),
            start_time: now() - 3 * 3_600_000 - 17 * 60_000 - 42_000,
            ..Default::default()
        };
        if scenario == "empty" {
            state.account_status = AccountStatus::Guest;
            return state;
        }
        state.tunnels = vec![
            TunnelState {
                display_address: "mc-survival.joinmc.link".into(),
                destination: "127.0.0.1:25565".into(),
                ..Default::default()
            },
            TunnelState {
                display_address: "bedrock-friends.gl.at.ply.gg:19132".into(),
                destination: "192.168.1.20:19132".into(),
                ..Default::default()
            },
            TunnelState {
                display_address: "web-demo.gl.at.ply.gg:7012".into(),
                destination: "127.0.0.1:8080".into(),
                ..Default::default()
            },
            TunnelState {
                display_address: "terraria-old.gl.at.ply.gg:41200".into(),
                destination: "127.0.0.1:7777".into(),
                is_disabled: true,
                disabled_reason: Some("over tunnel limit".into()),
            },
        ];
        state.pending_tunnels = vec![PendingTunnelState {
            id: "valheim".into(),
            status_msg: "allocating port".into(),
        }];
        state.notices = vec![NoticeState {
            priority: "low".into(),
            message: "A new version of playit is available.".into(),
            resolve_link: Some("https://playit.gg/download".into()),
        }];
        state
    }

    fn scenario_lifecycle(scenario: &str) -> AgentLifecycle {
        let error = |code, message: &str| ServiceError {
            code,
            message: message.into(),
            retryable: false,
            details: None,
        };
        match scenario {
            "setup" => AgentLifecycle::WaitingForSecret,
            "invalid-secret" => AgentLifecycle::HasInvalidSecret(error(
                ServiceErrorCode::InvalidSecret,
                "the API rejected this agent secret",
            )),
            "over-limit" => AgentLifecycle::DisabledOverLimit(error(
                ServiceErrorCode::AgentDisabledOverLimit,
                "agent limit reached",
            )),
            "error" => AgentLifecycle::Error(error(
                ServiceErrorCode::Internal,
                "failed to bind UDP socket: address already in use",
            )),
            "starting" => AgentLifecycle::Starting,
            other => AgentLifecycle::Running(running_state(other)),
        }
    }

    fn status(mock: &Mock) -> ServiceStatus {
        ServiceStatus {
            phase: match mock.lifecycle {
                AgentLifecycle::WaitingForSecret => ServicePhase::WaitingForSecret,
                AgentLifecycle::HasInvalidSecret(_) => ServicePhase::HasInvalidSecret,
                AgentLifecycle::DisabledOverLimit(_) => ServicePhase::DisabledOverLimit,
                AgentLifecycle::Starting => ServicePhase::Starting,
                AgentLifecycle::Running(_) => ServicePhase::Running,
                AgentLifecycle::Stopping => ServicePhase::Stopping,
                AgentLifecycle::Error(_) => ServicePhase::Error,
            },
            pid: std::process::id(),
            version: "1.2.0".into(),
            socket_path: mock.socket_path.clone(),
            has_secret: !matches!(mock.lifecycle, AgentLifecycle::WaitingForSecret),
            protocol: protocol_info(),
            ..Default::default()
        }
    }

    const LOG_LINES: &[(LogLevel, &str, &str)] = &[
        (
            LogLevel::Info,
            "playit_agent_core::network::tcp_clients",
            "new TCP client from 203.0.113.42:51822 to 127.0.0.1:25565",
        ),
        (
            LogLevel::Info,
            "playit_agent_core::network::udp_clients",
            "new UDP flow from 198.51.100.7:60211 to 192.168.1.20:19132",
        ),
        (
            LogLevel::Debug,
            "playit_agent_core::agent_control::maintained_control",
            "sent keep alive, latency=31ms",
        ),
        (
            LogLevel::Warn,
            "playit_agent_core::network::tcp_clients",
            "failed to connect to local service 127.0.0.1:8080: connection refused",
        ),
        (
            LogLevel::Info,
            "playit_agent_core::network::tcp_clients",
            "TCP client 203.0.113.42:51822 disconnected after 12m 40s",
        ),
        (
            LogLevel::Error,
            "playit_agent_core::network::udp_clients",
            "UDP send failed: message too long (os error 90)",
        ),
    ];

    const IDLE_LOG_LINES: &[(LogLevel, &str, &str)] = &[
        (
            LogLevel::Info,
            "playit",
            "Connected to playit; waiting for tunnels",
        ),
        (
            LogLevel::Debug,
            "playit_agent_core::agent_control::maintained_control",
            "sent keep alive, latency=31ms",
        ),
    ];

    fn lifecycle_log(lifecycle: &AgentLifecycle) -> (LogLevel, &'static str) {
        match lifecycle {
            AgentLifecycle::WaitingForSecret => (LogLevel::Info, "waiting for an agent secret"),
            AgentLifecycle::HasInvalidSecret(_) => (
                LogLevel::Error,
                "failed to authenticate, error=AuthenticationFailed",
            ),
            AgentLifecycle::DisabledOverLimit(_) => (
                LogLevel::Warn,
                "agent disabled: account is over its agent limit",
            ),
            AgentLifecycle::Error(_) => (
                LogLevel::Error,
                "failed to bind UDP socket: address already in use",
            ),
            _ => (LogLevel::Info, "connecting to the playit control server"),
        }
    }

    async fn simulate_traffic(mock: Shared, events: broadcast::Sender<ServiceUpdate>) {
        let mut tick = 0usize;
        loop {
            tokio::time::sleep(Duration::from_millis(700)).await;
            let (stats, has_tunnels) = {
                let mut mock = mock.lock().unwrap();
                let has_tunnels = match &mock.lifecycle {
                    AgentLifecycle::Running(state) => !state.tunnels.is_empty(),
                    other => {
                        // Other states log what the agent is trying, now and then.
                        if tick % 4 == 0 {
                            let (level, message) = lifecycle_log(other);
                            let _ = events.send(ServiceUpdate::Log(LogEntry {
                                level,
                                target: "playit_agent_core::agent_control".into(),
                                message: message.into(),
                                timestamp: now(),
                            }));
                        }
                        tick += 1;
                        continue;
                    }
                };
                if !has_tunnels {
                    (mock.stats.clone(), false)
                } else {
                    mock.stats.bytes_in += 48_211 + (tick as u64 * 977) % 20_000;
                    mock.stats.bytes_out += 1_284_337 + (tick as u64 * 7919) % 400_000;
                    mock.stats.active_tcp = 3 + (tick % 4) as u32;
                    mock.stats.active_udp = 2 + (tick % 3) as u32;
                    (mock.stats.clone(), true)
                }
            };
            // An idle agent announces itself once, then only sends keep-alives.
            let (level, target, message) = if has_tunnels {
                &LOG_LINES[tick % LOG_LINES.len()]
            } else {
                &IDLE_LOG_LINES[tick.min(1)]
            };
            let _ = events.send(ServiceUpdate::Log(LogEntry {
                level: level.clone(),
                target: target.to_string(),
                message: message.to_string(),
                timestamp: now(),
            }));
            let _ = events.send(ServiceUpdate::Stats(stats));
            tick += 1;
        }
    }

    fn set_lifecycle(
        mock: &Shared,
        events: &broadcast::Sender<ServiceUpdate>,
        lifecycle: AgentLifecycle,
    ) {
        let status = {
            let mut mock = mock.lock().unwrap();
            mock.lifecycle = lifecycle.clone();
            status(&mock)
        };
        let _ = events.send(ServiceUpdate::Status(status));
        let _ = events.send(ServiceUpdate::Lifecycle(lifecycle));
    }

    async fn serve(stream: UnixStream, mock: Shared, events: broadcast::Sender<ServiceUpdate>) {
        let (reader, writer) = stream.into_split();
        let mut transport = IpcTransport::new(reader, writer);
        let hello = ServerEnvelope::Hello(HelloEnvelope {
            protocol: protocol_info(),
        });
        if transport.write_json(&hello).await.is_err() {
            return;
        }

        while let Ok(envelope) = transport.read_json::<RequestEnvelope>().await {
            let mut subscribed = None;
            let response = match envelope.request {
                ServiceRequest::Subscribe => {
                    subscribed = Some(events.subscribe());
                    let mock = mock.lock().unwrap();
                    ServiceResponse::Subscribe(SubscribeResponse {
                        protocol: protocol_info(),
                        snapshot: SubscriptionSnapshot {
                            status: status(&mock),
                            lifecycle: mock.lifecycle.clone(),
                            stats: mock.stats.clone(),
                        },
                    })
                }
                ServiceRequest::GetStatus => ServiceResponse::Status(status(&mock.lock().unwrap())),
                ServiceRequest::GetState => {
                    ServiceResponse::State(mock.lock().unwrap().lifecycle.clone())
                }
                ServiceRequest::Stop => ServiceResponse::Stop(CommandResponse {
                    accepted: false,
                    message: Some("the mock daemon does not stop".into()),
                }),
                ServiceRequest::SetSecret { .. } => {
                    let mock_for_start = mock.clone();
                    let events_for_start = events.clone();
                    set_lifecycle(&mock, &events, AgentLifecycle::Starting);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        set_lifecycle(
                            &mock_for_start,
                            &events_for_start,
                            AgentLifecycle::Running(running_state("running")),
                        );
                    });
                    ServiceResponse::SetSecret(CommandResponse {
                        accepted: true,
                        message: None,
                    })
                }
                ServiceRequest::ResetSecret => {
                    set_lifecycle(&mock, &events, AgentLifecycle::WaitingForSecret);
                    ServiceResponse::ResetSecret(CommandResponse {
                        accepted: true,
                        message: Some("secret reset".into()),
                    })
                }
                ServiceRequest::GetSecretPath => ServiceResponse::SecretPath(SecretPathResponse {
                    secret_path: Some("./mock.toml".into()),
                }),
                ServiceRequest::GetAccountLoginUrl => {
                    ServiceResponse::AccountLoginUrl(AccountLoginUrlResponse {
                        login_url: "https://playit.gg/account".into(),
                    })
                }
            };

            let response = ServerEnvelope::Response(ResponseEnvelope {
                ipc_version: IPC_VERSION,
                request_id: envelope.request_id,
                response,
            });
            if transport.write_json(&response).await.is_err() {
                return;
            }

            if let Some(mut updates) = subscribed {
                while let Ok(event) = updates.recv().await {
                    let event = ServerEnvelope::Event(EventEnvelope {
                        ipc_version: IPC_VERSION,
                        event,
                    });
                    if transport.write_json(&event).await.is_err() {
                        return;
                    }
                }
                return;
            }
        }
    }
}
