use playit_api_client::api::{
    AgentSchema, AgentSchemaTunnelType, AgentTunnelSchema, ProxyProtocol, TunnelType,
};
use serde_json::Value;

fn proxy_modes(schema: &AgentTunnelSchema) -> Vec<ProxyProtocol> {
    schema.fields["proxy_protocol"]
        .variants
        .as_ref()
        .unwrap()
        .iter()
        .map(|mode| serde_json::from_value(Value::String(mode.clone())).unwrap())
        .collect()
}

#[test]
fn release_schema_supports_packet_mode_and_limits_nethernet_to_it() {
    let release: Value =
        serde_json::from_str(include_str!("../../../agent-schema-release.json")).unwrap();
    let schema: AgentSchema = serde_json::from_value(release["schema"].clone()).unwrap();

    assert_eq!(
        proxy_modes(schema.default_schema.as_ref().unwrap()),
        [
            ProxyProtocol::ProxyProtocolV1,
            ProxyProtocol::ProxyProtocolV2,
            ProxyProtocol::ProxyProtocolV2Pkt,
        ]
    );
    let nethernet = schema
        .schemas
        .iter()
        .find(|entry| {
            matches!(
                entry.tunnel_type,
                AgentSchemaTunnelType::TunnelType(TunnelType::NetherNet)
            )
        })
        .unwrap()
        .schema
        .as_ref()
        .unwrap();
    assert_eq!(proxy_modes(nethernet), [ProxyProtocol::ProxyProtocolV2Pkt]);
    assert!(nethernet.fields["proxy_protocol"].allow_null);
    assert!(nethernet.fields["proxy_protocol"].default_value.is_none());

    let version = format!(
        "{}.{}.{}",
        release["version_major"], release["version_minor"], release["version_patch"]
    );
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        release["release_url"],
        format!("https://github.com/playit-cloud/playit-agent/releases/tag/v{version}")
    );
}
