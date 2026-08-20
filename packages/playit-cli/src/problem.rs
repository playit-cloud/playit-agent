use playit_ipc::ipc::IpcError;
use playit_ipc::model::{
    AgentLifecycle, ServiceError, ServiceErrorCode, ServicePhase, ServiceStatus,
};

const ACCOUNT_AGENTS_URL: &str = "https://playit.gg/account/agents";
const ACCOUNT_UPGRADE_URL: &str = "https://playit.gg/account/upgrade";

pub fn render_ipc_error(error: &IpcError) -> String {
    match error {
        IpcError::Service(problem) => render_problem(problem),
        _ => error.to_string(),
    }
}

pub fn render_problem(problem: &ServiceError) -> String {
    match problem.meaning().code.as_str() {
        "agent_disabled_over_limit" => format!(
            "{}\n{}",
            over_limit_title(),
            over_limit_guidance()
        ),
        "invalid_secret" => "The playit service has an invalid secret. Run `playit reset`, then run setup again."
            .to_owned(),
        "secret_pinned" => "This command is unavailable while playitd uses an inline --secret. Restart it with a secret file to make setup changes."
            .to_owned(),
        "secret_write_failed" => {
            format!("The playit service could not save its secret: {}", problem.message)
        }
        "engine_unavailable" | "startup_failed" => {
            "The playit service is not ready yet. Try again after it finishes starting.".to_owned()
        }
        "catalog_unavailable" => {
            "Tunnel status could not be refreshed. The last accepted tunnel list is still active."
                .to_owned()
        }
        "shutdown_timed_out" => {
            "The playit service did not stop before its shutdown deadline.".to_owned()
        }
        _ => problem.message.clone(),
    }
}

pub fn render_problem_code(code: ServiceErrorCode) -> String {
    let error = ServiceError {
        code,
        message: String::new(),
        retryable: false,
        details: None,
    };
    render_problem(&error)
}

pub fn lifecycle_message(lifecycle: &AgentLifecycle) -> Option<String> {
    match lifecycle {
        AgentLifecycle::Running(_) => None,
        AgentLifecycle::WaitingForSecret => {
            Some("The playit service is waiting for setup to finish.".to_owned())
        }
        AgentLifecycle::HasInvalidSecret(error)
        | AgentLifecycle::DisabledOverLimit(error)
        | AgentLifecycle::Error(error) => Some(render_problem(error)),
        AgentLifecycle::Starting => Some("The playit service is starting...".to_owned()),
        AgentLifecycle::Stopping => Some("The playit service is stopping...".to_owned()),
    }
}

pub fn status_message(status: &ServiceStatus) -> Option<String> {
    if let Some(error) = &status.last_error {
        return Some(render_problem(error));
    }
    if matches!(status.phase, ServicePhase::DisabledOverLimit) {
        return Some(render_problem_code(
            ServiceErrorCode::AgentDisabledOverLimit,
        ));
    }
    if matches!(status.phase, ServicePhase::Running) {
        None
    } else {
        Some(format!(
            "playit service status: {}",
            service_phase_label(&status.phase)
        ))
    }
}

pub fn service_phase_label(phase: &ServicePhase) -> &'static str {
    match phase {
        ServicePhase::WaitingForSecret => "waiting for secret",
        ServicePhase::HasInvalidSecret => "invalid secret",
        ServicePhase::DisabledOverLimit => "disabled over limit",
        ServicePhase::Starting => "starting",
        ServicePhase::Running => "running",
        ServicePhase::Stopping => "stopping",
        ServicePhase::Error => "error",
    }
}

pub fn over_limit_guidance() -> String {
    format!(
        "Delete unused agents: {ACCOUNT_AGENTS_URL}\nIncrease your agent limit: {ACCOUNT_UPGRADE_URL}"
    )
}

pub fn over_limit_title() -> &'static str {
    "The playit service cannot start because this account is over the agent limit."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_wire_problem_has_one_meaning_for_every_client_surface() {
        let surfaces = ["cli", "tui", "stdout", "tray"];
        for code in playit_model::ProblemCode::ALL {
            let metadata = code.metadata();
            let problem = ServiceError {
                code: ServiceErrorCode::Internal,
                message: "fixture diagnostic".to_owned(),
                retryable: false,
                details: Some(serde_json::json!({
                    "problem_code": code.as_str(),
                    "retry": metadata.retry.as_str(),
                    "action": metadata.action.as_str(),
                })),
            };
            let meaning = problem.meaning();
            let rendered = render_problem(&problem);
            assert!(!meaning.code.is_empty());
            assert!(!meaning.retry.is_empty());
            assert!(!meaning.action.is_empty());
            assert!(!rendered.is_empty());
            assert_eq!(meaning.code, code.as_str());
            assert_eq!(meaning.retry, metadata.retry.as_str());
            assert_eq!(meaning.action, metadata.action.as_str());
            for _surface in surfaces {
                assert_eq!(problem.meaning(), meaning);
            }
        }
    }
}
