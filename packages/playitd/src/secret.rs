use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(unix)]
use playit_agent_core::utils::now_milli;
use playit_ipc::model::{ServiceError, ServiceErrorCode};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::errors::SecretError;

#[derive(Debug, Clone)]
enum SecretSource {
    Pinned(String),
    File(PathBuf),
}

#[derive(Debug, Clone)]
pub(crate) enum LoadedSecret {
    Ready(String),
    Missing,
    Invalid(String),
}

pub(crate) struct SecretProvisionRequest {
    secret: String,
    response_tx: oneshot::Sender<Result<(), String>>,
}

pub(crate) enum SecretProvisioning {
    Pinned,
    File(mpsc::Receiver<SecretProvisionRequest>),
}

pub(crate) struct SecretStore {
    source: SecretSource,
    provision_tx: Option<mpsc::Sender<SecretProvisionRequest>>,
}

impl SecretStore {
    pub(crate) fn from_options(
        secret: Option<String>,
        secret_path: Option<PathBuf>,
    ) -> (Arc<Self>, SecretProvisioning) {
        match secret {
            Some(secret) => (
                Arc::new(Self {
                    source: SecretSource::Pinned(secret),
                    provision_tx: None,
                }),
                SecretProvisioning::Pinned,
            ),
            None => {
                let path = secret_path.unwrap_or_else(crate::paths::default_secret_path);
                let (provision_tx, provision_rx) = mpsc::channel(8);
                (
                    Arc::new(Self {
                        source: SecretSource::File(path),
                        provision_tx: Some(provision_tx),
                    }),
                    SecretProvisioning::File(provision_rx),
                )
            }
        }
    }

    pub(crate) fn path(&self) -> Option<&Path> {
        match &self.source {
            SecretSource::Pinned(_) => None,
            SecretSource::File(path) => Some(path),
        }
    }

    pub(crate) fn supports_provisioning(&self) -> bool {
        matches!(self.source, SecretSource::File(_))
    }

    pub(crate) async fn load(&self) -> LoadedSecret {
        match &self.source {
            SecretSource::Pinned(secret) => match validate_secret(secret.trim()) {
                Ok(secret) => LoadedSecret::Ready(secret),
                Err(error) => {
                    LoadedSecret::Invalid(format!("Invalid secret passed via --secret: {error}"))
                }
            },
            SecretSource::File(path) => load_secret_from_path(path).await,
        }
    }

    pub(crate) async fn provision(&self, secret: String) -> Result<(), ServiceError> {
        let Some(provision_tx) = &self.provision_tx else {
            return Err(service_error(
                ServiceErrorCode::SecretPinned,
                "Secret provisioning is unavailable because playitd was started with --secret."
                    .to_string(),
                false,
            ));
        };

        let (response_tx, response_rx) = oneshot::channel();
        provision_tx
            .send(SecretProvisionRequest {
                secret,
                response_tx,
            })
            .await
            .map_err(|_| provisioning_unavailable_error())?;

        match response_rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => Err(service_error(
                ServiceErrorCode::SecretWriteFailed,
                message,
                true,
            )),
            Err(_) => Err(provisioning_unavailable_error()),
        }
    }

    pub(crate) async fn reset(&self) -> Result<String, ServiceError> {
        let SecretSource::File(path) = &self.source else {
            return Err(service_error(
                ServiceErrorCode::SecretPinned,
                "Secret reset is unavailable because playitd was started with --secret."
                    .to_string(),
                false,
            ));
        };

        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(format!(
                "Deleted secret file at {}. Restart playitd to reprovision a new secret.",
                path.display()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(format!(
                "Secret file was already absent at {}.",
                path.display()
            )),
            Err(error) => Err(service_error(
                ServiceErrorCode::SecretWriteFailed,
                format!("Failed to delete secret file {}: {error}", path.display()),
                true,
            )),
        }
    }
}

impl SecretProvisioning {
    pub(crate) async fn wait(
        &mut self,
        store: &SecretStore,
        cancel_token: &CancellationToken,
    ) -> Result<Option<String>, SecretError> {
        let Self::File(provision_rx) = self else {
            return Err(SecretError(
                "Secret provisioning is unavailable for a pinned secret".to_string(),
            ));
        };
        let Some(secret_path) = store.path() else {
            return Err(SecretError(
                "File-backed secret provisioning has no secret path".to_string(),
            ));
        };

        tracing::info!(
            secret_path = %secret_path.display(),
            "Waiting for frontend secret provisioning over IPC"
        );

        loop {
            tokio::select! {
                maybe_request = provision_rx.recv() => {
                    let Some(request) = maybe_request else {
                        return Err(SecretError("Secret provisioning channel closed".to_string()));
                    };

                    let result = persist_secret_file(secret_path, &request.secret).await;
                    let acknowledgement =
                        result.as_ref().map(|_| ()).map_err(ToString::to_string);
                    let _ = request.response_tx.send(acknowledgement);

                    match result {
                        Ok(()) => {
                            tracing::info!(
                                secret_path = %secret_path.display(),
                                "Secret provisioned successfully"
                            );
                            return Ok(Some(request.secret));
                        }
                        Err(error) => {
                            tracing::error!(secret_path = %secret_path.display(), "{error}");
                        }
                    }
                }
                _ = cancel_token.cancelled() => return Ok(None),
            }
        }
    }
}

