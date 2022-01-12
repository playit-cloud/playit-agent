use tokio::io::{AsyncReadExt, AsyncWriteExt};

use messages::agent_config::AgentConfig;

pub async fn load_or_create() -> std::io::Result<Option<AgentConfig>> {
    match tokio::fs::File::open("./playit.toml").await {
        Ok(mut file) => {
            let mut data = Vec::new();
            file.read_to_end(&mut data).await?;

            let config = match toml::from_slice::<AgentConfig>(&data) {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "failed to parse ./playit.toml");
                    return Ok(None);
                }
            };

            Ok(Some(config))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut file = tokio::fs::File::create("./playit.toml").await?;

            file.write_all(
                toml::to_string(&AgentConfig {
                    api_url: None,
                    refresh_from_api: true,
                    secret_key: "put-secret-here".to_string(),
                    mappings: vec![],
                }).unwrap().as_bytes(),
            ).await?;

            Ok(None)
        }
        Err(e) => Err(e),
    }
}
