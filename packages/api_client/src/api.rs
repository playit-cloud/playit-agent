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
	pub fn v1_tunnels_list(&self) -> impl std::future::Future<Output = Result<AccountTunnelsV1, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/v1/tunnels/list", ReqTunnelsListV1 {}).await)
		}
	}
	#[track_caller]
	pub fn v1_tunnels_create(&self, req: ReqTunnelsCreateV1) -> impl std::future::Future<Output = Result<ObjectId, ApiError<TunnelCreateErrorV1, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/v1/tunnels/create", req).await)
		}
	}
	#[track_caller]
	pub fn v1_schemas_get(&self, req: ReqSchemasGetV1) -> impl std::future::Future<Output = Result<SchemaData, ApiError<SchemaGetError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/v1/schemas/get", req).await)
		}
	}
	#[track_caller]
	pub fn v1_tunnels_config(&self, req: ReqTunnelsConfigV1) -> impl std::future::Future<Output = Result<(), ApiError<TunnelConfigError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/v1/tunnels/config", req).await)
		}
	}
	#[track_caller]
	pub fn v1_tunnels_propset(&self, req: ReqTunnelsPropset) -> impl std::future::Future<Output = Result<(), ApiError<TunnelPropSetError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/v1/tunnels/propset", req).await)
		}
	}
	#[track_caller]
	pub fn v1_tunnels_regionset(&self, req: ReqTunnelsRegionsetV1) -> impl std::future::Future<Output = Result<ObjectId, ApiError<TunnelRegionsetErrorV1, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/v1/tunnels/regionset", req).await)
		}
	}
	#[track_caller]
	pub fn v1_tunnels_typeset(&self, req: ReqTunnelsTypeset) -> impl std::future::Future<Output = Result<(), ApiError<TunnelTypeSetError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/v1/tunnels/typeset", req).await)
		}
	}
	#[track_caller]
	pub fn v1_agents_rundata(&self) -> impl std::future::Future<Output = Result<AgentRunDataV1, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/v1/agents/rundata", ReqAgentsRundataV1 {}).await)
		}
	}
	#[track_caller]
	pub fn info_pops(&self) -> impl std::future::Future<Output = Result<PlayitPops, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/info/pops", ReqInfoPops {}).await)
		}
	}
	#[track_caller]
	pub fn info_regions(&self) -> impl std::future::Future<Output = Result<PlayitRegions, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/info/regions", ReqInfoRegions {}).await)
		}
	}
	#[track_caller]
	pub fn login_signin(&self, req: ReqLoginSignin) -> impl std::future::Future<Output = Result<WebSession, ApiError<SigninFail, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/login/signin", req).await)
		}
	}
	#[track_caller]
	pub fn login_apply(&self, req: ReqLoginApply) -> impl std::future::Future<Output = Result<WebSession, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/login/apply", req).await)
		}
	}
	#[track_caller]
	pub fn login_clearcookie(&self) -> impl std::future::Future<Output = Result<ClearWebSession, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/login/clearcookie", ReqLoginClearcookie {}).await)
		}
	}
	#[track_caller]
	pub fn login_create_guest(&self) -> impl std::future::Future<Output = Result<WebSession, ApiError<LoginCreateGuestError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/login/create/guest", ReqLoginCreateGuest {}).await)
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
	pub fn login_reset_password(&self, req: ReqLoginResetPassword) -> impl std::future::Future<Output = Result<WebSession, ApiError<PasswordResetError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/login/reset/password", req).await)
		}
	}
	#[track_caller]
	pub fn login_reset_send(&self, req: ReqLoginResetSend) -> impl std::future::Future<Output = Result<(), ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/login/reset/send", req).await)
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
	pub fn shop_prices(&self) -> impl std::future::Future<Output = Result<ShopPrices, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/shop/prices", ReqShopPrices {}).await)
		}
	}
	#[track_caller]
	pub fn shop_availability_custom_domain(&self, req: ReqShopAvailabilityCustomDomain) -> impl std::future::Future<Output = Result<IsAvailable, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/shop/availability/custom_domain", req).await)
		}
	}
	#[track_caller]
	pub fn proto_register(&self, req: ReqProtoRegister) -> impl std::future::Future<Output = Result<SignedAgentKey, ApiError<ProtoRegisterError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/proto/register", req).await)
		}
	}
	#[track_caller]
	pub fn charge_get(&self, req: ReqChargeGet) -> impl std::future::Future<Output = Result<ChargeDetails, ApiError<ChargeGetError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/charge/get", req).await)
		}
	}
	#[track_caller]
	pub fn charge_refund(&self, req: ReqChargeRefund) -> impl std::future::Future<Output = Result<(), ApiError<ChargeRefundError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/charge/refund", req).await)
		}
	}
	#[track_caller]
	pub fn proto_mtu_submit(&self, req: ReqProtoMtuSubmit) -> impl std::future::Future<Output = Result<ProtoMtuSubmit, ApiError<ProtoMtuSubmitError, C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap(self.client.call(caller, "/proto/mtu_submit", req).await)
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
	pub fn health(&self) -> impl std::future::Future<Output = Result<HealthResponse, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/health", ReqHealth {}).await)
		}
	}
	#[track_caller]
	pub fn status_page(&self) -> impl std::future::Future<Output = Result<StatusPage, ApiErrorNoFail<C::Error>>> + '_ {
		let caller = std::panic::Location::caller();
		async {
			Self::unwrap_no_fail(self.client.call(caller, "/status-page", ReqStatusPage {}).await)
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

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
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
	TotpRequired,
	NotAllowedWithReadOnly,
	DefaultAgentBlocked,
	AgentNotSelfManaged,
	SelfManagedAgentCanOnlyAffectSelf,
	AccountNotAuthorized,
	ApiKeyScopeRequiresPremium,
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
pub struct ReqTunnelsListV1 {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AccountTunnelsV1 {
	pub tunnels: Vec<AccountTunnelV1>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AccountTunnelV1 {
	pub id: uuid::Uuid,
	pub created_at: chrono::DateTime<chrono::Utc>,
	pub name: Option<String>,
	pub user_enabled: bool,
	pub disabled_by_admin: bool,
	pub offline_reasons: Option<Vec<AccountTunnelOfflineReason>>,
	pub tunnel_type: Option<TunnelType>,
	pub port_type: PortType,
	pub port_count: u16,
	pub firewall_id: Option<uuid::Uuid>,
	pub props: AccountTunnelProps,
	pub origin: AccountTunnelOrigin,
	pub port_allocation_requests: Vec<PortAllocationRequest>,
	pub public_allocations: Vec<PublicAllocation>,
	pub connect_addresses: Vec<ConnectAddress>,
	pub region_change: Option<RegionChangeV1>,
}




#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum AccountTunnelOfflineReason {
	OriginNotSet,
	AgentDisabled,
	AgentOverLimit,
	RequiresTunnelDescription,
	TunnelDisabledByAdmin,
	TunnelDisabledByUser,
	PublicAllocationMissing,
	PublicAllocationPending,
	SubscriptionNotStarted,
	SubscriptionEnded,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum TunnelType {
	#[serde(rename = "minecraft-java")]
	MinecraftJava,
	#[serde(rename = "minecraft-bedrock")]
	MinecraftBedrock,
	#[serde(rename = "nethernet")]
	Nethernet,
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
	#[serde(rename = "https")]
	Https,
	#[serde(rename = "hytale")]
	Hytale,
	#[serde(rename = "project-zomboid")]
	ProjectZomboid,
	#[serde(rename = "vintage-story")]
	VintageStory,
	#[serde(rename = "mc-simple-voice-chat")]
	McSimpleVoiceChat,
	#[serde(rename = "ssh")]
	Ssh,
	#[serde(rename = "beam-mp")]
	BeamMp,
	#[serde(rename = "left-4-dead-2")]
	Left4Dead2,
	#[serde(rename = "veloren")]
	Veloren,
	#[serde(rename = "scp-secret-laboratory")]
	ScpSecretLaboratory,
	#[serde(rename = "necesse")]
	Necesse,
	#[serde(rename = "killing-floor")]
	KillingFloor,
	#[serde(rename = "garrys-mod")]
	GarrysMod,
	#[serde(rename = "factorio")]
	Factorio,
	#[serde(rename = "palworld")]
	Palworld,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum PortType {
	#[serde(rename = "tcp")]
	Tcp,
	#[serde(rename = "udp")]
	Udp,
	#[serde(rename = "both")]
	Both,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AccountTunnelProps {
	pub hostname_verify_level: HostnameVerifyLevel,
	pub app_protect_level: u32,
	pub http_firewall_ignore_acme: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum HostnameVerifyLevel {
	None,
	NoRawIp,
	NoAutoName,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "details")]
pub enum AccountTunnelOrigin {
	#[serde(rename = "not-set")]
	NotSet(TunnelOriginNotSet),
	#[serde(rename = "agent")]
	Agent(TunnelToAgent),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TunnelOriginNotSet {
	pub agent_config: Option<HasAgentConfig>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct HasAgentConfig {
	pub config_schema_id: uuid::Uuid,
	pub config_data: AgentTunnelConfig,
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
pub struct TunnelToAgent {
	pub agent_id: uuid::Uuid,
	pub name: String,
	pub config_schema_id: uuid::Uuid,
	pub config_data: AgentTunnelConfig,
	pub config_invalid: Option<InvalidTunnelConfig>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct InvalidTunnelConfig {
	pub agent_schema_id: uuid::Uuid,
	pub current_schema: AgentTunnelSchema,
	pub target_schema: Option<AgentTunnelSchema>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentTunnelSchema {
	pub fields: std::collections::HashMap<String,AgentTunnelSchemaField>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentTunnelSchemaField {
	pub label: Option<String>,
	pub description: Option<String>,
	pub value_type: AgentTunnelAttrType,
	pub allow_null: bool,
	pub default_value: Option<String>,
	pub variants: Option<Vec<String>>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum AgentTunnelAttrType {
	Ip,
	Ip4,
	Ip6,
	SockAddr,
	SockAddr4,
	SockAddr6,
	Port,
	U64,
	I64,
	Boolean,
	String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PortAllocationRequest {
	pub id: uuid::Uuid,
	pub status: PortAllocationStatus,
	pub region: PlayitNetwork,
	pub public_port: Option<u16>,
	pub public_ip: Option<std::net::IpAddr>,
	pub replaces_alloc_id: Option<uuid::Uuid>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum PortAllocationStatus {
	Pending,
	RanOutOfPorts,
	PublicPortNotAvailable,
	NoPortsAvailableOnIp,
	AccountPortLimitReached,
	#[serde(rename = "Other#catch_all")]
	OtherCatchAll,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum PlayitNetwork {
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
	#[serde(rename = "chile")]
	Chile,
	#[serde(rename = "seattle-washington")]
	SeattleWashington,
	#[serde(rename = "california")]
	California,
	#[serde(rename = "denver-colorado")]
	DenverColorado,
	#[serde(rename = "dallas-texas")]
	DallasTexas,
	#[serde(rename = "chicago-illinois")]
	ChicagoIllinois,
	#[serde(rename = "new-york")]
	NewYork,
	#[serde(rename = "miami")]
	Miami,
	#[serde(rename = "atlanta")]
	Atlanta,
	#[serde(rename = "united-kingdom")]
	UnitedKingdom,
	#[serde(rename = "germany")]
	Germany,
	#[serde(rename = "sweden")]
	Sweden,
	#[serde(rename = "poland")]
	Poland,
	#[serde(rename = "romania")]
	Romania,
	#[serde(rename = "_Test")]
	Test,
	#[serde(rename = "japan")]
	Japan,
	#[serde(rename = "australia")]
	Australia,
	#[serde(rename = "stage-seattle")]
	StageSeattle,
	#[serde(rename = "stage-frankfurt")]
	StageFrankfurt,
	#[serde(rename = "_Reserved3")]
	Reserved3,
	#[serde(rename = "_Reserved4")]
	Reserved4,
	#[serde(rename = "_Reserved5")]
	Reserved5,
	#[serde(rename = "_Reserved6")]
	Reserved6,
	#[serde(rename = "_Reserved7")]
	Reserved7,
	#[serde(rename = "_Reserved8")]
	Reserved8,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "details")]
pub enum PublicAllocation {
	#[serde(rename = "PortAllocation")]
	PortAllocation(PortAllocation),
	#[serde(rename = "Gateway")]
	Gateway(GatewayAllocation),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PortAllocation {
	pub alloc_id: uuid::Uuid,
	pub ip_region: PlayitNetwork,
	pub ip_hostname: String,
	pub auto_domain: String,
	pub ip: std::net::IpAddr,
	pub port: u16,
	pub port_count: u16,
	pub port_type: PortType,
	pub expire_notice: Option<ExpireNotice>,
	pub replaces_alloc_id: Option<uuid::Uuid>,
	pub scheduled_removal_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ExpireNotice {
	pub disable_at: chrono::DateTime<chrono::Utc>,
	pub remove_at: chrono::DateTime<chrono::Utc>,
	pub reason: DisabledReason,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum DisabledReason {
	#[serde(rename = "requires-premium")]
	RequiresPremium,
	#[serde(rename = "over-port-limit")]
	OverPortLimit,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct GatewayAllocation {
	pub id: Option<uuid::Uuid>,
	pub hostname: String,
	pub region: PlayitNetwork,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "value")]
pub enum ConnectAddress {
	#[serde(rename = "addr4")]
	Addr4(ConnectAddr4),
	#[serde(rename = "addr6")]
	Addr6(ConnectAddr6),
	#[serde(rename = "ip4")]
	Ip4(ConnectIp4),
	#[serde(rename = "ip6")]
	Ip6(ConnectIp6),
	#[serde(rename = "auto")]
	Auto(ConnectAutoName),
	#[serde(rename = "domain")]
	Domain(ConnectDomain),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ConnectAddr4 {
	pub address: std::net::SocketAddrV4,
	pub source: ConnectAddressSource,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "resource", content = "id")]
pub enum ConnectAddressSource {
	#[serde(rename = "port-allocation")]
	PortAllocation(uuid::Uuid),
	#[serde(rename = "gateway")]
	Gateway(uuid::Uuid),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ConnectAddr6 {
	pub address: std::net::SocketAddrV6,
	pub source: ConnectAddressSource,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ConnectIp4 {
	pub address: std::net::Ipv4Addr,
	pub default_port: u16,
	pub source: ConnectAddressSource,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ConnectIp6 {
	pub address: std::net::Ipv6Addr,
	pub default_port: u16,
	pub source: ConnectAddressSource,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ConnectAutoName {
	pub address: String,
	pub source: ConnectAddressSource,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ConnectDomain {
	pub id: uuid::Uuid,
	pub domain: String,
	pub address: String,
	pub mode: DomainMode,
	pub source: ConnectAddressSource,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum DomainMode {
	Ip,
	Srv,
	SrvAndIp,
	Hostname,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "state", content = "details")]
pub enum RegionChangeV1 {
	#[serde(rename = "allocating")]
	Allocating(RegionChangeAllocating),
	#[serde(rename = "overlapping")]
	Overlapping(RegionChangeOverlapping),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct RegionChangeAllocating {
	pub request_id: uuid::Uuid,
	pub target_region: PlayitNetwork,
	pub status: PortAllocationStatus,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct RegionChangeOverlapping {
	pub old_alloc_id: uuid::Uuid,
	pub new_alloc_id: uuid::Uuid,
	pub new_region: PlayitNetwork,
	pub old_region: PlayitNetwork,
	pub old_removed_at: chrono::DateTime<chrono::Utc>,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsCreateV1 {
	pub name: String,
	pub protocol: TunnelProtocol,
	pub origin: AccountTunnelOriginCreate,
	pub endpoint: CreateTunnelEndpoint,
	pub enabled: bool,
	pub firewall_id: Option<uuid::Uuid>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "details")]
pub enum TunnelProtocol {
	#[serde(rename = "tunnel-type")]
	TunnelType(TunnelType),
	#[serde(rename = "raw-ports")]
	RawPorts(TunnelProtocolRawPorts),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TunnelProtocolRawPorts {
	pub port_type: PortType,
	pub port_count: u16,
	pub software_description: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "data")]
pub enum AccountTunnelOriginCreate {
	#[serde(rename = "agent")]
	Agent(AgentOrigin),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentOrigin {
	pub agent_id: Option<uuid::Uuid>,
	pub config: AgentTunnelConfig,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "details")]
pub enum CreateTunnelEndpoint {
	#[serde(rename = "gateway")]
	Gateway(UseGateway),
	#[serde(rename = "dedicated-ip")]
	DedicatedIp(UseAllocDedicatedIp),
	#[serde(rename = "shared-ip")]
	SharedIp(UseAllocSharedIp),
	#[serde(rename = "region")]
	Region(UseAllocRegion),
	#[serde(rename = "port-allocation")]
	PortAllocation(uuid::Uuid),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct UseGateway {
	pub gateway_id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct UseAllocDedicatedIp {
	pub ip_hostname: String,
	pub port: Option<u16>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct UseAllocSharedIp {
	pub ip_hostname: String,
	pub port: Option<u16>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct UseAllocRegion {
	pub region: PlayitNetwork,
	pub port: Option<u16>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ObjectId {
	pub id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum TunnelCreateErrorV1 {
	AgentNotFound,
	InvalidAgentId,
	DedicatedIpNotFound,
	PortAllocNotFound,
	InvalidIpHostname,
	InvalidPortCount,
	RequiresVerifiedAccount,
	RegionNotSupported,
	InvalidTunnelConfig,
	FirewallNotFound,
	PortAllocDoesNotMatchPortDetails,
	RegionRequiresPlayitPremium,
	PortAllocCurrentlyAssigned,
	PublicPortRequiresPlayitPremium,
	AgentVersionTooOld,
	RequiresPlayitPremium,
	EndpointDoesNotSupportProtocol,
	AccountHasFlaggedTunnel,
	InvalidGatewayId,
	GatewayAlreadyHasTunnelType,
	GatewayDoesNotSupportTunnelType,
	TunnelTypeBlockedOnRegion,
	AccountTunnelLimitReached,
	AccountUnassignedTunnelLimitReached,
}

impl std::fmt::Display for TunnelCreateErrorV1 {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for TunnelCreateErrorV1 {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqSchemasGetV1 {
	pub id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct SchemaData {
	pub id: uuid::Uuid,
	pub details: AgentSchema,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentSchema {
	pub default_schema: Option<AgentTunnelSchema>,
	pub schemas: Vec<AgentSchemaForTunnelType>,
	pub only_explicit_schemas: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentSchemaForTunnelType {
	pub tunnel_type: AgentSchemaTunnelType,
	pub schema: Option<AgentTunnelSchema>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "name", content = "details")]
pub enum AgentSchemaTunnelType {
	#[serde(rename = "custom-tcp")]
	CustomTcp(AgentTunnelTypeSupportedPorts),
	#[serde(rename = "custom-udp")]
	CustomUdp(AgentTunnelTypeSupportedPorts),
	#[serde(rename = "custom-both")]
	CustomBoth(AgentTunnelTypeSupportedPorts),
	#[serde(rename = "tunnel-type")]
	TunnelType(TunnelType),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentTunnelTypeSupportedPorts {
	pub min: u16,
	pub max: u16,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum SchemaGetError {
	SchemaNotFound,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsConfigV1 {
	pub tunnel_id: uuid::Uuid,
	pub new_agent_id: Option<uuid::Uuid>,
	pub new_config: Option<AgentTunnelConfig>,
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
	TunnelTypeNotSupported(TunnelEndpointPortRequirements),
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
pub struct TunnelEndpointPortRequirements {
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
pub struct ReqTunnelsPropset {
	pub tunnel_id: uuid::Uuid,
	pub details: PropsetDetails,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "value")]
pub enum PropsetDetails {
	#[serde(rename = "hostname_verify_level")]
	HostnameVerifyLevel(HostnameVerifyLevel),
	#[serde(rename = "app_protect_level")]
	AppProtectLevel(u32),
	#[serde(rename = "http_firewall_ignore_acme")]
	HttpFirewallIgnoreAcme(bool),
	#[serde(rename = "custom_tunnel_details")]
	CustomTunnelDetails(String),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum TunnelPropSetError {
	RequiresPremium,
	TunnelNotFound,
	PropertyValueNotSupportedForTunnelType,
	PropertyValueInvalid,
}

impl std::fmt::Display for TunnelPropSetError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for TunnelPropSetError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsRegionsetV1 {
	pub tunnel_id: uuid::Uuid,
	pub region: PlayitNetwork,
	pub allow_port_change: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum TunnelRegionsetErrorV1 {
	RequiresPlayitPremium,
	TunnelNotFound,
	RegionNotSupported,
	AlreadyInRegion,
	RegionChangePending,
	NoCurrentAllocation,
	RegionChangeNotSupportedForTunnel,
}

impl std::fmt::Display for TunnelRegionsetErrorV1 {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for TunnelRegionsetErrorV1 {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsTypeset {
	pub tunnel_id: uuid::Uuid,
	pub tunnel_type: TunnelType,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum TunnelTypeSetError {
	RequiresPremium,
	TunnelNotFound,
	TunnelHasInvalidSettingsForType,
	CannotChangeTunnelType,
}

impl std::fmt::Display for TunnelTypeSetError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for TunnelTypeSetError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqAgentsRundataV1 {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentRunDataV1 {
	pub agent_id: uuid::Uuid,
	pub tunnels: Vec<AgentTunnelV1>,
	pub pending: Vec<AgentPendingTunnelV1>,
	pub notices: Vec<AgentNotice>,
	pub permissions: AgentPermissions,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentTunnelV1 {
	pub id: uuid::Uuid,
	pub internal_id: u64,
	pub name: String,
	pub display_address: String,
	pub port_type: PortType,
	pub port_count: u16,
	pub tunnel_type: Option<String>,
	pub tunnel_type_display: String,
	pub agent_config: AgentTunnelConfig,
	pub disabled_reason: Option<std::borrow::Cow<'static,str>>,
}


#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentPendingTunnelV1 {
	pub id: uuid::Uuid,
	pub name: String,
	pub tunnel_type: Option<String>,
	pub tunnel_type_display: String,
	pub port_type: PortType,
	pub port_count: u16,
	pub status_msg: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentNotice {
	pub priority: AgentNoticePriority,
	pub message: std::borrow::Cow<'static,str>,
	pub resolve_link: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum AgentNoticePriority {
	Critical,
	High,
	Low,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentPermissions {
	pub is_self_managed: bool,
	pub has_premium: bool,
	pub account_status: AccountStatus,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum AccountStatus {
	#[serde(rename = "guest")]
	Guest,
	#[serde(rename = "email-not-verified")]
	EmailNotVerified,
	#[serde(rename = "verified")]
	Verified,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqInfoPops {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PlayitPops {
	pub pops: Vec<Pop>,
	pub regions: Vec<PlayitNetwork>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Pop {
	pub pop: PlayitPop,
	pub name: String,
	pub region: PlayitNetwork,
	pub online: bool,
	pub ip4_premium: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
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
	#[serde(rename = "USAtlanta")]
	UsAtlanta,
	StageSeattle,
	StageFrankfurt,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqInfoRegions {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PlayitRegions {
	pub groups: Vec<RegionGroup>,
	pub regions: Vec<RegionInfo>,
	pub datacenters: Vec<DatacenterInfo>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct RegionGroup {
	pub id: PlayitRegionGroup,
	pub title: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum PlayitRegionGroup {
	#[serde(rename = "global")]
	Global,
	#[serde(rename = "north-america")]
	NorthAmerica,
	#[serde(rename = "europe")]
	Europe,
	#[serde(rename = "asia")]
	Asia,
	#[serde(rename = "south-america")]
	SouthAmerica,
	#[serde(rename = "oceania")]
	Oceania,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct RegionInfo {
	pub id: PlayitNetwork,
	pub required_tier: RegionPaymentTier,
	pub group_id: PlayitRegionGroup,
	pub title: String,
	pub routing_title: String,
	pub datacenter_ids: Vec<u32>,
	pub min_agent_version: Option<String>,
	pub allows_unprotected_tcp: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum RegionPaymentTier {
	#[serde(rename = "free")]
	Free,
	#[serde(rename = "playit-premium")]
	PlayitPremium,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct DatacenterInfo {
	pub id: u32,
	pub region: PlayitNetwork,
	pub name: PlayitDataCenter,
	pub title: String,
	pub lat: f64,
	pub lon: f64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum PlayitDataCenter {
	SameDc,
	FremontCa,
	Atlanta,
	Stockholm,
	Tokyo,
	Singapore,
	Sydney,
	SaoPaulo,
	Warsaw,
	Mumbai,
	Santiago,
	Frankfurt,
	Madrid,
	London,
	Bucharest,
	LosAngeles,
	NewYork,
	Seattle,
	Denver,
	Dallas,
	Miami,
	Chicago,
	StageSeattle,
	StageFrankfurt,
	#[serde(rename = "_LosAngeles1")]
	LosAngeles1,
	#[serde(rename = "_TelAviv1")]
	TelAviv1,
	#[serde(rename = "_Chicago1")]
	Chicago1,
	#[serde(rename = "_Dallas1")]
	Dallas1,
	#[serde(rename = "_MexicoCity1")]
	MexicoCity1,
	#[serde(rename = "_Miami1")]
	Miami1,
	#[serde(rename = "_SeattleStage1")]
	SeattleStage1,
	#[serde(rename = "_SingaporeStage1")]
	SingaporeStage1,
	#[serde(rename = "_Toronto1")]
	Toronto1,
	#[serde(rename = "_London1")]
	London1,
	#[serde(rename = "_Amsterdam1")]
	Amsterdam1,
	#[serde(rename = "_FrankfurtStage1")]
	FrankfurtStage1,
	#[serde(rename = "_Paris1")]
	Paris1,
	#[serde(rename = "_Seoul1")]
	Seoul1,
	#[serde(rename = "_Madrid1")]
	Madrid1,
	#[serde(rename = "_Melbourne1")]
	Melbourne1,
	#[serde(rename = "_LasVegas1")]
	LasVegas1,
	#[serde(rename = "_Mumbai1")]
	Mumbai1,
	#[serde(rename = "_KansasCity1")]
	KansasCity1,
	#[serde(rename = "_Honolulu1")]
	Honolulu1,
	#[serde(rename = "_Dallas2")]
	Dallas2,
	#[serde(rename = "_NewYork1")]
	NewYork1,
	#[serde(rename = "_Miami2")]
	Miami2,
	#[serde(rename = "_Bangalore1")]
	Bangalore1,
	#[serde(rename = "_Delhi1")]
	Delhi1,
}


pub type ReqLoginSignin = LoginCredentials;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct LoginCredentials {
	pub email: String,
	pub password: String,
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
	pub admin_review_id: Option<std::num::NonZeroU64>,
	pub read_only: bool,
	pub show_admin: bool,
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

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum SigninFail {
	IncorrectCredentials,
	AccountBanned,
}

impl std::fmt::Display for SigninFail {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for SigninFail {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqLoginApply {
	pub token: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqLoginClearcookie {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ClearWebSession {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqLoginCreateGuest {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum LoginCreateGuestError {
	Blocked,
	NoLongerSupported,
}

impl std::fmt::Display for LoginCreateGuestError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for LoginCreateGuestError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqLoginGuest {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum GuestLoginError {
	AccountIsNotGuest,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqLoginResetPassword {
	pub email: String,
	pub reset_code: String,
	pub new_password: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum PasswordResetError {
	ResetCodeExpired,
	InvalidResetCode,
	InvalidNewPassword,
}

impl std::fmt::Display for PasswordResetError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for PasswordResetError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqLoginResetSend {
	pub email: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsCreate {
	pub name: Option<String>,
	pub tunnel_type: Option<TunnelType>,
	pub tunnel_description: Option<String>,
	pub port_type: PortType,
	pub port_count: u16,
	pub origin: TunnelOriginCreate,
	pub enabled: bool,
	pub alloc: Option<TunnelCreateUseAllocation>,
	pub firewall_id: Option<uuid::Uuid>,
	pub proxy_protocol: Option<ProxyProtocol>,
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
pub struct UseAllocPortAlloc {
	pub alloc_id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct UseRegion {
	pub region: PlayitNetwork,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum ProxyProtocol {
	#[serde(rename = "proxy-protocol-v1")]
	ProxyProtocolV1,
	#[serde(rename = "proxy-protocol-v2")]
	ProxyProtocolV2,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum TunnelCreateError {
	DefaultAgentNotSupported,
	AgentNotFound,
	InvalidAgentId,
	AgentVersionTooOld,
	DedicatedIpNotFound,
	DedicatedIpPortNotAvailable,
	DedicatedIpNotEnoughSpace,
	PortAllocNotFound,
	InvalidIpHostname,
	ManagedMissingAgentId,
	InvalidPortCount,
	RequiresVerifiedAccount,
	FirewallNotFound,
	AllocInvalid,
	InvalidOrigin,
	RequiresPlayitPremium,
	TunnelTypeBlockedOnRegion,
	TunnelTypeRequiresDescription,
	AccountHasFlaggedTunnel,
	AccountTunnelLimitReached,
	AccountUnassignedTunnelLimitReached,
	Other,
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
	pub tunnels: Vec<AccountTunnel>,
	pub tcp_alloc: AllocatedPorts,
	pub udp_alloc: AllocatedPorts,
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
	pub disabled_reason: Option<TunnelOfflineReason>,
	pub region: Option<PlayitNetwork>,
	pub expire_notice: Option<ExpireNotice>,
	pub proxy_protocol: Option<ProxyProtocol>,
	pub hostname_verify_level: HostnameVerifyLevel,
	pub agent_over_limit: bool,
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
	pub reason: TunnelOfflineReason,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum TunnelOfflineReason {
	#[serde(rename = "requires-premium")]
	RequiresPremium,
	#[serde(rename = "over-port-limit")]
	OverPortLimit,
	#[serde(rename = "ip-used-in-gre")]
	IpUsedInGre,
	#[serde(rename = "public-port-not-available")]
	PublicPortNotAvailable,
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
	pub region: PlayitNetwork,
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
	pub region: PlayitNetwork,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct SubscriptionId {
	pub sub_id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
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
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Ratelimit {
	pub bytes_per_second: Option<u32>,
	pub packets_per_second: Option<u32>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AllocatedPorts {
	pub allowed: u32,
	pub claimed: u32,
	pub desired: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsUpdate {
	pub tunnel_id: uuid::Uuid,
	pub local_ip: std::net::IpAddr,
	pub local_port: Option<u16>,
	pub agent_id: Option<uuid::Uuid>,
	pub enabled: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum UpdateError {
	ChangingAgentIdNotAllowed,
	TunnelNotFound,
	CannotUpdateLocalAddressForUnassignedTunnel,
	InvalidAgentId,
	AddressOrProxyProtoNotSupportedByAgent,
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

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum DeleteError {
	TunnelNotFound,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsRename {
	pub tunnel_id: uuid::Uuid,
	pub name: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum TunnelRenameError {
	TunnelNotFound,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsFirewallAssign {
	pub tunnel_id: uuid::Uuid,
	pub firewall_id: Option<uuid::Uuid>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
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

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
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

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum TunnelEnableError {
	TunnelNotFound,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqTunnelsProxySet {
	pub tunnel_id: uuid::Uuid,
	pub proxy_protocol: Option<ProxyProtocol>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum TunnelProxySetError {
	TunnelNotFound,
	ProxyProtocolNotSupportedByAgent,
}

impl std::fmt::Display for TunnelProxySetError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for TunnelProxySetError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqClaimSetup {
	pub code: String,
	pub agent_type: ClaimAgentType,
	pub version: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum ClaimAgentType {
	#[serde(rename = "assignable")]
	Assignable,
	#[serde(rename = "self-managed")]
	SelfManaged,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum ClaimSetupResponse {
	WaitingForUserVisit,
	WaitingForUser,
	UserAccepted,
	UserRejected,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum ClaimSetupError {
	CodeExpired,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqClaimExchange {
	pub code: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentSecretKey {
	pub secret_key: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
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
pub struct ReqAgentsRename {
	pub agent_id: uuid::Uuid,
	pub name: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum AgentRenameError {
	AgentNotFound,
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
	Region(PlayitNetwork),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum AgentRoutingSetError {
	RequiresPremium,
	AgentNotFound,
	InvalidAgentId,
	RegionNotSupported,
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

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
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

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum AgentType {
	#[serde(rename = "default")]
	Default,
	#[serde(rename = "assignable")]
	Assignable,
	#[serde(rename = "self-managed")]
	SelfManaged,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
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
	pub agent_config: AgentTunnelConfig,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PortRange {
	pub from: u16,
	pub to: u16,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
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
	pub external_method: Option<ExternalDomainMethod>,
	pub parent: Option<uuid::Uuid>,
	pub sub_id: uuid::Uuid,
	pub ip_version_only: Option<IpVersion>,
	pub target: Option<DomainTarget>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum ExternalDomainMethod {
	Ns,
	Cname,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum IpVersion {
	#[serde(rename = "ipv4")]
	Ipv4,
	#[serde(rename = "ipv6")]
	Ipv6,
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
	#[serde(rename = "gateway")]
	Gateway(DomainTargetGateway),
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
pub struct DomainTargetGateway {
	pub gateway_id: uuid::Uuid,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqShopPrices {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ShopPrices {
	pub currency_guess: Option<Currency>,
	pub prices: Vec<ShopPrice>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum Currency {
	#[serde(rename = "USD")]
	Usd,
	#[serde(rename = "EUR")]
	Eur,
	#[serde(rename = "GBP")]
	Gbp,
	#[serde(rename = "BRL")]
	Brl,
	#[serde(rename = "AUD")]
	Aud,
	#[serde(rename = "INR")]
	Inr,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ShopPrice {
	pub product: SubProductType,
	pub currency: Currency,
	pub monthly: Option<u32>,
	pub yearly: Option<u32>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum SubProductType {
	#[serde(rename = "playit-premium")]
	PlayitPremium,
	#[serde(rename = "playit-premium-trial")]
	PlayitPremiumTrial,
	#[serde(rename = "dedicated-ip")]
	DedicatedIp,
	#[serde(rename = "udp-ports")]
	UdpPorts,
	#[serde(rename = "tcp-ports")]
	TcpPorts,
	#[serde(rename = "both-ports")]
	BothPorts,
	#[serde(rename = "custom-domain")]
	CustomDomain,
	#[serde(rename = "dedicated-port-alloc")]
	DedicatedPortAlloc,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqShopAvailabilityCustomDomain {
	pub name: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct IsAvailable {
	pub is_available: bool,
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

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
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

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
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
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqChargeGet {
	pub reference_code: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ChargeDetails {
	pub reference_code: String,
	pub created_at: chrono::DateTime<chrono::Utc>,
	pub invoice_type: InvoiceType,
	pub invoice_status: InvoiceStatus,
	pub total_cost: String,
	pub items: Vec<ChargeDetailsItem>,
	pub refund: Option<RefundStatus>,
	pub currency: Currency,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum InvoiceType {
	Subscription,
	StartSubscription,
	StripeSubscription,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum InvoiceStatus {
	#[serde(rename = "draft")]
	Draft,
	#[serde(rename = "open")]
	Open,
	#[serde(rename = "paid")]
	Paid,
	#[serde(rename = "void")]
	Void,
	#[serde(rename = "uncollectible")]
	Uncollectible,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ChargeDetailsItem {
	pub product: SubProductType,
	pub months: u32,
	pub total_cost: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "details")]
pub enum RefundStatus {
	#[serde(rename = "Pending")]
	Pending(PendingRefundRequest),
	#[serde(rename = "Applied")]
	Applied(RefundApplied),
	#[serde(rename = "DisputeCreated")]
	DisputeCreated(DisputeCreated),
	#[serde(rename = "ChargedBack")]
	ChargedBack(ChargedBack),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PendingRefundRequest {
	pub created_at: chrono::DateTime<chrono::Utc>,
	pub reason: RefundRequestReason,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum RefundRequestReason {
	#[serde(rename = "fraud")]
	Fraud,
	#[serde(rename = "not-satisfied")]
	NotSatisfied,
	#[serde(rename = "issuer-fraud-warning")]
	IssuerFraudWarning,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct RefundApplied {
	pub created_at: chrono::DateTime<chrono::Utc>,
	pub refund_amount: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct DisputeCreated {
	pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ChargedBack {
	pub created_at: chrono::DateTime<chrono::Utc>,
	pub amount: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum ChargeGetError {
	ChargeNotFound,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqChargeRefund {
	pub reference_code: String,
	pub reason: RefundRequestReason,
	pub email: Option<String>,
	pub refund_message: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum ChargeRefundError {
	ChargeNotFound,
	UnauthorizedReason,
}

impl std::fmt::Display for ChargeRefundError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for ChargeRefundError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqProtoMtuSubmit {
	pub measurements: Vec<AgentMtuMeasurement>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct AgentMtuMeasurement {
	pub data_center_id: u32,
	pub direction: AgentMtuDirection,
	pub mtu: u16,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum AgentMtuDirection {
	#[serde(rename = "send")]
	Send,
	#[serde(rename = "receive")]
	Receive,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ProtoMtuSubmit {
	pub accepted: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum ProtoMtuSubmitError {
	EmptyMeasurements,
	TooManyMeasurements,
	InvalidMtu,
	InvalidDataCenter,
}

impl std::fmt::Display for ProtoMtuSubmitError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{:?}", self)
	}
}

impl std::error::Error for ProtoMtuSubmitError {
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqQueryRegion {
	pub limit_region: Option<PlayitNetwork>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct QueryRegion {
	pub region: PlayitNetwork,
	pub pop: PlayitPop,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum QueryRegionError {
	FailedToDetermineLocation,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqHealth {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct HealthResponse {
	pub database: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReqStatusPage {
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct StatusPage {
	pub generated_at: Option<chrono::DateTime<chrono::Utc>>,
	pub categories: Vec<StatusPageCategory>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct StatusPageCategory {
	pub category: String,
	pub elements: Vec<StatusPageEntry>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct StatusPageEntry {
	pub id: u64,
	pub title: String,
	pub details: String,
	pub status: String,
	pub status_level: StatusLevel,
	pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub enum StatusLevel {
	#[serde(rename = "success")]
	Success,
	#[serde(rename = "warning")]
	Warning,
	#[serde(rename = "error")]
	Error,
	#[serde(rename = "info")]
	Info,
	#[serde(rename = "critical")]
	Critical,
}

