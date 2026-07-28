use std::time::Duration;

use playit_agent_core::utils::now_milli;
use playit_api_client::PlayitApi;
use tokio::sync::RwLock;

const CACHE_TTL: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct GuestLoginCache {
    value: RwLock<Option<(String, u64)>>,
}

impl GuestLoginCache {
    pub async fn get_or_create(&self, api: &PlayitApi) -> Result<String, String> {
        let now = now_milli();
        {
            let cache = self.value.read().await;
            if let Some((link, timestamp)) = &*cache
                && now.saturating_sub(*timestamp) < CACHE_TTL.as_millis() as u64
            {
                return Ok(link.clone());
            }
        }

        let session = api
            .login_guest()
            .await
            .map_err(|error| format!("{error:?}"))?;
        let link = format!(
            "https://playit.gg/login/guest-account/{}",
            session.session_key
        );
        *self.value.write().await = Some((link.clone(), now));
        Ok(link)
    }
}

#[cfg(test)]
mod tests {
    use super::CACHE_TTL;

    #[test]
    fn cache_ttl_is_fifteen_seconds() {
        assert_eq!(CACHE_TTL.as_secs(), 15);
    }
}
