pub mod api_client;
pub mod agent_config;
pub mod dependent_task;
pub mod tcp_client;
pub mod tunnel_client;
pub mod udp_client;
pub mod lan_address;
pub mod events;
pub mod tracked_task;
pub mod application;
pub mod ping_task;

pub fn now_milli() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod test {
    use tokio::net::{lookup_host, TcpStream};
    use tokio::net::ToSocketAddrs;

    #[tokio::test]
    async fn test() {
        let hosts = lookup_host("numbers-charms.at.playit.gg:1234").await.unwrap();
        for host in hosts {
            println!("peer addr: {:?}", host);
        }
    }
}