use std::net::IpAddr;
use std::num::{NonZeroU16, NonZeroU64};
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use playit_agent_core::gateway::{
    AgentVersion, GatewayError, GatewayErrorCode, GatewayOrigin, GatewayOriginHost,
    GatewayOriginTarget, GatewayRunData, Platform, PlayitGateway, RegisterRequest, RunDataContext,
    SignedAgentKey,
};
use playit_api_client::PlayitApi;
use playit_api_client::api::{
    AccountStatus as ApiAccountStatus, AgentRunDataV1, AgentTunnelV1, ApiError, ApiErrorNoFail,
    ApiResponseError, AuthError, ClaimAgentType, ClaimSetupResponse, Platform as ApiPlatform,
    PortType, ProtoRegisterError, ProxyProtocol as ApiProxyProtocol, ReqAgentsRoutingGet,
    ReqClaimExchange, ReqClaimSetup, ReqProtoRegister, TunnelType,
};
use playit_api_client::http_client::HttpClientError;
use playit_model::{
    AccountStatus, CatalogTunnel, Hostname, Notice, NoticeContent, NoticePriority, OriginHost,
    OriginTarget, PendingTunnel, Problem, ProblemCode, ProblemSubject, ProxyProtocol, SubjectKind,
    TunnelAvailability, TunnelCatalog, TunnelId, TunnelProtocol, TunnelSpec,
};
use tokio::sync::RwLock;

use crate::{ClaimExchange, ClaimFailure, ClaimGateway, ClaimMode, ClaimProgress};

const LOGIN_CACHE_MILLIS: u64 = 15_000;

#[derive(Clone)]
pub struct GeneratedClientGateway {
    client: PlayitApi,
    login_cache: Arc<LoginUrlCache>,
}

#[derive(Default)]
struct LoginUrlCache {
    entry: RwLock<Option<(String, u64)>>,
}

impl LoginUrlCache {
    async fn get(&self, now_millis: u64) -> Option<String> {
        let entry = self.entry.read().await;
        match &*entry {
            Some((url, cached_at))
                if now_millis.saturating_sub(*cached_at) < LOGIN_CACHE_MILLIS =>
            {
                Some(url.clone())
            }
            _ => None,
        }
    }

    async fn store(&self, url: String, now_millis: u64) {
        *self.entry.write().await = Some((url, now_millis));
    }
}

impl GeneratedClientGateway {
    pub fn new(api_base: String, secret: String) -> Self {
        Self {
            client: PlayitApi::create(api_base, Some(secret)),
            login_cache: Arc::new(LoginUrlCache::default()),
        }
    }

    pub(crate) fn without_secret(api_base: String) -> Self {
        Self {
            client: PlayitApi::create(api_base, None),
            login_cache: Arc::new(LoginUrlCache::default()),
        }
    }

    async fn cached_login_url(&self, now_millis: u64) -> Result<String, GatewayError> {
        if let Some(url) = self.login_cache.get(now_millis).await {
            return Ok(url);
        }

        let session = self.client.login_guest().await.map_err(map_login_error)?;
        let url = format!(
            "https://playit.gg/login/guest-account/{}",
            session.session_key
        );
        self.login_cache.store(url.clone(), now_millis).await;
        Ok(url)
    }
}

#[async_trait]
impl ClaimGateway for GeneratedClientGateway {
    async fn progress(
        &self,
        code: &str,
        mode: ClaimMode,
        version: &str,
    ) -> Result<ClaimProgress, ClaimFailure> {
        let agent_type = match mode {
            ClaimMode::Assignable => ClaimAgentType::Assignable,
            ClaimMode::SelfManaged => ClaimAgentType::SelfManaged,
        };
        self.client
            .claim_setup(ReqClaimSetup {
                code: code.to_owned(),
                agent_type,
                version: version.to_owned(),
            })
            .await
            .map(|progress| match progress {
                ClaimSetupResponse::WaitingForUserVisit => ClaimProgress::WaitingForVisit,
                ClaimSetupResponse::WaitingForUser => ClaimProgress::WaitingForApproval,
                ClaimSetupResponse::UserAccepted => ClaimProgress::Approved,
                ClaimSetupResponse::UserRejected => ClaimProgress::Rejected,
            })
            .map_err(claim_error)
    }

