use std::{collections::HashMap, net::SocketAddr};

use playit_agent_proto::PortProto;
use playit_api_client::api::ProxyProtocol;
use tokio::sync::RwLock;

#[derive(Default)]
pub struct OriginLookup {
    map: RwLock<HashMap<Key, OriginResource>>,
}

impl OriginLookup {
    pub async fn update<I: Iterator<Item = OriginResource>>(&self, resources: I) {
        let mut lock = self.map.write().await;
        lock.clear();

        for res in resources {
            match res.proto {
                PortProto::Tcp => {
                    lock.insert(Key { tunnel_id: res.tunnel_id, is_tcp: true }, res);
                }
                PortProto::Udp => {
                    lock.insert(Key { tunnel_id: res.tunnel_id, is_tcp: false }, res);
                }
                PortProto::Both => {
                    lock.insert(Key { tunnel_id: res.tunnel_id, is_tcp: true }, res.clone());
                    lock.insert(Key { tunnel_id: res.tunnel_id, is_tcp: false }, res);
                }
            }
        }
    }

    pub async fn lookup(&self, tunnel_id: u64, is_tcp: bool) -> Option<OriginResource> {
        self.map.read().await.get(&Key { tunnel_id, is_tcp }).cloned()
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct Key {
    tunnel_id: u64,
    is_tcp: bool,
}

#[derive(Debug, Clone)]
pub struct OriginResource {
    pub tunnel_id: u64,
    pub proto: PortProto,
    pub local_addr: SocketAddr,
    pub port_count: u16,
    pub proxy_protocol: Option<ProxyProtocol>,
}

impl OriginResource {
    pub fn resolve_local(&self, port_offset: u16) -> Option<SocketAddr> {
        if port_offset == 0 {
            Some(self.local_addr)
        } else if port_offset < self.port_count {
            Some(SocketAddr::new(self.local_addr.ip(), self.local_addr.port() + port_offset))
        } else {
            None
        }
    }
}
