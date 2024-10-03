use std::{borrow::Cow, fmt::Debug, time::Duration};

use playit_api_client::api::ClaimSetupResponse;
use serde::{Deserialize, Serialize};

use crate::{ui::UIMessage, CliError};

#[derive(Serialize, Deserialize, Debug)]
pub enum CliMessageType {
    Failed,
    Error,
    ClaimSetupStatus,
    AttentionNeeded,
    TunnelDetail,
}

#[derive(Serialize)]
pub struct CliUIMessage<T: CliMessage> {
    #[serde(rename = "type")]
    msg_type: CliMessageType,
    #[serde(flatten)]
    message: T,
}

impl<T: CliMessage> UIMessage for CliUIMessage<T> {
    fn is_fullscreen(&self) -> bool {
        match self.msg_type {
            CliMessageType::Failed => false,
            CliMessageType::Error => false,
            CliMessageType::ClaimSetupStatus => true,
            CliMessageType::AttentionNeeded => true,
            CliMessageType::TunnelDetail => false,
        }
    }

    fn write_json(&self) -> Option<String> {
        Some(serde_json::to_string(self).unwrap())
    }
    
    fn write_csv(&self) -> Option<String> {
        let csv = self.message.csv()?;
        Some(format!("{:?},{}", self.msg_type, csv))
    }
}

impl<T: CliMessage> std::fmt::Display for CliUIMessage<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.human(f)
    }
}

impl<T: CliMessage> From<T> for CliUIMessage<T> {
    fn from(value: T) -> Self {
        CliUIMessage {
            msg_type: T::TYPE,
            message: value
        }
    }
}

pub trait CliMessage: Serialize {
    const TYPE: CliMessageType;

    fn human_wait(&self) -> Option<Duration> {
        None
    }

    fn human(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result;

    fn csv(&self) -> Option<String>;
}

#[derive(Serialize)]
pub struct CliErrorPrint<E: Debug> {
    pub message: String,
    pub error: E,
}

impl<E: Debug + Serialize> CliMessage for CliErrorPrint<E> {
    const TYPE: CliMessageType = CliMessageType::Error;

    fn human_wait(&self) -> Option<Duration> {
        Some(Duration::from_secs(2))
    }
    
    fn human(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "\n-----ERROR-----\nMessage: {}\nDetails: {:?}\n-----ERROR-----\n", self.message, self.error)
    }
    
    fn csv(&self) -> Option<String> {
        let mut error_name = std::any::type_name::<E>();
        let mut error_typename = 'get_name: {
            if error_name.starts_with("&") {
                error_name = &error_name[1..];
            }

            if let Some(pos) = error_name.find("<") {
                if let Some(start) = error_name[..pos].rfind("::") {
                    break 'get_name &error_name[start + 2..pos];
                }
                break 'get_name &error_name[..pos];
            }

            if let Some(pos) = error_name.rfind("::") {
                break 'get_name &error_name[pos + 2..];
            }

            error_name
        };

        if error_typename.eq("str") || error_typename.eq("String") {
            error_typename = "CustomError";
        }

        Some(format!("{},{:?},{:?}", error_typename, self.message, format!("{:?}", self.error)))
    }
}

#[derive(Serialize)]
pub struct ClaimSetupStatus {
    pub status: ClaimSetupResponse,
    pub claim_url: String,
}

impl CliMessage for ClaimSetupStatus {
    const TYPE: CliMessageType = CliMessageType::ClaimSetupStatus;

    fn human(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            ClaimSetupResponse::WaitingForUserVisit => {
                write!(f, "Visit link to setup {}", self.claim_url)
            }
            ClaimSetupResponse::WaitingForUser => {
                write!(f, "Approve program at {}", self.claim_url)
            }
            ClaimSetupResponse::UserAccepted => {
                write!(f, "Program approved :). Secret code being setup.")
            }
            ClaimSetupResponse::UserRejected => {
                write!(f, "Program rejected :(")
            }
        }
    }

    fn csv(&self) -> Option<String> {
        Some(format!("{:?},{:?}", self.status, self.claim_url))
    }
}

pub struct ProgramFail {
    pub error: CliError,
}

impl CliMessage for ProgramFail {
    const TYPE: CliMessageType = CliMessageType::Failed;

    fn human(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "program failed\nreason:\n{:?}", self.error)
    }

    fn csv(&self) -> Option<String> {
        Some(format!("{:?}", self.error))
    }
}

impl Serialize for ProgramFail {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: serde::Serializer {
        serializer.serialize_str(&format!("{:?}", self.error))
    }
}

#[derive(Serialize)]
pub struct AttentionNeeded {
    pub note: String,
    pub url: String,
}

impl CliMessage for AttentionNeeded {
    const TYPE: CliMessageType = CliMessageType::AttentionNeeded;

    fn human_wait(&self) -> Option<Duration> {
        Some(Duration::from_secs(5))
    }

    fn human(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "---- Attention Needed ----\n{}\n{}\n--------------------------", self.note, self.url)
    }

    fn csv(&self) -> Option<String> {
        Some(format!("{},{}", self.note, self.url))
    }
}

#[derive(Serialize)]
pub struct TunnelDetails {
    pub detail: TunnelDetail,
    pub value: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize)]
pub enum TunnelDetail {
    Id,
    ManageUrl,
    Domain,
    PortStart,
    Address,
    Region,
}

impl CliMessage for TunnelDetails {
    const TYPE: CliMessageType = CliMessageType::TunnelDetail;

    fn human(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tunnel Detail ({:?}): {}", self.detail, self.value)
    }

    fn csv(&self) -> Option<String> {
        Some(format!("{:?},{}", self.detail, self.value))
    }
}