    async fn exchange(&self, code: &str) -> Result<ClaimExchange, ClaimFailure> {
        match self
            .client
            .claim_exchange(ReqClaimExchange {
                code: code.to_owned(),
            })
            .await
        {
            Ok(response) => Ok(ClaimExchange::Accepted(response.secret_key)),
            Err(ApiError::Fail(status)) => Ok(ClaimExchange::Pending(format!("{status:?}"))),
            Err(error) => Err(claim_error(error)),
        }
    }
}

fn claim_error(error: impl std::fmt::Debug) -> ClaimFailure {
    ClaimFailure {
        problem: Problem::new(ProblemCode::ProvisioningUnavailable),
        detail: format!("{error:?}"),
    }
}

#[async_trait]
impl PlayitGateway for GeneratedClientGateway {
    async fn register(&self, request: RegisterRequest) -> Result<SignedAgentKey, GatewayError> {
        let registered = self
            .client
            .proto_register(ReqProtoRegister {
                agent_version: None,
                proto_version: request.proto_version,
                version: api_version(request.version),
                platform: api_platform(request.platform),
                client_addr: request.client_addr,
                tunnel_addr: request.tunnel_addr,
            })
            .await
            .map_err(map_register_error)?;
        Ok(SignedAgentKey {
            key: registered.key,
        })
    }

    async fn control_addresses(&self) -> Result<Vec<std::net::SocketAddr>, GatewayError> {
        let routing = self
            .client
            .agents_routing_get(ReqAgentsRoutingGet { agent_id: None })
            .await
            .map_err(|error| map_api_error("control_addresses", error))?;
        Ok(routing
            .targets6
            .into_iter()
            .map(|address| std::net::SocketAddr::new(address.into(), 5525))
            .chain(
                routing
                    .targets4
                    .into_iter()
                    .map(|address| std::net::SocketAddr::new(address.into(), 5525)),
            )
            .collect())
    }

    async fn run_data(&self, context: RunDataContext) -> Result<GatewayRunData, GatewayError> {
        let data = self
            .client
            .v1_agents_rundata()
            .await
            .map_err(|error| map_no_fail_error("run_data", error))?;
        convert_run_data(self, data, context).await
    }

    async fn account_login_url(&self) -> Result<String, GatewayError> {
        self.cached_login_url(now_millis()).await
    }
}

async fn convert_run_data(
    gateway: &GeneratedClientGateway,
    mut data: AgentRunDataV1,
    context: RunDataContext,
) -> Result<GatewayRunData, GatewayError> {
    let mut origins = Vec::with_capacity(data.tunnels.len());
    let mut catalog_tunnels = Vec::with_capacity(data.tunnels.len());

    for tunnel in &data.tunnels {
        let converted = convert_tunnel(tunnel)?;
        origins.push(converted.origin);
        catalog_tunnels.push(converted.catalog);
    }

    let catalog = TunnelCatalog::try_new(
        context.catalog_revision,
        context.accepted_at_millis,
        catalog_tunnels,
    )
    .map_err(|error| {
        GatewayError::new(
            GatewayErrorCode::InvalidRunData,
            "run_data",
            format!("invalid tunnel catalog: {error:?}"),
        )
    })?;
    let account_status = map_account_status(data.permissions.account_status);
    let login_link = if account_status == AccountStatus::Guest {
        gateway
            .cached_login_url(context.accepted_at_millis)
            .await
            .ok()
    } else {
        None
    };

    data.notices.sort_by_key(|notice| notice.priority);
    let notices = data
        .notices
        .into_iter()
        .map(|notice| Notice {
            problem: Problem::new(ProblemCode::RemoteNotice),
            content: Some(NoticeContent {
                priority: match notice.priority {
                    playit_api_client::api::AgentNoticePriority::Critical => {
                        NoticePriority::Critical
                    }
                    playit_api_client::api::AgentNoticePriority::High => NoticePriority::High,
                    playit_api_client::api::AgentNoticePriority::Low => NoticePriority::Low,
                },
                message: notice.message.into_owned(),
                resolve_url: notice.resolve_link,
            }),
        })
        .collect();

    Ok(GatewayRunData {
        agent_id: data.agent_id.to_string(),
        origins,
        catalog,
        account_status,
        pending: data
            .pending
            .into_iter()
            .map(|pending| PendingTunnel {
                id: pending.id.to_string(),
                status: pending.status_msg,
            })
            .collect(),
        notices,
        login_url: login_link,
    })
}

