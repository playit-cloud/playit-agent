use std::net::SocketAddr;

use tokio::net::lookup_host;

pub async fn address_lookup(name: &str, default_port: u16) -> Vec<SocketAddr> {
    if let Ok(addr) = name.parse::<SocketAddr>() {
        return vec![addr];
    }

    let mut parts = name.split(':');
    let ip_part = match parts.next() {
        Some(v) => v,
        _ => return vec![],
    };

    let port_number = parts
        .next()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(default_port);

    if parts.next().is_some() {
        return vec![];
    }

    ip_lookup(&format!("{}:{}", ip_part, port_number)).await
}

async fn ip_lookup(name: &str) -> Vec<SocketAddr> {
    let iter = match lookup_host(name).await {
        Ok(v) => v,
        Err(error) => {
            tracing::error!(?error, %name, "failed to perform hostname lookup");
            return vec![];
        }
    };

    iter.collect()
}

#[cfg(test)]
mod test {
    use super::*;
    use tracing::Level;

    #[tokio::test]
    async fn test_lookup() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .try_init();
        assert!(!address_lookup("control.playit.gg", 5523).await.is_empty());
        assert!(!address_lookup("ping.playit.gg", 5523).await.is_empty());
    }
}
