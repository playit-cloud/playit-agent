//! Tokio side of the window: keeps a live subscription to playitd and runs commands that
//! need the network or their own IPC connection. Results flow back to GPUI as events.

use std::sync::Arc;
use std::time::Duration;

use playit_api_client::PlayitApi;
use playit_api_client::api::{
    ApiError, ClaimAgentType, ClaimSetupResponse, ReqClaimExchange, ReqClaimSetup,
};
use playit_ipc::ipc::IpcClient;
use playit_ipc::model::{ServiceUpdate, SubscribeResponse};
use rand::Rng;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

const RECONNECT_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum BackendEvent {
    Subscribed(Box<SubscribeResponse>),
    Update(ServiceUpdate),
    Unreachable {
        error: String,
    },
    Setup {
        url: Option<String>,
        step: SetupStep,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupStep {
    Preparing,
    WaitingForUserVisit,
    WaitingForUser,
    Approved,
    Rejected,
    Failed(String),
}

#[derive(Debug)]
pub enum UiCommand {
    StartSetup,
    ResetSecret,
    /// Open the guest account's one-click login link, which the daemon creates on request.
    OpenGuestLogin,
    /// Skip the remaining reconnect delay and try the daemon again now.
    Reconnect,
}

/// Results of commands the window has to act on, like opening a link or showing an error.
#[derive(Debug)]
pub enum CommandOutcome {
    OpenUrl(String),
    Failed(String),
}

pub fn spawn(
    runtime: &tokio::runtime::Handle,
    socket_path: String,
    events: UnboundedSender<BackendEvent>,
    commands: UnboundedReceiver<UiCommand>,
    outcomes: UnboundedSender<CommandOutcome>,
) {
    let reconnect = Arc::new(Notify::new());
    runtime.spawn(run_subscription(
        socket_path.clone(),
        events.clone(),
        reconnect.clone(),
    ));
    runtime.spawn(run_commands(
        socket_path,
        events,
        commands,
        outcomes,
        reconnect,
    ));
}

async fn run_subscription(
    socket_path: String,
    events: UnboundedSender<BackendEvent>,
    reconnect: Arc<Notify>,
) {
    loop {
        let error = match subscribe(&socket_path, &events).await {
            Ok(()) => return,
            Err(error) => error,
        };
        if events.send(BackendEvent::Unreachable { error }).is_err() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(RECONNECT_DELAY) => {}
            _ = reconnect.notified() => {}
        }
    }
}

/// Streams updates until the connection drops. Returns `Ok` only once the window is gone.
async fn subscribe(
    socket_path: &str,
    events: &UnboundedSender<BackendEvent>,
) -> Result<(), String> {
    let mut client = IpcClient::connect_with_path(socket_path)
        .await
        .map_err(|error| error.to_string())?;
    let snapshot = client
        .subscribe()
        .await
        .map_err(|error| error.to_string())?;
    if events
        .send(BackendEvent::Subscribed(Box::new(snapshot)))
        .is_err()
    {
        return Ok(());
    }

    loop {
        let update = client
            .recv_update()
            .await
            .map_err(|error| format!("Connection lost: {error}"))?;
        if events.send(BackendEvent::Update(update)).is_err() {
            return Ok(());
        }
    }
}

async fn run_commands(
    socket_path: String,
    events: UnboundedSender<BackendEvent>,
    mut commands: UnboundedReceiver<UiCommand>,
    outcomes: UnboundedSender<CommandOutcome>,
    reconnect: Arc<Notify>,
) {
    let mut setup: Option<JoinHandle<()>> = None;

    while let Some(command) = commands.recv().await {
        match command {
            UiCommand::StartSetup => {
                if let Some(previous) = setup.take() {
                    previous.abort();
                }
                setup = Some(tokio::spawn(run_setup(socket_path.clone(), events.clone())));
            }
            UiCommand::ResetSecret => {
                let socket_path = socket_path.clone();
                let outcomes = outcomes.clone();
                tokio::spawn(async move {
                    if let Err(error) = reset_secret(&socket_path).await {
                        let _ = outcomes.send(CommandOutcome::Failed(error));
                    }
                });
            }
            UiCommand::Reconnect => reconnect.notify_one(),
            UiCommand::OpenGuestLogin => {
                let socket_path = socket_path.clone();
                let outcomes = outcomes.clone();
                tokio::spawn(async move {
                    let _ = outcomes.send(match account_login_url(&socket_path).await {
                        Ok(url) => CommandOutcome::OpenUrl(url),
                        Err(error) => CommandOutcome::Failed(error),
                    });
                });
            }
        }
    }

    if let Some(setup) = setup {
        setup.abort();
    }
}

async fn reset_secret(socket_path: &str) -> Result<(), String> {
    let mut client = IpcClient::connect_with_path(socket_path)
        .await
        .map_err(|error| format!("Could not reach playit: {error}"))?;
    let response = client
        .reset_secret()
        .await
        .map_err(|error| format!("Failed to reset secret: {error}"))?;
    if response.accepted {
        Ok(())
    } else {
        Err(response
            .message
            .unwrap_or_else(|| "playitd rejected the reset request".to_string()))
    }
}

async fn account_login_url(socket_path: &str) -> Result<String, String> {
    let mut client = IpcClient::connect_with_path(socket_path)
        .await
        .map_err(|error| format!("Could not reach playit: {error}"))?;
    client
        .get_account_login_url()
        .await
        .map(|response| response.login_url)
        .map_err(|error| format!("Could not open your playit account: {error}"))
}

/// Claims a new agent in the browser and hands its secret to the daemon, mirroring
/// `playit setup` in the CLI.
async fn run_setup(socket_path: String, events: UnboundedSender<BackendEvent>) {
    let send = |url: Option<String>, step: SetupStep| {
        let _ = events.send(BackendEvent::Setup { url, step });
    };

    let mut code = [0u8; 5];
    rand::rng().fill(&mut code);
    let code = hex::encode(code);
    send(
        Some(format!("https://playit.gg/claim/{code}")),
        SetupStep::Preparing,
    );

    match claim_secret(&code, &send).await {
        Ok(Some(secret)) => {
            if let Err(error) = provision_secret(&socket_path, &secret).await {
                send(None, SetupStep::Failed(error));
            }
        }
        Ok(None) => send(None, SetupStep::Rejected),
        Err(error) => send(None, SetupStep::Failed(error)),
    }
}

/// Waits for the claim to be approved and returns its secret, or `None` if it was rejected.
async fn claim_secret(
    code: &str,
    send: &impl Fn(Option<String>, SetupStep),
) -> Result<Option<String>, String> {
    let api_base = std::env::var("API_BASE").unwrap_or_else(|_| "https://api.playit.gg".into());
    let api = PlayitApi::create(api_base, None);
    let mut last_step = SetupStep::Preparing;

    loop {
        let step = match api
            .claim_setup(ReqClaimSetup {
                code: code.to_string(),
                agent_type: ClaimAgentType::Assignable,
                version: format!("playit {}", env!("CARGO_PKG_VERSION")),
            })
            .await
        {
            Ok(ClaimSetupResponse::WaitingForUserVisit) => SetupStep::WaitingForUserVisit,
            Ok(ClaimSetupResponse::WaitingForUser) => SetupStep::WaitingForUser,
            Ok(ClaimSetupResponse::UserAccepted) => {
                send(None, SetupStep::Approved);
                break;
            }
            Ok(ClaimSetupResponse::UserRejected) => return Ok(None),
            Err(error) => {
                tracing::warn!(?error, "failed loading claim setup");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        if step != last_step {
            send(None, step.clone());
            last_step = step;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    loop {
        match api
            .claim_exchange(ReqClaimExchange {
                code: code.to_string(),
            })
            .await
        {
            Ok(response) => return Ok(Some(response.secret_key)),
            Err(ApiError::Fail(status)) => {
                tracing::info!(?status, "waiting for claim exchange");
            }
            Err(error) => return Err(format!("Failed to finish setup: {error}")),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn provision_secret(socket_path: &str, secret: &str) -> Result<(), String> {
    let mut client = IpcClient::connect_with_path(socket_path)
        .await
        .map_err(|error| format!("Could not reach playit: {error}"))?;
    let response = client
        .set_secret(secret)
        .await
        .map_err(|error| format!("Failed to save the agent secret: {error}"))?;
    if response.accepted {
        Ok(())
    } else {
        Err(response
            .message
            .unwrap_or_else(|| "playitd rejected the secret".to_string()))
    }
}
