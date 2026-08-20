use playit_ipc::ipc::{
    EventEnvelope, HelloEnvelope, IPC_VERSION, RequestEnvelope, ResponseEnvelope, ServerEnvelope,
    ServiceRequest, ServiceResponse, protocol_info,
};
use playit_ipc::model::{
    AccountLoginUrlResponse, AccountStatus, AgentLifecycle, AgentState, CommandResponse,
    ConnectionStats, LogEntry, LogLevel, NoticeState, PendingTunnelState, SecretPathResponse,
    ServiceError, ServiceErrorCode, ServicePhase, ServiceStatus, ServiceUpdate, SubscribeResponse,
    SubscriptionSnapshot, TunnelState,
};

fn fixture_error(
    code: ServiceErrorCode,
    message: &str,
    retryable: bool,
    details: Option<serde_json::Value>,
) -> ServiceError {
    ServiceError {
        code,
        message: message.to_string(),
        retryable,
        details,
    }
}

fn running_state() -> AgentState {
    AgentState {
        version: "1.0.10".to_string(),
        tunnels: vec![TunnelState {
            display_address: "demo.playit.gg:25565".to_string(),
            destination: "127.0.0.1:25565".to_string(),
            is_disabled: false,
            disabled_reason: None,
        }],
        pending_tunnels: vec![PendingTunnelState {
            id: "pending-1".to_string(),
            status_msg: "allocating".to_string(),
        }],
        notices: vec![NoticeState {
            priority: "info".to_string(),
            message: "fixture notice".to_string(),
            resolve_link: Some("https://playit.gg/account".to_string()),
        }],
        account_status: AccountStatus::Verified,
        agent_id: "agent-1".to_string(),
        login_link: Some("https://playit.gg/login/fixture".to_string()),
        start_time: 1_700_000_000_000,
    }
}

fn fixture_status(phase: ServicePhase, uptime_secs: u64) -> ServiceStatus {
    ServiceStatus {
        phase,
        pid: 4242,
        uptime_secs,
        version: "1.0.10".to_string(),
        socket_path: "/run/playit/playitd.sock".to_string(),
        secret_path: Some("/etc/playit/playit.toml".to_string()),
        has_secret: true,
        protocol: protocol_info(),
        last_error: None,
    }
}

fn serialize_lines<T: serde::Serialize>(values: &[T]) -> Vec<String> {
    values
        .iter()
        .map(|value| serde_json::to_string(value).unwrap())
        .collect()
}

fn fixture_lines(fixture: &str) -> Vec<String> {
    fixture.lines().map(str::to_string).collect()
}

#[test]
fn client_transcript_remains_ipc_v2_compatible() {
    let requests = vec![
        ServiceRequest::Subscribe,
        ServiceRequest::GetStatus,
        ServiceRequest::GetState,
        ServiceRequest::Stop,
        ServiceRequest::SetSecret {
            secret: "fixture-secret".to_string(),
        },
        ServiceRequest::ResetSecret,
        ServiceRequest::GetSecretPath,
        ServiceRequest::GetAccountLoginUrl,
    ];
    let envelopes: Vec<_> = requests
        .into_iter()
        .enumerate()
        .map(|(index, request)| RequestEnvelope {
            ipc_version: IPC_VERSION,
            request_id: index as u64 + 1,
            request,
        })
        .collect();

    let actual = serialize_lines(&envelopes);
    let fixture = include_str!("../fixtures/ipc_v2_client_transcript.jsonl");
    assert_eq!(actual, fixture_lines(fixture));

    for line in fixture.lines() {
        let decoded: RequestEnvelope = serde_json::from_str(line).unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap(), line);
    }
}

