use clap::{Args, Parser, Subcommand, ValueEnum};
use uuid::Uuid;

#[derive(Parser, Debug)]
pub struct CliArgs {
    #[command(subcommand)]
    pub cmd: Commands,

    /* secrets */
    #[arg(long)]
    pub secret: Option<String>,
    #[arg(long("secret-path"), alias = "secret_path")]
    pub secret_path: Option<String>,
    #[arg(long("secret-wait"), alias = "secret_wait", default_value = "false")]
    pub secret_wait: bool,

    /* logging */
    #[arg(short, long, default_value = "false")]
    pub stdout: bool,
    #[arg(short, long("log-path"))]
    pub log_path: Option<String>,

    /* other opts */
    #[arg(long("platform-docker"), alias = "platform_docker", default_value = "false")]
    pub platform_docker: bool,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    Version,
    #[command(subcommand)]
    Account(CmdAccount),
    #[command(subcommand)]
    Claim(CmdClaim),
    #[command(subcommand, about = "Start the playit program")]
    Start,
    #[command(subcommand)]
    Tunnels(CmdTunnels),
    #[command(subcommand)]
    Reset,
    SecretPath,
    #[cfg(target_os = "linux")]
    Setup,
}

#[derive(Subcommand, Debug)]
pub enum CmdAccount {
    LoginUrl,
}

#[derive(Subcommand, Debug)]
#[command(about = "Commands to setup a new agent")]
pub enum CmdClaim {
    Generate,
    Url(CmdClaimUrl),
    Exchange(CmdClaimExchange),
}

#[derive(Args, Debug)]
#[command(about = "Generate a URL for the user to link the agent to their account")]
pub struct CmdClaimUrl {
    #[arg()]
    pub claim_code: String,

    #[arg(long("name"))]
    pub agent_name: Option<String>,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdAgentType {
    SelfManaged,
    Asignable,
}

#[derive(Args, Debug)]
#[command(about = "Exchange the claim code for a secret key to operate the agent")]
pub struct CmdClaimExchange {
    #[arg()]
    pub claim_code: String,

    #[arg(long, default_value = "0")]
    pub wait: u32,

    #[arg(long("type"), default_value = "self-managed")]
    pub agent_type: CmdAgentType,
}

#[derive(Subcommand, Debug)]
#[command(about = "Commands to manage tunnels")]
pub enum CmdTunnels {
    Prepare(CmdTunnelsPrepare),
    Delete(CmdTunnelsDelete),
    Find(CmdTunnelsFind),
    List,
    WaitFor(CmdTunnelsWaitFor),
    Set(CmdTunnelsSet),
}

#[derive(Args, Debug)]
pub struct CmdTunnelsPrepare {
    #[arg()]
    pub name: String,
    #[arg()]
    pub tunnel_type: CmdTunnelType,
    #[arg()]
    pub local_address: String,

    #[arg(short('r'), long("region"), default_value = "optimal")]
    pub region: CmdTunnelRegion,
    #[arg(long("ignore-region"), default_value = "true")]
    pub ignore_region: bool,
    #[arg(long("ignore-name"), default_value = "false")]
    pub ignore_name: bool,

    #[arg(short('c'), long("port-count"), default_value = "1")]
    pub port_count: u16,
    #[arg(short('u'), long("update-only"), default_value = "false")]
    pub update_only: bool,
    #[arg(short('n'), long("create-new"), default_value = "false")]
    pub create_new: bool,

    #[arg(short('p'), long("public-port"))]
    pub public_port: Option<u16>,
    #[arg(short('d'), long("use-dedicated-ip"))]
    pub use_dedicated_ip: Option<String>,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdTunnelRegion {
    GlobalAnycast,
    Optimal,
    NorthAmerica,
    Europe,
    Asia,
    India,
    SouthAmerica,
}

#[derive(Args, Debug)]
pub struct CmdTunnelsFind {
    #[arg()]
    pub name: String,
    #[arg()]
    pub tunnel_type: CmdTunnelType,
    #[arg(short('c'), long("port-count"), default_value = "1")]
    pub port_count: u32,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdTunnelType {
    MinecraftJava,
    MinecraftBedrock,
    Tcp,
    Udp,
    Both,
}

#[derive(Args, Debug)]
pub struct CmdTunnelsWaitFor {
    pub tunnel_id: Uuid,

    #[arg(long, default_value = "0")]
    pub wait: u32,
}

#[derive(Args, Debug)]
pub struct CmdTunnelsDelete {
    pub tunnel_id: Uuid,

    #[arg(long, default_value = "false")]
    pub confirm: bool,
}

#[derive(Args, Debug)]
pub struct CmdTunnelsSet {
    pub tunnel_id: Uuid,

    #[command(subcommand)]
    pub command: CmdTunnelsSetCommands,
}

#[derive(Subcommand, Debug)]
pub enum CmdTunnelsSetCommands {
    LocalAddress(CmdSetLocalAddress),
    Status(CmdSetStatus),
    ProxyProtocol(CmdSetProxyProtocol),
    Firewall(CmdSetFirewall),
}

#[derive(Args, Debug)]
pub struct CmdSetLocalAddress {
    #[arg()]
    pub address: String,
}

#[derive(Args, Debug)]
pub struct CmdSetStatus {
    #[arg()]
    pub status: CmdTunnelStatus,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdTunnelStatus {
    Enabled,
    Disabled,
}

#[derive(Args, Debug)]
pub struct CmdSetProxyProtocol {
    #[arg()]
    pub protocol: CmdTunnelProxyProtocol,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdTunnelProxyProtocol {
    None,
    ProxyProtocolV1,
    ProxyProtocolV2,
}

#[derive(Args, Debug)]
pub struct CmdSetFirewall {
    #[arg()]
    pub firewall_id: Uuid,
}