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
	pub fn tunnels_create(&self, req: ReqTunnelsCreate) -> impl std::future::Future<Output = Result<ObjectId, ApiError<TunnelCreateError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/tunnels/create", req).await)
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
	pub fn claim_details(&self, req: ReqClaimDetails) -> impl std::future::Future<Output = Result<AgentClaimDetails, ApiError<ClaimDetailsError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/claim/details", req).await)
		}
	}
	#[track_caller]
	pub fn claim_setup(&self, req: ReqClaimSetup) -> impl std::future::Future<Output = Result<ClaimSetupResponse, ApiError<ClaimSetupError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/claim/setup", req).await)
		}
	}
	#[track_caller]
	pub fn claim_exchange(&self, req: ReqClaimExchange) -> impl std::future::Future<Output = Result<AgentSecretKey, ApiError<ClaimExchangeError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/claim/exchange", req).await)
		}
	}
	#[track_caller]
	pub fn claim_accept(&self, req: ReqClaimAccept) -> impl std::future::Future<Output = Result<AgentAccepted, ApiError<ClaimAcceptError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/claim/accept", req).await)
		}
	}
	#[track_caller]
	pub fn claim_reject(&self, req: ReqClaimReject) -> impl std::future::Future<Output = Result<(), ApiError<ClaimRejectError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/claim/reject", req).await)
		}
	}
	#[track_caller]
	pub fn proto_register(&self, req: ReqProtoRegister) -> impl std::future::Future<Output = Result<SignedAgentKey, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/proto/register", req).await)
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
	pub fn ping_submit(&self, req: ReqPingSubmit) -> impl std::future::Future<Output = Result<(), ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/ping/submit", req).await)
		}
	}
	#[track_caller]
	pub fn ping_get(&self) -> impl std::future::Future<Output = Result<PingExperiments, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/ping/get", ReqPingGet {}).await)
		}
	}
	#[track_caller]
	pub fn tunnels_list_json(&self, req: ReqTunnelsList) -> impl std::future::Future<Output = Result<serde_json::Value, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/tunnels/list", req).await)
		}
	}
	#[track_caller]
	pub fn agents_list_json(&self) -> impl std::future::Future<Output = Result<serde_json::Value, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/agents/list", ReqAgentsList {}).await)
		}
	}
	#[track_caller]
	pub fn query_region(&self, req: ReqQueryRegion) -> impl std::future::Future<Output = Result<QueryRegion, ApiError<QueryRegionError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/query/region", req).await)
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
	pub fn tunnels_firewall_assign(&self, req: ReqTunnelsFirewallAssign) -> impl std::future::Future<Output = Result<(), ApiError<TunnelsFirewallAssignError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/tunnels/firewall/assign", req).await)
		}
	}
	#[track_caller]
	pub fn tunnels_proxy_set(&self, req: ReqTunnelsProxySet) -> impl std::future::Future<Output = Result<(), ApiError<TunnelProxySetError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/tunnels/proxy/set", req).await)
		}
	}
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
	AgentNotSelfManaged,
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
pub struct ReqTunnelsDelete {
	pub tunnel_id: uuid::Uuid,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum DeleteError {
	TunnelNotFound,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqClaimDetails {
	pub code: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentClaimDetails {
	pub name: String,
	pub remote_ip: std::net::IpAddr,
	pub agent_type: AgentType,
	pub version: String,
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
pub enum ClaimDetailsError {
	AlreadyClaimed,
	AlreadyRejected,
	ClaimExpired,
	DifferentOwner,
	WaitingForAgent,
	InvalidCode,
}

impl std::fmt::Display for ClaimDetailsError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for ClaimDetailsError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqClaimSetup {
	pub code: String,
	pub agent_type: AgentType,
	pub version: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum ClaimSetupResponse {
	WaitingForUserVisit,
	WaitingForUser,
	UserAccepted,
	UserRejected,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum ClaimSetupError {
	InvalidCode,
	CodeExpired,
	VersionTextTooLong,
}

impl std::fmt::Display for ClaimSetupError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for ClaimSetupError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqClaimExchange {
	pub code: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentSecretKey {
	pub secret_key: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum ClaimExchangeError {
	CodeNotFound,
	CodeExpired,
	UserRejected,
	NotAccepted,
	NotSetup,
}

impl std::fmt::Display for ClaimExchangeError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for ClaimExchangeError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqClaimAccept {
	pub code: String,
	pub name: String,
	pub agent_type: AgentType,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentAccepted {
	pub agent_id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum ClaimAcceptError {
	InvalidCode,
	AgentNotReady,
	CodeNotFound,
	InvalidAgentType,
	ClaimAlreadyAccepted,
	ClaimRejected,
	CodeExpired,
	InvalidName,
}

impl std::fmt::Display for ClaimAcceptError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for ClaimAcceptError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqClaimReject {
	pub code: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum ClaimRejectError {
	InvalidCode,
	CodeNotFound,
	ClaimAccepted,
	ClaimAlreadyRejected,
}

impl std::fmt::Display for ClaimRejectError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for ClaimRejectError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqProtoRegister {
	pub agent_version: PlayitAgentVersion,
	pub client_addr: std::net::SocketAddr,
	pub tunnel_addr: std::net::SocketAddr,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PlayitAgentVersion {
	pub version: AgentVersion,
	pub official: bool,
	pub details_website: Option<String>,
	pub proto_version: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentVersion {
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
pub struct SignedAgentKey {
	pub key: String,
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
pub struct ReqPingSubmit {
	pub results: Vec<PingExperimentResult>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PingExperimentResult {
	pub id: u64,
	pub target: PingTarget,
	pub samples: Vec<PingSample>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PingTarget {
	pub ip: std::net::IpAddr,
	pub port: u16,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PingSample {
	pub tunnel_server_id: u64,
	pub dc_id: u64,
	pub server_ts: u64,
	pub latency: u64,
	pub count: u16,
	pub num: u16,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqPingGet {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PingExperiments {
	pub experiments: Vec<PingExperimentDetails>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PingExperimentDetails {
	pub id: u64,
	pub test_interval: u64,
	pub ping_interval: u64,
	pub samples: u64,
	pub targets: std::borrow::Cow<'static,[PingTarget]>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsList {
	pub tunnel_id: Option<uuid::Uuid>,
	pub agent_id: Option<uuid::Uuid>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqAgentsList {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqQueryRegion {
	pub limit_region: Option<PlayitRegion>,
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

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct QueryRegion {
	pub region: PlayitRegion,
	pub pop: PlayitPop,
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
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum QueryRegionError {
	FailedToDetermineLocation,
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
pub struct ReqTunnelsProxySet {
	pub tunnel_id: uuid::Uuid,
	pub proxy_protocol: Option<ProxyProtocol>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum TunnelProxySetError {
	TunnelNotFound,
}

