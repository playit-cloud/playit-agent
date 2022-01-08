use std::time::Duration;

use tokio::sync::mpsc::channel;
use tracing::Level;

use agent::api_client::ApiClient;
use agent::tunnel_client::TunnelClient;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_writer(std::io::stderr)
        .init();

    let api_client = ApiClient::new("https://api.playit.cloud/agent".to_string(), None);
    let (tx, _rx) = channel(1024);

    let client = TunnelClient::new(api_client, tx).await.unwrap();

    loop {
        match client.ping().await {
            Ok(pong) => {
                tracing::info!(
                    latency = pong.id,
                    tunnel_server_id = pong.tunnel_server_id,
                    "Got ping response"
                );
            }
            Err(error) => {
                tracing::error!(?error, "ping failed");
            }
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
