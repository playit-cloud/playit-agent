use std::net::IpAddr;
use std::sync::Arc;

use playit_api_client::api::PortType;

#[derive(Debug)]
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
