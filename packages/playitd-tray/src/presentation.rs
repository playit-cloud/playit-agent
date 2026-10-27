//! Turns the service state into what the tray shows. Kept free of Windows APIs so it can
//! be unit tested on any platform.

use playit_ipc::model::{AccountStatus, AgentLifecycle};

/// Windows truncates tray tooltips at 127 characters.
const MAX_TOOLTIP_CHARS: usize = 120;
const MAX_STATUS_CHARS: usize = 80;

#[derive(Clone, Debug)]
pub enum ServiceView {
    NotInstalled,
    Stopped,
    Starting,
    Stopping,
    /// The service is running and answered over IPC.
    Agent(AgentLifecycle),
    /// The service state could not be read.
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayPresentation {
    pub tooltip: String,
    pub status_line: String,
    /// The agent is online; the tray shows the colored icon.
    pub online: bool,
    /// The primary action should read "Finish Setup" rather than "Open playit".
    pub needs_setup: bool,
    pub can_start: bool,
    pub can_stop: bool,
    pub can_reset: bool,
}

impl TrayPresentation {
    pub fn from_view(view: &ServiceView) -> Self {
        let base = Self {
            tooltip: String::new(),
            status_line: String::new(),
            online: false,
            needs_setup: false,
            can_start: false,
            can_stop: false,
            can_reset: false,
        };

        let (status, presentation) = match view {
            ServiceView::NotInstalled => ("Background service is not installed".to_string(), base),
            ServiceView::Stopped => (
                "Stopped".to_string(),
                Self {
                    can_start: true,
                    ..base
                },
            ),
            ServiceView::Starting => ("Starting…".to_string(), base),
            ServiceView::Stopping => ("Stopping…".to_string(), base),
            ServiceView::Unknown(reason) => (format!("Status unavailable: {reason}"), base),
            ServiceView::Agent(lifecycle) => {
                let running = Self {
                    can_stop: true,
                    can_reset: true,
                    ..base.clone()
                };
                match lifecycle {
                    AgentLifecycle::WaitingForSecret => (
                        "Setup required".to_string(),
                        Self {
                            needs_setup: true,
                            can_reset: false,
                            ..running
                        },
                    ),
                    AgentLifecycle::HasInvalidSecret(_) => (
                        "Invalid secret - reset to set up again".to_string(),
                        running,
                    ),
                    AgentLifecycle::DisabledOverLimit(_) => {
                        ("Account is over its agent limit".to_string(), running)
                    }
                    AgentLifecycle::Starting => ("Connecting…".to_string(), running),
                    AgentLifecycle::Stopping => ("Stopping…".to_string(), base),
                    AgentLifecycle::Error(error) => (format!("Error: {}", error.message), running),
                    AgentLifecycle::Running(state) => {
                        let disabled = state
                            .tunnels
                            .iter()
                            .filter(|tunnel| tunnel.is_disabled)
                            .count();
                        let enabled = state.tunnels.len() - disabled;
                        let mut tunnels = format!("{enabled} enabled, {disabled} disabled");
                        if !state.pending_tunnels.is_empty() {
                            tunnels.push_str(&format!(", {} pending", state.pending_tunnels.len()));
                        }
                        let guest = if matches!(state.account_status, AccountStatus::Guest) {
                            " (guest account)"
                        } else {
                            ""
                        };
                        (
                            format!("Agent online - {tunnels}{guest}"),
                            Self {
                                online: true,
                                ..running
                            },
                        )
                    }
                }
            }
        };

        Self {
            tooltip: truncate(&format!("playit: {status}"), MAX_TOOLTIP_CHARS),
            status_line: truncate(&status, MAX_STATUS_CHARS),
            ..presentation
        }
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    let single_line = text.replace(['\r', '\n'], " ");
    if single_line.chars().count() <= max_chars {
        return single_line;
    }
    let mut truncated = single_line
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use playit_ipc::model::{AgentState, ServiceError, TunnelState};

    #[test]
    fn running_agent_is_online_with_tunnel_count() {
        let view = ServiceView::Agent(AgentLifecycle::Running(AgentState {
            tunnels: vec![TunnelState::default(), TunnelState::default()],
            account_status: AccountStatus::Verified,
            ..AgentState::default()
        }));

        let presentation = TrayPresentation::from_view(&view);

        assert!(presentation.online);
        assert_eq!(
            presentation.tooltip,
            "playit: Agent online - 2 enabled, 0 disabled"
        );
        assert!(presentation.can_stop);
        assert!(presentation.can_reset);
        assert!(!presentation.can_start);
    }

    #[test]
    fn disabled_tunnels_are_not_counted_as_enabled() {
        let presentation =
            TrayPresentation::from_view(&ServiceView::Agent(AgentLifecycle::Running(AgentState {
                tunnels: vec![
                    TunnelState {
                        is_disabled: true,
                        ..TunnelState::default()
                    };
                    2
                ],
                ..AgentState::default()
            })));
        assert!(presentation.online);
        assert_eq!(
            presentation.status_line,
            "Agent online - 0 enabled, 2 disabled"
        );
    }

    #[test]
    fn waiting_for_secret_asks_for_setup_and_hides_reset() {
        let presentation =
            TrayPresentation::from_view(&ServiceView::Agent(AgentLifecycle::WaitingForSecret));

        assert!(presentation.needs_setup);
        assert!(!presentation.can_reset);
        assert!(!presentation.online);
    }

    #[test]
    fn stopped_service_can_be_started() {
        let presentation = TrayPresentation::from_view(&ServiceView::Stopped);

        assert!(presentation.can_start);
        assert!(!presentation.can_stop);
    }

    #[test]
    fn long_errors_are_truncated_to_one_tooltip_line() {
        let view = ServiceView::Agent(AgentLifecycle::Error(ServiceError {
            message: format!("first line\n{}", "x".repeat(300)),
            ..ServiceError::default()
        }));

        let presentation = TrayPresentation::from_view(&view);

        assert!(presentation.tooltip.chars().count() <= MAX_TOOLTIP_CHARS);
        assert!(presentation.status_line.chars().count() <= MAX_STATUS_CHARS);
        assert!(!presentation.tooltip.contains('\n'));
        assert!(presentation.tooltip.ends_with('…'));
    }
}
