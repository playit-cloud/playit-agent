use std::time::Duration;

use playit_api_client::{
    PlayitApi,
    api::{
        ApiError, ApiResponseError, ClaimAgentType, ClientRejection, DevicePairingStarted,
        PairingPollError, PairingPollResult, ReqPairingDevicePoll, ReqPairingDeviceStart,
    },
};

use crate::agent_control::version::client_info;

/// Pairing an install with a playit account: the server picks a short code, the install shows
/// it with the link the user must open, and polls until the user approves or denies it.
pub enum PairingStart {
    Started(DevicePairingStarted),
    /// The API predates device pairing, or does not know this build yet; use the claim flow
    /// (`/claim/setup` and `/claim/exchange`) instead.
    Unsupported,
    /// The API refuses to pair this build, e.g. because the release was blocked.
    Refused(ClientRejection),
}

pub enum PairingWait {
    /// The agent secret key.
    Approved(String),
    Denied,
    /// The code lapsed before the user approved it; start a new pairing.
    Expired,
}

/// Retries until the API answers.
pub async fn start_device_pairing(
    api: &PlayitApi,
    mode: ClaimAgentType,
    name_hint: Option<String>,
) -> PairingStart {
    loop {
        match api
            .pairing_device_start(ReqPairingDeviceStart {
                client: client_info(),
                mode_hint: Some(mode),
                name_hint: name_hint.clone(),
            })
            .await
        {
            Ok(started) => return PairingStart::Started(started),
            Err(ApiError::ApiError(ApiResponseError::PathNotFound(_))) => {
                return PairingStart::Unsupported;
            }
            Err(ApiError::Fail(
                ClientRejection::UnknownApp
                | ClientRejection::ReleaseNotRegistered
                | ClientRejection::InvalidPlatform,
            )) => return PairingStart::Unsupported,
            Err(ApiError::Fail(rejection)) => return PairingStart::Refused(rejection),
            Err(error) => {
                tracing::warn!(?error, "failed to start pairing");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

pub async fn wait_for_approval(api: &PlayitApi, started: &DevicePairingStarted) -> PairingWait {
    let mut interval = started.interval.max(1);

    loop {
        tokio::time::sleep(Duration::from_secs(interval as u64)).await;

        match api
            .pairing_device_poll(ReqPairingDevicePoll {
                device_code: started.device_code.clone(),
            })
            .await
        {
            Ok(PairingPollResult::Pending(pending)) => interval = pending.interval.max(1),
            Ok(PairingPollResult::Approved(key)) => return PairingWait::Approved(key.secret_key),
            Err(ApiError::Fail(PairingPollError::SlowDown)) => interval += 5,
            Err(ApiError::Fail(PairingPollError::Denied)) => return PairingWait::Denied,
            Err(ApiError::Fail(
                PairingPollError::Expired
                | PairingPollError::CodeNotFound
                | PairingPollError::AlreadyRedeemed,
            )) => return PairingWait::Expired,
            Err(error) => tracing::warn!(?error, "failed to poll pairing"),
        }
    }
}
