#[derive(Clone, Debug)]
pub(super) enum BackendRequest {
    RefreshStatus,
    StartService,
    StopService,
    ResetAgent,
    Shutdown,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum BackendRequestKind {
    RefreshStatus,
    StartService,
    StopService,
    ResetAgent,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ServiceStateSnapshot {
    pub(super) service_running: bool,
    pub(super) reset_agent_enabled: bool,
}

#[derive(Clone, Debug)]
pub(super) enum BackendResponse {
    RequestCompleted {
        request: BackendRequestKind,
        snapshot: ServiceStateSnapshot,
        error: Option<String>,
    },
}
