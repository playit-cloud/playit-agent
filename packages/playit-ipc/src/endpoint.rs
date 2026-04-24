use std::path::{Path, PathBuf};

use crate::paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcEndpoint {
    Filesystem(PathBuf),
    Namespaced(String),
}

impl IpcEndpoint {
    pub fn parse(value: impl Into<String>) -> Self {
        let value = value.into();
        if let Some(name) = value.strip_prefix('@') {
            return Self::Namespaced(name.to_string());
        }

        Self::Filesystem(PathBuf::from(value))
    }

    pub fn default() -> Self {
        Self::parse(paths::default_socket_path_string())
    }

    pub fn as_display_path(&self) -> String {
        match self {
            Self::Filesystem(path) => path.display().to_string(),
            Self::Namespaced(name) => format!("@{name}"),
        }
    }

    pub fn is_filesystem(&self) -> bool {
        matches!(self, Self::Filesystem(_))
    }

    pub fn is_windows_named_pipe(&self) -> bool {
        matches!(self, Self::Filesystem(path) if path.to_string_lossy().starts_with(r"\\.\pipe\"))
    }

    pub fn filesystem_path(&self) -> Option<&Path> {
        match self {
            Self::Filesystem(path) => Some(path.as_path()),
            Self::Namespaced(_) => None,
        }
    }
}

impl From<&str> for IpcEndpoint {
    fn from(value: &str) -> Self {
        Self::parse(value)
    }
}

impl From<String> for IpcEndpoint {
    fn from(value: String) -> Self {
        Self::parse(value)
    }
}
