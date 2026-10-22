use crate::presentation::ServiceView;

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

#[derive(Clone, Debug)]
pub(super) enum BackendResponse {
    RequestCompleted {
        request: BackendRequestKind,
        view: ServiceView,
        error: Option<String>,
    },
}
