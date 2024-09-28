use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use playit_api_client::api::{PortType, ProxyProtocol};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct AddressValue<V> {
    pub value: V,
    pub from_port: u16,
    pub to_port: u16,
}

pub trait AddressLookup: 'static + Send + Sync {
    type Value: 'static + Send + Sync;

    fn lookup(&self, ip: IpAddr, port: u16, proto: PortType) -> Option<AddressValue<Self::Value>>;
}

impl<T: AddressLookup> AddressLookup for Arc<T> {
    type Value = T::Value;

    fn lookup(&self, ip: IpAddr, port: u16, proto: PortType) -> Option<AddressValue<Self::Value>> {
        T::lookup(&*self, ip, port, proto)
    }
}

#[derive(Clone, Debug)]
pub struct HostOrigin {
    pub tunnel_id: Uuid,
    pub host_addr: SocketAddr,
    pub use_special_lan: Option<bool>,
    pub proxy_protocol: Option<ProxyProtocol>,
}

impl std::fmt::Display for HostOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HostOrigin({}, special: {:?}, proxy: {:?})", self.host_addr, self.use_special_lan, self.proxy_protocol)
    }
}

impl Into<SocketAddr> for HostOrigin {
    fn into(self) -> SocketAddr {
        self.host_addr
    }
}

impl From<SocketAddr> for HostOrigin {
    fn from(value: SocketAddr) -> Self {
        HostOrigin {
            tunnel_id: Uuid::default(),
            host_addr: value,
            use_special_lan: None,
            proxy_protocol: None,
        }
    }
}
