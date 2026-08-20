use std::num::{NonZeroU32, NonZeroUsize};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub api_base: ApiEndpoint,
    pub control: ControlPolicy,
    pub tcp: AdmissionPolicy,
    pub udp: AdmissionPolicy,
    pub shutdown: ShutdownPolicy,
    pub paths: RuntimeLocations,
}

impl AppConfig {
    pub fn validate(raw: RawAppConfig) -> Result<Self, ConfigError> {
        Ok(Self {
            api_base: ApiEndpoint::parse(raw.api_base)?,
            control: ControlPolicy {
                connect_timeout: nonzero_duration(
                    raw.control_connect_timeout_millis,
                    ConfigField::ControlConnectTimeout,
                )?,
                retry_delay: nonzero_duration(
                    raw.control_retry_delay_millis,
                    ConfigField::ControlRetryDelay,
                )?,
                event_queue_capacity: nonzero_usize(
                    raw.control_event_queue_capacity,
                    ConfigField::ControlEventQueueCapacity,
                )?,
            },
            tcp: AdmissionPolicy::validate(
                raw.tcp_per_second,
                raw.tcp_burst,
                raw.tcp_queue_capacity,
                ConfigField::TcpPerSecond,
                ConfigField::TcpBurst,
                ConfigField::TcpQueueCapacity,
            )?,
            udp: AdmissionPolicy::validate(
                raw.udp_per_second,
                raw.udp_burst,
                raw.udp_queue_capacity,
                ConfigField::UdpPerSecond,
                ConfigField::UdpBurst,
                ConfigField::UdpQueueCapacity,
            )?,
            shutdown: ShutdownPolicy {
                drain_timeout: nonzero_duration(
                    raw.shutdown_drain_timeout_millis,
                    ConfigField::ShutdownDrainTimeout,
                )?,
            },
            paths: RuntimeLocations {
                secret: RuntimeLocation::parse(raw.secret_location, ConfigField::SecretLocation)?,
                ipc: RuntimeLocation::parse(raw.ipc_location, ConfigField::IpcLocation)?,
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawAppConfig {
    pub api_base: String,
    pub control_connect_timeout_millis: u64,
    pub control_retry_delay_millis: u64,
    pub control_event_queue_capacity: usize,
    pub tcp_per_second: u32,
    pub tcp_burst: u32,
    pub tcp_queue_capacity: usize,
    pub udp_per_second: u32,
    pub udp_burst: u32,
    pub udp_queue_capacity: usize,
    pub shutdown_drain_timeout_millis: u64,
    pub secret_location: String,
    pub ipc_location: String,
}

impl Default for RawAppConfig {
    fn default() -> Self {
        Self {
            api_base: "https://api.playit.gg".to_owned(),
            control_connect_timeout_millis: 10_000,
            control_retry_delay_millis: 2_000,
            control_event_queue_capacity: 1_024,
            tcp_per_second: 100,
            tcp_burst: 300,
            tcp_queue_capacity: 1_024,
            udp_per_second: 16,
            udp_burst: 32,
            udp_queue_capacity: 2_048,
            shutdown_drain_timeout_millis: 5_000,
            secret_location: "playit.toml".to_owned(),
            ipc_location: "playit.sock".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiEndpoint(String);

impl ApiEndpoint {
    pub fn parse(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        let valid = ["http://", "https://"]
            .iter()
            .find_map(|prefix| value.strip_prefix(prefix))
            .is_some_and(|authority| {
                !authority.is_empty()
                    && !authority.starts_with('/')
                    && !authority.bytes().any(|byte| byte.is_ascii_whitespace())
            });
        valid.then_some(Self(value)).ok_or(ConfigError {
            field: ConfigField::ApiBase,
            kind: ConfigErrorKind::InvalidEndpoint,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLocations {
    pub secret: RuntimeLocation,
    pub ipc: RuntimeLocation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLocation(String);

impl RuntimeLocation {
    pub fn parse(value: impl Into<String>, field: ConfigField) -> Result<Self, ConfigError> {
        let value = value.into();
        (!value.trim().is_empty())
            .then_some(Self(value))
            .ok_or(ConfigError {
                field,
                kind: ConfigErrorKind::Empty,
            })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPolicy {
    pub connect_timeout: Duration,
    pub retry_delay: Duration,
    pub event_queue_capacity: NonZeroUsize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionPolicy {
    pub per_second: NonZeroU32,
    pub burst: NonZeroU32,
    pub queue_capacity: NonZeroUsize,
}

impl AdmissionPolicy {
    fn validate(
        per_second: u32,
        burst: u32,
        queue_capacity: usize,
        per_second_field: ConfigField,
        burst_field: ConfigField,
        queue_field: ConfigField,
    ) -> Result<Self, ConfigError> {
        Ok(Self {
            per_second: nonzero_u32(per_second, per_second_field)?,
            burst: nonzero_u32(burst, burst_field)?,
            queue_capacity: nonzero_usize(queue_capacity, queue_field)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownPolicy {
    pub drain_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigError {
    pub field: ConfigField,
    pub kind: ConfigErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigErrorKind {
    Zero,
    Empty,
    InvalidEndpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigField {
    ApiBase,
    ControlConnectTimeout,
    ControlRetryDelay,
    ControlEventQueueCapacity,
    TcpPerSecond,
    TcpBurst,
    TcpQueueCapacity,
    UdpPerSecond,
    UdpBurst,
    UdpQueueCapacity,
    ShutdownDrainTimeout,
    SecretLocation,
    IpcLocation,
}

fn nonzero_u32(value: u32, field: ConfigField) -> Result<NonZeroU32, ConfigError> {
    NonZeroU32::new(value).ok_or(ConfigError {
        field,
        kind: ConfigErrorKind::Zero,
    })
}

fn nonzero_usize(value: usize, field: ConfigField) -> Result<NonZeroUsize, ConfigError> {
    NonZeroUsize::new(value).ok_or(ConfigError {
        field,
        kind: ConfigErrorKind::Zero,
    })
}

fn nonzero_duration(value: u64, field: ConfigField) -> Result<Duration, ConfigError> {
    (value != 0)
        .then_some(Duration::from_millis(value))
        .ok_or(ConfigError {
            field,
            kind: ConfigErrorKind::Zero,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preserve_current_admission_limits() {
        let config = AppConfig::validate(RawAppConfig::default()).unwrap();
        assert_eq!(config.tcp.per_second.get(), 100);
        assert_eq!(config.tcp.burst.get(), 300);
        assert_eq!(config.udp.per_second.get(), 16);
        assert_eq!(config.udp.burst.get(), 32);
    }

    #[test]
    fn zero_capacities_are_rejected_before_runtime_creation() {
        let raw = RawAppConfig {
            udp_queue_capacity: 0,
            ..RawAppConfig::default()
        };
        assert_eq!(
            AppConfig::validate(raw),
            Err(ConfigError {
                field: ConfigField::UdpQueueCapacity,
                kind: ConfigErrorKind::Zero,
            })
        );
    }

    #[test]
    fn durations_and_endpoints_are_validated() {
        let zero_timeout = RawAppConfig {
            shutdown_drain_timeout_millis: 0,
            ..RawAppConfig::default()
        };
        assert_eq!(
            AppConfig::validate(zero_timeout).unwrap_err().field,
            ConfigField::ShutdownDrainTimeout
        );

        let invalid_endpoint = RawAppConfig {
            api_base: "api.playit.gg".to_owned(),
            ..RawAppConfig::default()
        };
        assert_eq!(
            AppConfig::validate(invalid_endpoint).unwrap_err().kind,
            ConfigErrorKind::InvalidEndpoint
        );
    }
}
