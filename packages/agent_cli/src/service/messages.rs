use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlayitServiceRequest {
    Stop,
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "details")]
pub enum PlayitServiceResponse {
    ShuttingDown,
    Status(PlayitServiceStatus),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlayitServiceStatus {
    Running,
    FailingToLoadDataFromApi,
}