#[test]
fn server_transcript_remains_ipc_v2_compatible() {
    let status = fixture_status(ServicePhase::Running, 17);
    let state = AgentLifecycle::Running(running_state());
    let stats = ConnectionStats {
        bytes_in: 10,
        bytes_out: 20,
        active_tcp: 1,
        active_udp: 2,
    };
    let envelopes = vec![
        ServerEnvelope::Hello(HelloEnvelope {
            protocol: protocol_info(),
        }),
        ServerEnvelope::Response(ResponseEnvelope {
            ipc_version: IPC_VERSION,
            request_id: 1,
            response: ServiceResponse::Subscribe(SubscribeResponse {
                protocol: protocol_info(),
                snapshot: SubscriptionSnapshot {
                    status,
                    lifecycle: state,
                    stats: stats.clone(),
                },
            }),
        }),
        ServerEnvelope::Response(ResponseEnvelope {
            ipc_version: IPC_VERSION,
            request_id: 4,
            response: ServiceResponse::Stop(CommandResponse {
                accepted: true,
                message: Some("shutdown requested".to_string()),
            }),
        }),
        ServerEnvelope::Response(ResponseEnvelope {
            ipc_version: IPC_VERSION,
            request_id: 7,
            response: ServiceResponse::SecretPath(SecretPathResponse {
                secret_path: Some("/etc/playit/playit.toml".to_string()),
            }),
        }),
        ServerEnvelope::Response(ResponseEnvelope {
            ipc_version: IPC_VERSION,
            request_id: 8,
            response: ServiceResponse::AccountLoginUrl(AccountLoginUrlResponse {
                login_url: "https://playit.gg/login/fixture".to_string(),
            }),
        }),
        ServerEnvelope::Response(ResponseEnvelope {
            ipc_version: IPC_VERSION,
            request_id: 9,
            response: ServiceResponse::Error(fixture_error(
                ServiceErrorCode::InvalidRequestType,
                "unsupported request",
                false,
                Some(serde_json::json!({"request_type": "future_request"})),
            )),
        }),
        ServerEnvelope::Event(EventEnvelope {
            ipc_version: IPC_VERSION,
            event: ServiceUpdate::Status(fixture_status(ServicePhase::Stopping, 18)),
        }),
        ServerEnvelope::Event(EventEnvelope {
            ipc_version: IPC_VERSION,
            event: ServiceUpdate::Lifecycle(AgentLifecycle::Stopping),
        }),
        ServerEnvelope::Event(EventEnvelope {
            ipc_version: IPC_VERSION,
            event: ServiceUpdate::Stats(stats),
        }),
        ServerEnvelope::Event(EventEnvelope {
            ipc_version: IPC_VERSION,
            event: ServiceUpdate::Log(LogEntry {
                level: LogLevel::Warn,
                target: "playitd::fixture".to_string(),
                message: "fixture log".to_string(),
                timestamp: 1_700_000_000_123,
            }),
        }),
    ];

    let actual = serialize_lines(&envelopes);
    let fixture = include_str!("../fixtures/ipc_v2_server_transcript.jsonl");
    assert_eq!(actual, fixture_lines(fixture));

    for line in fixture.lines() {
        let decoded: ServerEnvelope = serde_json::from_str(line).unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap(), line);
    }
}

#[test]
fn lifecycle_json_remains_ipc_v2_compatible() {
    let states = vec![
        AgentLifecycle::WaitingForSecret,
        AgentLifecycle::HasInvalidSecret(fixture_error(
            ServiceErrorCode::InvalidSecret,
            "invalid fixture secret",
            true,
            None,
        )),
        AgentLifecycle::DisabledOverLimit(fixture_error(
            ServiceErrorCode::AgentDisabledOverLimit,
            "fixture account is over limit",
            true,
            Some(serde_json::json!({"agents": 3, "limit": 2})),
        )),
        AgentLifecycle::Starting,
        AgentLifecycle::Running(running_state()),
        AgentLifecycle::Stopping,
        AgentLifecycle::Error(fixture_error(
            ServiceErrorCode::Internal,
            "fixture failure",
            false,
            None,
        )),
    ];

    let actual = serialize_lines(&states);
    let fixture = include_str!("../fixtures/ipc_v2_lifecycle.jsonl");
    assert_eq!(actual, fixture_lines(fixture));

    for line in fixture.lines() {
        let decoded: AgentLifecycle = serde_json::from_str(line).unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap(), line);
    }
}
