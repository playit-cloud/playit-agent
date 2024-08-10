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
	pub async fn tunnels_create(&self, req: ReqTunnelsCreate) -> Result<ObjectId, ApiError<TunnelCreateError, C::Error>> {
		Self::unwrap(self.client.call("/tunnels/create", req).await)
	}
	pub async fn tunnels_delete(&self, req: ReqTunnelsDelete) -> Result<(), ApiError<DeleteError, C::Error>> {
		Self::unwrap(self.client.call("/tunnels/delete", req).await)
	}
	pub async fn claim_details(&self, req: ReqClaimDetails) -> Result<AgentClaimDetails, ApiError<ClaimDetailsError, C::Error>> {
		Self::unwrap(self.client.call("/claim/details", req).await)
	}
	pub async fn claim_setup(&self, req: ReqClaimSetup) -> Result<ClaimSetupResponse, ApiError<ClaimSetupError, C::Error>> {
		Self::unwrap(self.client.call("/claim/setup", req).await)
	}
	pub async fn claim_exchange(&self, req: ReqClaimExchange) -> Result<AgentSecretKey, ApiError<ClaimExchangeError, C::Error>> {
		Self::unwrap(self.client.call("/claim/exchange", req).await)
	}
	pub async fn claim_accept(&self, req: ReqClaimAccept) -> Result<AgentAccepted, ApiError<ClaimAcceptError, C::Error>> {
		Self::unwrap(self.client.call("/claim/accept", req).await)
	}
	pub async fn claim_reject(&self, req: ReqClaimReject) -> Result<(), ApiError<ClaimRejectError, C::Error>> {
		Self::unwrap(self.client.call("/claim/reject", req).await)
	}
	pub async fn proto_register(&self, req: ReqProtoRegister) -> Result<SignedAgentKey, ApiErrorNoFail<C::Error>> {
		Self::unwrap_no_fail(self.client.call("/proto/register", req).await)
	}
	pub async fn login_guest(&self) -> Result<WebSession, ApiError<GuestLoginError, C::Error>> {
		Self::unwrap(self.client.call("/login/guest", ReqLoginGuest {}).await)
	}
	pub async fn agents_routing_get(&self, req: ReqAgentsRoutingGet) -> Result<AgentRouting, ApiError<AgentRoutingGetError, C::Error>> {
		Self::unwrap(self.client.call("/agents/routing/get", req).await)
	}
	pub async fn agents_rundata(&self) -> Result<AgentRunData, ApiErrorNoFail<C::Error>> {
		Self::unwrap_no_fail(self.client.call("/agents/rundata", ReqAgentsRundata {}).await)
	}
	pub async fn tunnels_list_json(&self, req: ReqTunnelsList) -> Result<serde_json::Value, ApiErrorNoFail<C::Error>> {
		Self::unwrap_no_fail(self.client.call("/tunnels/list", req).await)
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

#[derive(Debug)]
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


#[derive(Debug)]
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

fn ok_or_default<'a, T, D>(deserializer: D) -> Result<T, D::Error>
    where T: serde::Deserialize<'a> + Default,
          D: serde::Deserializer<'a>
{
    match T::deserialize(deserializer) {
        Ok(v) => Ok(v),
        Err(_) => Ok(T::default())
    }
}



pub trait PlayitHttpClient {
    type Error;

    fn call<Req: serde::Serialize + std::marker::Send, Res: serde::de::DeserializeOwned, Err: serde::de::DeserializeOwned>(&self, path: &str, req: Req) -> impl std::future::Future<Output = Result<ApiResult<Res, Err>, Self::Error>>;
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
	Internal,
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
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentVersion {
	pub platform: Platform,
	pub version: String,
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
	pub auth: WebAuth,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct WebAuth {
	pub update_version: u32,
	pub account_id: u64,
	pub timestamp: u64,
	pub account_status: AccountStatus,
	pub totp_status: TotpStatus,
	pub admin_id: Option<u64>,
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
	#[serde(deserialize_with = "ok_or_default")]
	pub routing_target: Option<AgentRoutingTarget>,
	pub targets4: Vec<std::net::Ipv4Addr>,
	pub targets6: Vec<std::net::Ipv6Addr>,
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
pub enum AgentRoutingGetError {
	MissingAgentId,
	AgentIdNotSupported,
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
	pub ping_experiments: Vec<PingExperimentRequest>,
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
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PingExperimentRequest {
	pub id: u64,
	pub taret_ip: std::net::IpAddr,
	pub target_port: u16,
	pub alt_recv_ip: Option<std::net::IpAddr>,
	pub frequency_ms: u64,
	pub run_condition: Option<PingExperimentCondition>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PingExperimentCondition {
	#[serde(deserialize_with = "ok_or_default")]
	pub routing_target: Option<AgentRoutingTarget>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsList {
	pub tunnel_id: Option<uuid::Uuid>,
	pub agent_id: Option<uuid::Uuid>,
}

