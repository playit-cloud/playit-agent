#![cfg_attr(rustfmt, rustfmt_skip)]
impl<C: PlayitHttpClient> PlayitApiClient<C> {
	pub fn new(client: C) -> Self {
		PlayitApiClient { client }
	}
	pub fn get_client(&self) -> &C {
		&self.client
	}
	fn unwrap<S, F>(res: Result<ApiResult<S, F>, C::Error>) -> Result<S, ApiError<F, C::Error>> {
		match res {
			Ok(ApiResult::Success(v)) => Ok(v),
			Ok(ApiResult::Fail(fail)) => Err(ApiError::Fail(fail)),
			Ok(ApiResult::Error(error)) => Err(ApiError::ApiError(error)),
			Err(error) => Err(ApiError::ClientError(error)),
		}
	}
	fn unwrap_no_fail<S>(res: Result<ApiResult<S, ()>, C::Error>) -> Result<S, ApiErrorNoFail<C::Error>> {
		match res {
			Ok(ApiResult::Success(v)) => Ok(v),
			Ok(ApiResult::Fail(_)) => panic!(),
			Ok(ApiResult::Error(error)) => Err(ApiErrorNoFail::ApiError(error)),
			Err(error) => Err(ApiErrorNoFail::ClientError(error)),
		}
	}
	#[track_caller]
	pub fn login_guest(&self) -> impl std::future::Future<Output = Result<WebSession, ApiError<GuestLoginError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/login/guest", ReqLoginGuest {}).await)
		}
	}
	#[track_caller]
	pub fn tunnels_create(&self, req: ReqTunnelsCreate) -> impl std::future::Future<Output = Result<ObjectId, ApiError<TunnelCreateError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/tunnels/create", req).await)
		}
	}
	#[track_caller]
	pub fn tunnels_list(&self, req: ReqTunnelsList) -> impl std::future::Future<Output = Result<AccountTunnels, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/tunnels/list", req).await)
		}
	}
	#[track_caller]
	pub fn tunnels_update(&self, req: ReqTunnelsUpdate) -> impl std::future::Future<Output = Result<(), ApiError<UpdateError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/tunnels/update", req).await)
		}
	}
	#[track_caller]
	pub fn tunnels_delete(&self, req: ReqTunnelsDelete) -> impl std::future::Future<Output = Result<(), ApiError<DeleteError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/tunnels/delete", req).await)
		}
	}
	#[track_caller]
	pub fn tunnels_rename(&self, req: ReqTunnelsRename) -> impl std::future::Future<Output = Result<(), ApiError<TunnelRenameError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/tunnels/rename", req).await)
		}
	}
	#[track_caller]
	pub fn tunnels_firewall_assign(&self, req: ReqTunnelsFirewallAssign) -> impl std::future::Future<Output = Result<(), ApiError<TunnelsFirewallAssignError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/tunnels/firewall/assign", req).await)
		}
	}
	#[track_caller]
	pub fn tunnels_ratelimit(&self, req: ReqTunnelsRatelimit) -> impl std::future::Future<Output = Result<(), ApiError<TunnelRatelimitError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/tunnels/ratelimit", req).await)
		}
	}
	#[track_caller]
	pub fn tunnels_enable(&self, req: ReqTunnelsEnable) -> impl std::future::Future<Output = Result<(), ApiError<TunnelEnableError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/tunnels/enable", req).await)
		}
	}
	#[track_caller]
	pub fn tunnels_proxy_set(&self, req: ReqTunnelsProxySet) -> impl std::future::Future<Output = Result<(), ApiError<TunnelProxySetError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/tunnels/proxy/set", req).await)
		}
	}
	#[track_caller]
	pub fn tunnels_config(&self, req: ReqTunnelsConfig) -> impl std::future::Future<Output = Result<(), ApiError<TunnelConfigError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/tunnels/config", req).await)
		}
	}
	#[track_caller]
	pub fn agents_rename(&self, req: ReqAgentsRename) -> impl std::future::Future<Output = Result<(), ApiError<AgentRenameError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/agents/rename", req).await)
		}
	}
	#[track_caller]
	pub fn agents_routing_set(&self, req: ReqAgentsRoutingSet) -> impl std::future::Future<Output = Result<(), ApiError<AgentRoutingSetError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/agents/routing/set", req).await)
		}
	}
	#[track_caller]
	pub fn agents_routing_get(&self, req: ReqAgentsRoutingGet) -> impl std::future::Future<Output = Result<AgentRouting, ApiError<AgentRoutingGetError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/agents/routing/get", req).await)
		}
	}
	#[track_caller]
	pub fn agents_rundata(&self) -> impl std::future::Future<Output = Result<AgentRunData, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/agents/rundata", ReqAgentsRundata {}).await)
		}
	}
	#[track_caller]
	pub fn domains_list(&self) -> impl std::future::Future<Output = Result<Domains, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/domains/list", ReqDomainsList {}).await)
		}
	}
	#[track_caller]
	pub fn proto_register(&self, req: ReqProtoRegister) -> impl std::future::Future<Output = Result<SignedAgentKey, ApiError<ProtoRegisterError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/proto/register", req).await)
		}
	}
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "message")]
pub enum ApiResponseError {
	#[serde(rename = "validation")]
	Validation(String),
	#[serde(rename = "path-not-found")]
	PathNotFound(PathNotFound),
	#[serde(rename = "auth")]
	Auth(AuthError),
	#[serde(rename = "internal")]
	Internal(ApiInternalError),
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PathNotFound {
	pub path: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum AuthError {
	AuthRequired,
	InvalidHeader,
	InvalidSignature,
	InvalidTimestamp,
	InvalidApiKey,
	InvalidAgentKey,
	SessionExpired,
	InvalidAuthType,
	ScopeNotAllowed,
	NoLongerValid,
	GuestAccountNotAllowed,
	EmailMustBeVerified,
	AccountDoesNotExist,
	AdminOnly,
	InvalidToken,
	TotpRequred,
	NotAllowedWithReadOnly,
	DefaultAgentBlocked,
	AgentNotSelfManaged,
	SelfManagedAgentCanOnlyAffectSelf,
	AccountNotAuthorized,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ApiInternalError {
	pub trace_id: String,
}

impl std::fmt::Display for ApiResponseError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for ApiResponseError {
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(tag = "status", content = "data")]
pub enum ApiResult<S, F> {
    #[serde(rename = "success")]
    Success(S),
    #[serde(rename = "fail")]
    Fail(F),
    #[serde(rename = "error")]
    Error(ApiResponseError),
}

#[derive(Debug, serde::Serialize)]
pub enum ApiError<F, C> {
    Fail(F),
    ApiError(ApiResponseError),
    ClientError(C),
}

impl<F: std::fmt::Debug, C: std::fmt::Debug> std::fmt::Display for ApiError<F, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl<F: std::fmt::Debug, C: std::fmt::Debug> std::error::Error for ApiError<F, C> {
}


#[derive(Debug, serde::Serialize)]
pub enum ApiErrorNoFail<C> {
    ApiError(ApiResponseError),
    ClientError(C),
}

impl<C: std::fmt::Debug> std::fmt::Display for ApiErrorNoFail<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl<C: std::fmt::Debug> std::error::Error for ApiErrorNoFail<C> {
}



pub trait PlayitHttpClient {
    type Error;

    fn call<Req: serde::Serialize + std::marker::Send, Res: serde::de::DeserializeOwned, Err: serde::de::DeserializeOwned>(&self, caller: &'static std::panic::Location<'static>, path: &str, req: Req) -> impl std::future::Future<Output = Result<ApiResult<Res, Err>, Self::Error>>;
}

#[derive(Clone)]
pub struct PlayitApiClient<C: PlayitHttpClient> {
    client: C,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqLoginGuest {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct WebSession {
	pub session_key: String,
	pub auth: WebAuthToken,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct WebAuthToken {
	pub update_version: u32,
	pub account_id: u64,
	pub timestamp: u64,
	pub account_status: AccountStatus,
	pub totp_status: TotpStatus,
	pub admin_id: Option<std::num::NonZeroU64>,
	pub read_only: bool,
	pub show_admin: bool,
}



#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum AccountStatus {
	#[serde(rename = "guest")]
	Guest,
	#[serde(rename = "email-not-verified")]
	EmailNotVerified,
	#[serde(rename = "verified")]
	Verified,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "status")]
pub enum TotpStatus {
	#[serde(rename = "required")]
	Required,
	#[serde(rename = "not-setup")]
	NotSetup,
	#[serde(rename = "signed")]
	Signed(SignedEpoch),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct SignedEpoch {
	pub epoch_sec: u32,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum GuestLoginError {
	AccountIsNotGuest,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsCreate {
	pub name: Option<String>,
	pub tunnel_type: Option<TunnelType>,
	pub port_type: PortType,
	pub port_count: u16,
	pub origin: TunnelOriginCreate,
	pub enabled: bool,
	pub alloc: Option<TunnelCreateUseAllocation>,
	pub firewall_id: Option<uuid::Uuid>,
	pub proxy_protocol: Option<ProxyProtocol>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum TunnelType {
	#[serde(rename = "minecraft-java")]
	MinecraftJava,
	#[serde(rename = "minecraft-bedrock")]
	MinecraftBedrock,
	#[serde(rename = "valheim")]
	Valheim,
	#[serde(rename = "terraria")]
	Terraria,
	#[serde(rename = "starbound")]
	Starbound,
	#[serde(rename = "rust")]
	Rust,
	#[serde(rename = "7days")]
	Num7days,
	#[serde(rename = "unturned")]
	Unturned,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum PortType {
	#[serde(rename = "tcp")]
	Tcp,
	#[serde(rename = "udp")]
	Udp,
	#[serde(rename = "both")]
	Both,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "data")]
pub enum TunnelOriginCreate {
	#[serde(rename = "default")]
	Default(AssignedDefaultCreate),
	#[serde(rename = "agent")]
	Agent(AssignedAgentCreate),
	#[serde(rename = "managed")]
	Managed(AssignedManagedCreate),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AssignedDefaultCreate {
	pub local_ip: std::net::IpAddr,
	pub local_port: Option<u16>,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AssignedAgentCreate {
	pub agent_id: uuid::Uuid,
	pub local_ip: std::net::IpAddr,
	pub local_port: Option<u16>,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AssignedManagedCreate {
	pub agent_id: Option<uuid::Uuid>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "details")]
pub enum TunnelCreateUseAllocation {
	#[serde(rename = "dedicated-ip")]
	DedicatedIp(UseAllocDedicatedIp),
	#[serde(rename = "port-allocation")]
	PortAllocation(UseAllocPortAlloc),
	#[serde(rename = "region")]
	Region(UseRegion),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct UseAllocDedicatedIp {
	pub ip_hostname: String,
	pub port: Option<u16>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct UseAllocPortAlloc {
	pub alloc_id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct UseRegion {
	pub region: AllocationRegion,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum AllocationRegion {
	#[serde(rename = "smart-global")]
	SmartGlobal,
	#[serde(rename = "global")]
	Global,
	#[serde(rename = "north-america")]
	NorthAmerica,
	#[serde(rename = "europe")]
	Europe,
	#[serde(rename = "asia")]
	Asia,
	#[serde(rename = "india")]
	India,
	#[serde(rename = "south-america")]
	SouthAmerica,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum ProxyProtocol {
	#[serde(rename = "proxy-protocol-v1")]
	ProxyProtocolV1,
	#[serde(rename = "proxy-protocol-v2")]
	ProxyProtocolV2,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ObjectId {
	pub id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum TunnelCreateError {
	AgentIdRequired,
	AgentNotFound,
	InvalidAgentId,
	DedicatedIpNotFound,
	DedicatedIpPortNotAvailable,
	DedicatedIpNotEnoughSpace,
	PortAllocNotFound,
	InvalidIpHostname,
	ManagedMissingAgentId,
	InvalidPortCount,
	RequiresVerifiedAccount,
}

impl std::fmt::Display for TunnelCreateError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for TunnelCreateError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsList {
	pub tunnel_id: Option<uuid::Uuid>,
	pub agent_id: Option<uuid::Uuid>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AccountTunnels {
	pub tcp_alloc: AllocatedPorts,
	pub udp_alloc: AllocatedPorts,
	pub tunnels: Vec<AccountTunnel>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AllocatedPorts {
	pub allowed: u32,
	pub claimed: u32,
	pub desired: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AccountTunnel {
	pub id: uuid::Uuid,
	pub tunnel_type: Option<TunnelType>,
	pub created_at: chrono::DateTime<chrono::Utc>,
	pub name: Option<String>,
	pub port_type: PortType,
	pub port_count: u16,
	pub alloc: AccountTunnelAllocation,
	pub origin: Option<TunnelOrigin>,
	pub domain: Option<TunnelDomain>,
	pub firewall_id: Option<uuid::Uuid>,
	pub ratelimit: Ratelimit,
	pub active: bool,
	pub disabled_reason: Option<TunnelDisabledReason>,
	pub region: Option<AllocationRegion>,
	pub expire_notice: Option<TunnelExpireNotice>,
	pub proxy_protocol: Option<ProxyProtocol>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "status", content = "data")]
pub enum AccountTunnelAllocation {
	#[serde(rename = "pending")]
	Pending,
	#[serde(rename = "disabled")]
	Disabled(TunnelDisabled),
	#[serde(rename = "allocated")]
	Allocated(TunnelAllocated),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TunnelDisabled {
	pub reason: TunnelDisabledReason,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum TunnelDisabledReason {
	#[serde(rename = "requires-premium")]
	RequiresPremium,
	#[serde(rename = "over-port-limit")]
	OverPortLimit,
	#[serde(rename = "ip-used-in-gre")]
	IpUsedInGre,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TunnelAllocated {
	pub id: uuid::Uuid,
	pub ip_hostname: String,
	pub static_ip4: Option<std::net::Ipv4Addr>,
	pub static_ip6: std::net::Ipv6Addr,
	pub assigned_domain: String,
	pub assigned_srv: Option<String>,
	pub tunnel_ip: std::net::IpAddr,
	pub port_start: u16,
	pub port_end: u16,
	pub assignment: TunnelAssignment,
	pub ip_type: IpType,
	pub region: AllocationRegion,
}



#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "subscription")]
pub enum TunnelAssignment {
	#[serde(rename = "dedicated-ip")]
	DedicatedIp(TunnelDedicatedIp),
	#[serde(rename = "shared-ip")]
	SharedIp,
	#[serde(rename = "dedicated-port")]
	DedicatedPort(SubscriptionId),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TunnelDedicatedIp {
	pub sub_id: uuid::Uuid,
	pub region: AllocationRegion,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct SubscriptionId {
	pub sub_id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum IpType {
	#[serde(rename = "both")]
	Both,
	#[serde(rename = "ip4")]
	Ip4,
	#[serde(rename = "ip6")]
	Ip6,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "data")]
pub enum TunnelOrigin {
	#[serde(rename = "agent")]
	Agent(AssignedAgent),
	#[serde(rename = "managed")]
	Managed(AssignedManaged),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AssignedAgent {
	pub agent_id: uuid::Uuid,
	pub agent_name: String,
	pub local_ip: std::net::IpAddr,
	pub local_port: Option<u16>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AssignedManaged {
	pub agent_id: uuid::Uuid,
	pub agent_name: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TunnelDomain {
	pub id: uuid::Uuid,
	pub name: String,
	pub is_external: bool,
	pub parent: Option<uuid::Uuid>,
	pub source: TunnelDomainSource,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum TunnelDomainSource {
	#[serde(rename = "from-ip")]
	FromIp,
	#[serde(rename = "from-tunnel")]
	FromTunnel,
	#[serde(rename = "from-agent-ip")]
	FromAgentIp,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Ratelimit {
	pub bytes_per_second: Option<u32>,
	pub packets_per_second: Option<u32>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TunnelExpireNotice {
	pub disable_at: chrono::DateTime<chrono::Utc>,
	pub remove_at: chrono::DateTime<chrono::Utc>,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsUpdate {
	pub tunnel_id: uuid::Uuid,
	pub local_ip: std::net::IpAddr,
	pub local_port: Option<u16>,
	pub agent_id: Option<uuid::Uuid>,
	pub enabled: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum UpdateError {
	ChangingAgentIdNotAllowed,
	TunnelNotFound,
}

impl std::fmt::Display for UpdateError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for UpdateError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsDelete {
	pub tunnel_id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum DeleteError {
	TunnelNotFound,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsRename {
	pub tunnel_id: uuid::Uuid,
	pub name: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum TunnelRenameError {
	TunnelNotFound,
	NameTooLong,
}

impl std::fmt::Display for TunnelRenameError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for TunnelRenameError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsFirewallAssign {
	pub tunnel_id: uuid::Uuid,
	pub firewall_id: Option<uuid::Uuid>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum TunnelsFirewallAssignError {
	TunnelNotFound,
	InvalidFirewallId,
}

impl std::fmt::Display for TunnelsFirewallAssignError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for TunnelsFirewallAssignError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsRatelimit {
	pub tunnel_id: uuid::Uuid,
	pub bytes_per_second: Option<u32>,
	pub packets_per_second: Option<u32>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum TunnelRatelimitError {
	TunnelNotFound,
	InvalidRatelimit,
	PlayitPremiumRequired,
}

impl std::fmt::Display for TunnelRatelimitError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for TunnelRatelimitError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsEnable {
	pub tunnel_id: uuid::Uuid,
	pub enabled: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum TunnelEnableError {
	TunnelNotFound,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsProxySet {
	pub tunnel_id: uuid::Uuid,
	pub proxy_protocol: Option<ProxyProtocol>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum TunnelProxySetError {
	TunnelNotFound,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsConfig {
	pub tunnel_id: uuid::Uuid,
	pub new_agent_id: Option<uuid::Uuid>,
	pub new_config: Option<AgentTunnelConfig>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentTunnelConfig {
	pub fields: Vec<AgentTunnelAttr>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentTunnelAttr {
	pub name: String,
	pub value: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "error", content = "details")]
pub enum TunnelConfigError {
	#[serde(rename = "TunnelNotFound")]
	TunnelNotFound,
	#[serde(rename = "AgentNotFound")]
	AgentNotFound,
	#[serde(rename = "AgentVersionUnknown")]
	AgentVersionUnknown,
	#[serde(rename = "CannotConfigTunnelWithoutAgent")]
	CannotConfigTunnelWithoutAgent,
	#[serde(rename = "SelfManagedAgentCannotReassignTunnel")]
	SelfManagedAgentCannotReassignTunnel,
	#[serde(rename = "InvalidConfig")]
	InvalidConfig(AgentSchemaValidationError),
	#[serde(rename = "ConfigNotCompatibleWithAgent")]
	ConfigNotCompatibleWithAgent,
	#[serde(rename = "NothingToUpdate")]
	NothingToUpdate,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "error", content = "field")]
pub enum AgentSchemaValidationError {
	#[serde(rename = "NoSchemaFound")]
	NoSchemaFound,
	#[serde(rename = "TooManyFields")]
	TooManyFields,
	#[serde(rename = "TunnelTypeNotSupported")]
	TunnelTypeNotSupported(AgentTunnelTypeDetails),
	#[serde(rename = "UnknownField")]
	UnknownField(String),
	#[serde(rename = "MissingRequiredField")]
	MissingRequiredField(String),
	#[serde(rename = "InvalidValueForType")]
	InvalidValueForType(String),
	#[serde(rename = "ValueNotInVariants")]
	ValueNotInVariants(String),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentTunnelTypeDetails {
	pub tunnel_type: Option<TunnelType>,
	pub port_type: PortType,
	pub port_count: u16,
}

impl std::fmt::Display for TunnelConfigError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for TunnelConfigError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqAgentsRename {
	pub agent_id: uuid::Uuid,
	pub name: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum AgentRenameError {
	AgentNotFound,
	InvalidName,
	InvalidAgentId,
}

impl std::fmt::Display for AgentRenameError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for AgentRenameError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqAgentsRoutingSet {
	pub agent_id: uuid::Uuid,
	pub routing: AgentRoutingTarget,
	pub disable_ip6: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "details")]
pub enum AgentRoutingTarget {
	#[serde(rename = "Automatic")]
	Automatic,
	#[serde(rename = "Pop")]
	Pop(PlayitPop),
	#[serde(rename = "Region")]
	Region(PlayitRegion),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum PlayitPop {
	Any,
	#[serde(rename = "USLosAngeles")]
	UsLosAngeles,
	#[serde(rename = "USSeattle")]
	UsSeattle,
	#[serde(rename = "USDallas")]
	UsDallas,
	#[serde(rename = "USMiami")]
	UsMiami,
	#[serde(rename = "USChicago")]
	UsChicago,
	#[serde(rename = "USNewJersey")]
	UsNewJersey,
	CanadaToronto,
	Mexico,
	BrazilSaoPaulo,
	Spain,
	London,
	Germany,
	Poland,
	Sweden,
	IndiaDelhi,
	IndiaMumbai,
	IndiaBangalore,
	Singapore,
	Tokyo,
	Sydney,
	SantiagoChile,
	Israel,
	Romania,
	#[serde(rename = "USNewYork")]
	UsNewYork,
	#[serde(rename = "USDenver")]
	UsDenver,
	Staging,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum PlayitRegion {
	GlobalAnycast,
	NorthAmerica,
	Europe,
	Asia,
	India,
	SouthAmerica,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum AgentRoutingSetError {
	RequiresPremium,
	AgentNotFound,
	InvalidAgentId,
}

impl std::fmt::Display for AgentRoutingSetError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for AgentRoutingSetError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqAgentsRoutingGet {
	pub agent_id: Option<uuid::Uuid>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentRouting {
	pub agent_id: uuid::Uuid,
	pub targets4: Vec<std::net::Ipv4Addr>,
	pub targets6: Vec<std::net::Ipv6Addr>,
	pub disable_ip6: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum AgentRoutingGetError {
	MissingAgentId,
	InvalidAgentId,
}

impl std::fmt::Display for AgentRoutingGetError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for AgentRoutingGetError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqAgentsRundata {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentRunData {
	pub agent_id: uuid::Uuid,
	pub agent_type: AgentType,
	pub account_status: AgentAccountStatus,
	pub tunnels: Vec<AgentTunnel>,
	pub pending: Vec<AgentPendingTunnel>,
	pub account_features: AccountFeatures,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum AgentType {
	#[serde(rename = "default")]
	Default,
	#[serde(rename = "assignable")]
	Assignable,
	#[serde(rename = "self-managed")]
	SelfManaged,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum AgentAccountStatus {
	#[serde(rename = "account-delete-scheduled")]
	AccountDeleteScheduled,
	#[serde(rename = "banned")]
	Banned,
	#[serde(rename = "has-message")]
	HasMessage,
	#[serde(rename = "email-not-verified")]
	EmailNotVerified,
	#[serde(rename = "guest")]
	Guest,
	#[serde(rename = "ready")]
	Ready,
	#[serde(rename = "agent-over-limit")]
	AgentOverLimit,
	#[serde(rename = "agent-disabled")]
	AgentDisabled,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentTunnel {
	pub id: uuid::Uuid,
	pub internal_id: u64,
	pub name: Option<String>,
	pub ip_num: u64,
	pub region_num: u16,
	pub port: PortRange,
	pub proto: PortType,
	pub local_ip: std::net::IpAddr,
	pub local_port: u16,
	pub tunnel_type: Option<String>,
	pub assigned_domain: String,
	pub custom_domain: Option<String>,
	pub disabled: Option<AgentTunnelDisabled>,
	pub proxy_protocol: Option<ProxyProtocol>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PortRange {
	pub from: u16,
	pub to: u16,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum AgentTunnelDisabled {
	ByUser,
	BySystem,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentPendingTunnel {
	pub id: uuid::Uuid,
	pub name: Option<String>,
	pub proto: PortType,
	pub port_count: u16,
	pub tunnel_type: Option<String>,
	pub is_disabled: bool,
	pub region_num: u16,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AccountFeatures {
	pub regional_tunnels: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqDomainsList {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Domains {
	pub domains: Vec<Domain>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Domain {
	pub id: uuid::Uuid,
	pub name: String,
	pub is_external: bool,
	pub parent: Option<uuid::Uuid>,
	pub sub_id: uuid::Uuid,
	pub target: Option<DomainTarget>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "details")]
pub enum DomainTarget {
	#[serde(rename = "ip-address")]
	IpAddress(DomainTargetIp),
	#[serde(rename = "tunnel")]
	Tunnel(DomainTargetTunnel),
	#[serde(rename = "external-cname")]
	ExternalCname(DomainTargetExternalCName),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct DomainTargetIp {
	pub ip_address: std::net::IpAddr,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct DomainTargetTunnel {
	pub tunnel_id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct DomainTargetExternalCName {
	pub cname: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqProtoRegister {
	pub agent_version: Option<PlayitAgentVersion>,
	pub proto_version: u64,
	pub version: AgentVersion,
	pub platform: Platform,
	pub client_addr: std::net::SocketAddr,
	pub tunnel_addr: std::net::SocketAddr,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PlayitAgentVersion {
	pub version: AgentVersionOld,
	pub proto_version: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentVersionOld {
	pub platform: Platform,
	pub version: String,
	pub has_expired: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum Platform {
	#[serde(rename = "linux")]
	Linux,
	#[serde(rename = "freebsd")]
	Freebsd,
	#[serde(rename = "windows")]
	Windows,
	#[serde(rename = "macos")]
	Macos,
	#[serde(rename = "android")]
	Android,
	#[serde(rename = "ios")]
	Ios,
	#[serde(rename = "docker")]
	Docker,
	#[serde(rename = "minecraft-plugin")]
	MinecraftPlugin,
	#[serde(rename = "unknown")]
	Unknown,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentVersion {
	pub variant_id: uuid::Uuid,
	pub version_major: u32,
	pub version_minor: u32,
	pub version_patch: u32,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct SignedAgentKey {
	pub key: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum ProtoRegisterError {
	UnknownPlayitVersion,
	DisabledByUser,
	AgentDisabledOverLimit,
	AccountBanned,
}

impl std::fmt::Display for ProtoRegisterError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for ProtoRegisterError {
}