#[derive(Debug)]
struct ConvertedTunnel {
    origin: GatewayOrigin,
    catalog: CatalogTunnel,
}

fn convert_tunnel(tunnel: &AgentTunnelV1) -> Result<ConvertedTunnel, GatewayError> {
    let target = parse_target(tunnel)?;
    let protocol = match tunnel.port_type {
        PortType::Tcp => TunnelProtocol::Tcp,
        PortType::Udp => TunnelProtocol::Udp,
        PortType::Both => TunnelProtocol::Both,
    };
    let proxy_protocol = parse_proxy_protocol(tunnel)?;
    let id =
        NonZeroU64::new(tunnel.internal_id).ok_or_else(|| invalid_tunnel(tunnel, "zero id"))?;
    let id = TunnelId::new(id);
    let subject = ProblemSubject {
        kind: SubjectKind::Tunnel,
        id: id.get(),
    };
    let catalog_target = match &target {
        GatewayOriginTarget::Port { host, port } => OriginTarget::Port {
            host: catalog_host(host, tunnel)?,
            first_port: NonZeroU16::new(*port)
                .ok_or_else(|| invalid_tunnel(tunnel, "zero local port"))?,
        },
        GatewayOriginTarget::Https {
            host,
            http_port,
            https_port,
        } => OriginTarget::Https {
            host: catalog_host(host, tunnel)?,
            http_port: NonZeroU16::new(*http_port)
                .ok_or_else(|| invalid_tunnel(tunnel, "zero HTTP port"))?,
            https_port: NonZeroU16::new(*https_port)
                .ok_or_else(|| invalid_tunnel(tunnel, "zero HTTPS port"))?,
        },
    };
    let availability = if tunnel.disabled_reason.is_some() {
        TunnelAvailability::Disabled(
            Problem::new(ProblemCode::TunnelDisabled).with_subject(subject),
        )
    } else {
        TunnelAvailability::Active
    };

    Ok(ConvertedTunnel {
        origin: GatewayOrigin {
            tunnel_id: tunnel.internal_id,
            protocol,
            target,
            port_count: tunnel.port_count,
            proxy_protocol,
        },
        catalog: CatalogTunnel {
            spec: TunnelSpec {
                id,
                protocol,
                target: catalog_target,
                port_count: NonZeroU16::new(tunnel.port_count).unwrap_or(NonZeroU16::MIN),
                proxy_protocol,
            },
            availability,
            public_address: tunnel.display_address.clone(),
            disabled_reason: tunnel.disabled_reason.as_ref().map(ToString::to_string),
        },
    })
}

fn parse_target(tunnel: &AgentTunnelV1) -> Result<GatewayOriginTarget, GatewayError> {
    let host = tunnel
        .agent_config
        .fields
        .iter()
        .find(|field| field.name == "local_ip")
        .map(|field| field.value.trim())
        .filter(|value| !value.is_empty())
        .map(parse_host)
        .unwrap_or_else(|| GatewayOriginHost::Ip("127.0.0.1".parse().unwrap()));
    let tunnel_type = tunnel
        .tunnel_type
        .clone()
        .and_then(|value| serde_json::from_value::<TunnelType>(value.into()).ok());

    if matches!(tunnel_type, Some(TunnelType::Https)) {
        return Ok(GatewayOriginTarget::Https {
            host,
            http_port: field_port(tunnel, "http_port").unwrap_or(80),
            https_port: field_port(tunnel, "https_port").unwrap_or(443),
        });
    }

    let port = field_port(tunnel, "local_port")
        .or_else(|| tunnel.display_address.rsplit(':').next()?.parse().ok())
        .ok_or_else(|| invalid_tunnel(tunnel, "missing local port"))?;
    Ok(GatewayOriginTarget::Port { host, port })
}

