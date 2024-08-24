use std::time::Duration;

use playit_api_client::{http_client::HttpClient, PlayitApi};
use playit_ping_monitor::PingMonitor;
use rand::random;
use serde::{Deserialize, Serialize};

#[tokio::main]
async fn main() {
    let (non_blocking, _guard) = tracing_appender::non_blocking(std::io::stdout());
    tracing_subscriber::fmt().with_writer(non_blocking).init();

    let playit_secret = {
        let content = 'load_secret: {
            if let Ok(secret) = tokio::fs::read_to_string("playit.toml").await {
                break 'load_secret Some(secret);
            }

            let config_path = dirs::config_local_dir();
            if let Some(path) = config_path {
                let config_root = path.to_string_lossy();
                let config_file = format!("{}/playit_gg/playit.toml", config_root);

                if let Ok(secret) = tokio::fs::read_to_string(&config_file).await {
                    break 'load_secret Some(secret);
                }
            }

            #[cfg(target_os = "linux")] {
                let old_path = "/etc/playit/playit.toml";
                if let Ok(secret) = tokio::fs::read_to_string(old_path).await {
                    break 'load_secret Some(secret);
                }
            }

            None
        };

        content
            .and_then(|s| toml::from_str::<Config>(&s).ok())
            .and_then(|c| {
                hex::decode(&c.secret_key).ok()?;
                Some(c.secret_key)
            })
    };

    let mut ping_monitor = PingMonitor::new(PlayitApi::new(HttpClient::new(
        "https://api.playit.gg".to_string(),
        playit_secret,
    ))).await.unwrap();

    loop {
        if let Err(error) = ping_monitor.refresh().await {
            tracing::error!(?error, "error running ping monitor");
        }
        tokio::time::sleep(Duration::from_millis(3_000 + (random::<u64>() % 5_000))).await;
    }
}

#[derive(Deserialize, Serialize)]
struct Config {
    secret_key: String,
}