async fn load_secret_from_path(path: &Path) -> LoadedSecret {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return LoadedSecret::Missing,
        Err(error) => {
            return LoadedSecret::Invalid(format!(
                "Failed to read secret file {}: {error}",
                path.display()
            ));
        }
    };

    match parse_secret_file(&content) {
        Ok(secret) => LoadedSecret::Ready(secret),
        Err(()) => LoadedSecret::Invalid(format!(
            "Invalid secret file at {}. Remove or replace it with a valid secret.",
            path.display()
        )),
    }
}

async fn persist_secret_file(path: &Path, secret: &str) -> Result<(), SecretError> {
    let secret = validate_secret(secret.trim())?;

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            SecretError(format!(
                "Failed to create secret directory {}: {error}",
                parent.display()
            ))
        })?;
    }

    let content = if path.extension().and_then(|extension| extension.to_str()) == Some("toml") {
        toml::to_string(&SecretConfig {
            secret_key: secret.clone(),
        })
        .map_err(|error| {
            SecretError(format!(
                "Failed to serialize secret file {}: {error}",
                path.display()
            ))
        })?
    } else {
        secret
    };

    secure_write_secret_file(path, content.as_bytes()).await
}

#[cfg(unix)]
async fn secure_write_secret_file(path: &Path, content: &[u8]) -> Result<(), SecretError> {
    let path = path.to_path_buf();
    let content = content.to_vec();

    tokio::task::spawn_blocking(move || secure_write_secret_file_blocking(&path, &content))
        .await
        .map_err(|error| SecretError(format!("Failed to join secret file writer task: {error}")))?
}

#[cfg(unix)]
fn secure_write_secret_file_blocking(path: &Path, content: &[u8]) -> Result<(), SecretError> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("playit.toml");
    let temporary_path = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        now_milli()
    ));

    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary_path)
            .map_err(|error| {
                SecretError(format!(
                    "Failed to create temporary secret file {}: {error}",
                    temporary_path.display()
                ))
            })?;

        file.write_all(content).map_err(|error| {
            SecretError(format!(
                "Failed to write temporary secret file {}: {error}",
                temporary_path.display()
            ))
        })?;
        file.sync_all().map_err(|error| {
            SecretError(format!(
                "Failed to sync temporary secret file {}: {error}",
                temporary_path.display()
            ))
        })?;
        drop(file);

        std::fs::rename(&temporary_path, path).map_err(|error| {
            SecretError(format!(
                "Failed to replace secret file {} with {}: {error}",
                path.display(),
                temporary_path.display()
            ))
        })?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| {
                SecretError(format!(
                    "Failed to set secret file permissions on {}: {error}",
                    path.display()
                ))
            },
        )?;

        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }

    result
}

#[cfg(not(unix))]
async fn secure_write_secret_file(path: &Path, content: &[u8]) -> Result<(), SecretError> {
    tokio::fs::write(path, content).await.map_err(|error| {
        SecretError(format!(
            "Failed to write secret file {}: {error}",
            path.display()
        ))
    })
}

fn parse_secret_file(content: &str) -> Result<String, ()> {
    let trimmed = content.trim();
    if let Ok(secret) = validate_secret(trimmed) {
        return Ok(secret);
    }

    let config = toml::from_str::<SecretConfig>(content).map_err(|_| ())?;
    validate_secret(config.secret_key.trim()).map_err(|_| ())
}

fn validate_secret(secret: &str) -> Result<String, SecretError> {
    hex::decode(secret)
        .map(|_| secret.to_string())
        .map_err(|_| {
            SecretError(
                "The secret is not valid. It should be the key generated by playit setup."
                    .to_string(),
            )
        })
}

fn service_error(code: ServiceErrorCode, message: String, retryable: bool) -> ServiceError {
    ServiceError {
        code,
        message,
        retryable,
        details: None,
    }
}

fn provisioning_unavailable_error() -> ServiceError {
    service_error(
        ServiceErrorCode::ProvisioningUnavailable,
        "playitd is no longer waiting for secret provisioning".to_string(),
        true,
    )
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SecretConfig {
    secret_key: String,
}