fn parse_host(value: &str) -> GatewayOriginHost {
    IpAddr::from_str(value)
        .map(GatewayOriginHost::Ip)
        .unwrap_or_else(|_| GatewayOriginHost::Hostname(value.to_owned()))
}

fn field_port(tunnel: &AgentTunnelV1, name: &str) -> Option<u16> {
    tunnel
        .agent_config
        .fields
        .iter()
        .find(|field| field.name == name)
        .and_then(|field| field.value.parse().ok())
}

fn parse_proxy_protocol(tunnel: &AgentTunnelV1) -> Result<ProxyProtocol, GatewayError> {
    let value = tunnel
        .agent_config
        .fields
        .iter()
        .find(|field| field.name == "proxy_protocol")
        .map(|field| field.value.clone());
    let Some(value) = value else {
        return Ok(ProxyProtocol::None);
    };
    let parsed = serde_json::from_value::<ApiProxyProtocol>(value.into())
        .map_err(|_| invalid_tunnel(tunnel, "invalid proxy protocol"))?;
    Ok(match parsed {
        ApiProxyProtocol::ProxyProtocolV1 => ProxyProtocol::V1,
        ApiProxyProtocol::ProxyProtocolV2 => ProxyProtocol::V2,
    })
}

fn catalog_host(
    host: &GatewayOriginHost,
    tunnel: &AgentTunnelV1,
) -> Result<OriginHost, GatewayError> {
    match host {
        GatewayOriginHost::Ip(address) => Ok(OriginHost::Ip(*address)),
        GatewayOriginHost::Hostname(hostname) => Hostname::parse(hostname.clone())
            .map(OriginHost::Hostname)
            .map_err(|_| invalid_tunnel(tunnel, "invalid local hostname")),
    }
}

fn invalid_tunnel(tunnel: &AgentTunnelV1, detail: &str) -> GatewayError {
    GatewayError::new(
        GatewayErrorCode::InvalidRunData,
        "run_data",
        format!("tunnel {}: {detail}", tunnel.internal_id),
    )
}

fn map_account_status(status: ApiAccountStatus) -> AccountStatus {
    match status {
        ApiAccountStatus::Guest => AccountStatus::Guest,
        ApiAccountStatus::EmailNotVerified => AccountStatus::EmailNotVerified,
        ApiAccountStatus::Verified => AccountStatus::Verified,
    }
}

fn api_version(version: AgentVersion) -> playit_api_client::api::AgentVersion {
    playit_api_client::api::AgentVersion {
        variant_id: version.variant_id,
        version_major: version.major,
        version_minor: version.minor,
        version_patch: version.patch,
    }
}

fn api_platform(platform: Platform) -> ApiPlatform {
    match platform {
        Platform::Linux => ApiPlatform::Linux,
        Platform::Freebsd => ApiPlatform::Freebsd,
        Platform::Windows => ApiPlatform::Windows,
        Platform::Macos => ApiPlatform::Macos,
        Platform::Android => ApiPlatform::Android,
        Platform::Ios => ApiPlatform::Ios,
        Platform::Docker => ApiPlatform::Docker,
        Platform::MinecraftPlugin => ApiPlatform::MinecraftPlugin,
        Platform::Unknown => ApiPlatform::Unknown,
    }
}

fn map_register_error(error: ApiError<ProtoRegisterError, HttpClientError>) -> GatewayError {
    match error {
        ApiError::Fail(ProtoRegisterError::AgentDisabledOverLimit) => GatewayError::new(
            GatewayErrorCode::AgentDisabledOverLimit,
            "register",
            "agent disabled because the account is over its agent limit",
        ),
        ApiError::Fail(error) => {
            GatewayError::new(GatewayErrorCode::Rejected, "register", format!("{error:?}"))
        }
        ApiError::ApiError(error) => map_response_error("register", error),
        ApiError::ClientError(error) => transport_error("register", error),
    }
}

