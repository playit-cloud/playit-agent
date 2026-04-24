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

#[cfg(test)]
mod tests {
    use super::IpcEndpoint;
    use std::path::Path;

    #[test]
    fn parses_filesystem_socket() {
        assert_eq!(
            IpcEndpoint::parse("/var/run/playitd.sock"),
            IpcEndpoint::Filesystem("/var/run/playitd.sock".into())
        );
    }

    #[test]
    fn parses_namespaced_socket() {
        assert_eq!(
            IpcEndpoint::parse("@playitd"),
            IpcEndpoint::Namespaced("playitd".to_string())
        );
    }

    #[test]
    fn leaves_windows_pipe_as_filesystem_style_endpoint() {
        let endpoint = IpcEndpoint::parse(r"\\.\pipe\playitd-system");
        assert_eq!(
            endpoint.filesystem_path(),
            Some(Path::new(r"\\.\pipe\playitd-system"))
        );
    }

    #[test]
    fn display_path_restores_namespaced_prefix() {
        assert_eq!(
            IpcEndpoint::Namespaced("playitd".to_string()).as_display_path(),
            "@playitd"
        );
    }
}
