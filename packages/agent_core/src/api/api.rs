impl<C: PlayitHttpClient> PlayitApiClient<C> {
    pub fn new(client: C) -> Self {
        PlayitApiClient { client }
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
    pub async fn tunnels_list(&self, req: ReqTunnelsList) -> Result<AccountTunnels, ApiErrorNoFail<C::Error>> {
        Self::unwrap_no_fail(self.client.call("/tunnels/list", req).await)
    }
    pub async fn tunnels_update(&self, req: ReqTunnelsUpdate) -> Result<(), ApiError<UpdateError, C::Error>> {
        Self::unwrap(self.client.call("/tunnels/update", req).await)
    }
    pub async fn tunnels_firewall_assign(&self, req: ReqTunnelsFirewallAssign) -> Result<(), ApiError<TunnelsFirewallAssignError, C::Error>> {
        Self::unwrap(self.client.call("/tunnels/firewall/assign", req).await)
    }
    pub async fn agents_list(&self) -> Result<Agents, ApiErrorNoFail<C::Error>> {
        Self::unwrap_no_fail(self.client.call("/agents/list", ReqAgentsList {}).await)
    }
    pub async fn agents_delete(&self, req: ReqAgentsDelete) -> Result<(), ApiError<AgentsDeleteError, C::Error>> {
        Self::unwrap(self.client.call("/agents/delete", req).await)
    }
    pub async fn agents_rename(&self, req: ReqAgentsRename) -> Result<(), ApiError<AgentRenameError, C::Error>> {
        Self::unwrap(self.client.call("/agents/rename", req).await)
    }
    pub async fn allocations_list(&self, req: ReqAllocationsList) -> Result<AccountAllocations, ApiErrorNoFail<C::Error>> {
        Self::unwrap_no_fail(self.client.call("/allocations/list", req).await)
    }
    pub async fn tunnels_rename(&self, req: ReqTunnelsRename) -> Result<(), ApiError<TunnelRenameError, C::Error>> {
        Self::unwrap(self.client.call("/tunnels/rename", req).await)
    }
    pub async fn firewalls_list(&self) -> Result<Firewalls, ApiErrorNoFail<C::Error>> {
        Self::unwrap_no_fail(self.client.call("/firewalls/list", ReqFirewallsList {}).await)
    }
    pub async fn firewalls_create(&self, req: ReqFirewallsCreate) -> Result<ObjectId, ApiError<FirewallsCreateError, C::Error>> {
        Self::unwrap(self.client.call("/firewalls/create", req).await)
    }
    pub async fn firewalls_update(&self, req: ReqFirewallsUpdate) -> Result<(), ApiError<FirewallsUpdateError, C::Error>> {
        Self::unwrap(self.client.call("/firewalls/update", req).await)
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
    pub async fn login_create_guest(&self) -> Result<WebSession, ApiError<LoginCreateGuestError, C::Error>> {
        Self::unwrap(self.client.call("/login/create/guest", ReqLoginCreateGuest {}).await)
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



#[async_trait::async_trait]
pub trait PlayitHttpClient {
    type Error;

    async fn call<Req: serde::Serialize + std::marker::Send, Res: serde::de::DeserializeOwned, Err: serde::de::DeserializeOwned>(&self, path: &str, req: Req) -> Result<ApiResult<Res, Err>, Self::Error>;
}

pub struct PlayitApiClient<C: PlayitHttpClient> {
    client: C,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "message")]
pub enum ApiResponseError {
    #[serde(rename = "validation")]
    Validation(String),
    #[serde(rename = "path-not-found")]
    PathNotFound,
    #[serde(rename = "auth")]
    Auth(AuthError),
    #[serde(rename = "internal")]
    Internal,
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
    pub origin: TunnelOrigin,
    pub domain: Option<TunnelDomain>,
    pub firewall_id: Option<uuid::Uuid>,
    pub ratelimit: Ratelimit,
    pub active: bool,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "status", content = "data")]
pub enum AccountTunnelAllocation {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "disabled")]
    Disabled,
    #[serde(rename = "allocated")]
    Allocated(TunnelAllocated),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TunnelAllocated {
    pub id: uuid::Uuid,
    pub ip_hostname: String,
    pub assigned_domain: String,
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
    DedicatedIp(SubscriptionId),
    #[serde(rename = "shared-ip")]
    SharedIp,
    #[serde(rename = "dedicated-port")]
    DedicatedPort(SubscriptionId),
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
#[serde(tag = "type", content = "data")]
pub enum TunnelOrigin {
    #[serde(rename = "default")]
    Default(AssignedDefault),
    #[serde(rename = "agent")]
    Agent(AssignedAgent),
    #[serde(rename = "managed")]
    Managed(AssignedManaged),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AssignedDefault {
    pub local_ip: std::net::IpAddr,
    pub local_port: Option<u16>,
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
pub struct ReqAgentsList {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Agents {
    pub agents: Vec<Agent>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Agent {
    pub id: uuid::Uuid,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub version: Option<String>,
    pub agent_type: AgentType,
    pub details: AgentStatus,
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

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "state", content = "data")]
pub enum AgentStatus {
    #[serde(rename = "disabled-above-limit")]
    DisabledAboveLimit,
    #[serde(rename = "disabled-by-user")]
    DisabledByUser,
    #[serde(rename = "approval-needed")]
    ApprovalNeeded,
    #[serde(rename = "connected")]
    Connected(AgentConnectedDetails),
    #[serde(rename = "offline")]
    Offline,
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentConnectedDetails {
    pub tunnel_server_id: u64,
    pub data_center_id: u32,
    pub data_center_name: String,
    pub agent_version: u64,
    pub client_addr: std::net::SocketAddr,
    pub tunnel_addr: std::net::SocketAddr,
    pub activity_latest_epoch_ms: u64,
    pub activity_start_epoch_ms: u64,
    pub tunnel_latency_ms: u64,
}



#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqAgentsDelete {
    pub agent_id: uuid::Uuid,
    pub tunnels_strategy: DeleteAgentTunnelStrategy,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "details")]
pub enum DeleteAgentTunnelStrategy {
    #[serde(rename = "require_empty")]
    RequireEmpty,
    #[serde(rename = "delete_tunnels")]
    DeleteTunnels,
    #[serde(rename = "move_to_agent")]
    MoveToAgent(TunnelStrategyMoveToAgent),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TunnelStrategyMoveToAgent {
    pub agent_id: Option<uuid::Uuid>,
    pub disable_tunnels: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum AgentsDeleteError {
    AgentNotFound,
    AgentNotAuthorized,
    TunnelStrategyNotAllowed,
    MoveToAgentNotFound,
    AgentHasExistingTunnels,
}

impl std::fmt::Display for AgentsDeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for AgentsDeleteError {
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
pub struct ReqAllocationsList {
    pub alloc_id: Option<uuid::Uuid>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AccountAllocations {
    pub ports: Vec<DedicatedPortAllocation>,
    pub ips: Vec<DedicatedIpAllocation>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct DedicatedPortAllocation {
    pub alloc_id: uuid::Uuid,
    pub ip_hostname: String,
    pub port: u16,
    pub port_count: u16,
    pub port_type: PortType,
    pub sub_id: uuid::Uuid,
    pub region: AllocationRegion,
    pub tunnel_id: Option<uuid::Uuid>,
    pub ip_type: IpType,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct DedicatedIpAllocation {
    pub ip_hostname: String,
    pub sub_id: Option<uuid::Uuid>,
    pub region: AllocationRegion,
    pub ip_type: IpType,
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
pub struct ReqFirewallsList {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Firewalls {
    pub max_firewalls: u32,
    pub max_rules: u32,
    pub firewalls: Vec<Firewall>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Firewall {
    pub id: uuid::Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub registered_at: Option<chrono::DateTime<chrono::Utc>>,
    pub rules: String,
    pub rule_count: u32,
    pub tunnels_assigned_count: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqFirewallsCreate {
    pub name: String,
    pub description: Option<String>,
    pub rules: String,
    pub tunnel_id: Option<uuid::Uuid>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum FirewallsCreateError {
    TooManyFirewalls,
    TooManyRules,
    InvalidRules,
    InvalidTunnelId,
}

impl std::fmt::Display for FirewallsCreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for FirewallsCreateError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqFirewallsUpdate {
    pub firewall_id: uuid::Uuid,
    pub rules: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum FirewallsUpdateError {
    TooManyRules,
    InvalidRules,
    FirewallNotFound,
}

impl std::fmt::Display for FirewallsUpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for FirewallsUpdateError {
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
    ClaimCodeNotFound,
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
pub struct ReqLoginCreateGuest {
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
pub enum LoginCreateGuestError {
    Blocked,
}