fn map_api_error<F: std::fmt::Debug>(
    operation: &'static str,
    error: ApiError<F, HttpClientError>,
) -> GatewayError {
    match error {
        ApiError::Fail(error) => {
            GatewayError::new(GatewayErrorCode::Rejected, operation, format!("{error:?}"))
        }
        ApiError::ApiError(error) => map_response_error(operation, error),
        ApiError::ClientError(error) => transport_error(operation, error),
    }
}

fn map_no_fail_error(
    operation: &'static str,
    error: ApiErrorNoFail<HttpClientError>,
) -> GatewayError {
    match error {
        ApiErrorNoFail::ApiError(error) => map_response_error(operation, error),
        ApiErrorNoFail::ClientError(error) => transport_error(operation, error),
    }
}

fn map_login_error<F: std::fmt::Debug>(error: ApiError<F, HttpClientError>) -> GatewayError {
    map_api_error("account_login_url", error)
}

fn map_response_error(operation: &'static str, error: ApiResponseError) -> GatewayError {
    let code = match error {
        ApiResponseError::Auth(AuthError::InvalidAgentKey | AuthError::NoLongerValid) => {
            GatewayErrorCode::InvalidSecret
        }
        ApiResponseError::Auth(_) => GatewayErrorCode::Authentication,
        _ => GatewayErrorCode::Rejected,
    };
    GatewayError::new(code, operation, format!("{error:?}"))
}

fn transport_error(operation: &'static str, error: HttpClientError) -> GatewayError {
    GatewayError::new(GatewayErrorCode::Transport, operation, format!("{error:?}"))
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use playit_api_client::api::{AgentTunnelAttr, AgentTunnelConfig};

    fn tunnel(local_port: &str) -> AgentTunnelV1 {
        AgentTunnelV1 {
            id: uuid::Uuid::nil(),
            internal_id: 7,
            name: "fixture".to_owned(),
            display_address: "fixture.playit.gg:25565".to_owned(),
            port_type: PortType::Tcp,
            port_count: 0,
            tunnel_type: None,
            tunnel_type_display: "custom".to_owned(),
            agent_config: AgentTunnelConfig {
                fields: vec![
                    AgentTunnelAttr {
                        name: "local_ip".to_owned(),
                        value: "origin.internal".to_owned(),
                    },
                    AgentTunnelAttr {
                        name: "local_port".to_owned(),
                        value: local_port.to_owned(),
                    },
                ],
            },
            disabled_reason: None,
        }
    }

    #[test]
    fn adapter_converts_generated_tunnels_without_leaking_generated_types() {
        let converted = convert_tunnel(&tunnel("25565")).unwrap();
        assert_eq!(converted.origin.tunnel_id, 7);
        assert_eq!(converted.catalog.spec.port_count, NonZeroU16::MIN);
        assert!(matches!(
            converted.catalog.spec.target,
            OriginTarget::Port {
                host: OriginHost::Hostname(ref hostname),
                first_port,
            } if hostname.as_str() == "origin.internal" && first_port.get() == 25565
        ));
    }

    #[test]
    fn adapter_rejects_the_complete_catalog_when_one_tunnel_is_invalid() {
        let mut tunnel = tunnel("invalid");
        tunnel.display_address = "invalid".to_owned();
        let error = convert_tunnel(&tunnel).unwrap_err();
        assert_eq!(error.code, GatewayErrorCode::InvalidRunData);
        assert_eq!(error.problem_code(), ProblemCode::CatalogInvalid);
    }

    #[tokio::test]
    async fn adapter_login_cache_expires_at_the_contract_deadline() {
        let cache = LoginUrlCache::default();
        cache.store("fixture".to_owned(), 1_000).await;

        assert_eq!(
            cache.get(1_000 + LOGIN_CACHE_MILLIS - 1).await,
            Some("fixture".to_owned())
        );
        assert_eq!(cache.get(1_000 + LOGIN_CACHE_MILLIS).await, None);
    }
}
