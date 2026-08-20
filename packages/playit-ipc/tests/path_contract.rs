use std::path::Path;

use playit_ipc::endpoint::IpcEndpoint;
use playit_ipc::ipc::{get_default_endpoint, get_default_socket_path};

#[test]
fn endpoint_parsing_preserves_filesystem_and_namespace_forms() {
    let filesystem = IpcEndpoint::parse("./run/playitd.sock");
    assert_eq!(filesystem.as_display_path(), "./run/playitd.sock");
    assert_eq!(
        filesystem.filesystem_path(),
        Some(Path::new("./run/playitd.sock"))
    );
    assert!(filesystem.is_filesystem());

    let namespaced = IpcEndpoint::parse("@playitd");
    assert_eq!(namespaced.as_display_path(), "@playitd");
    assert_eq!(namespaced.filesystem_path(), None);
    assert!(!namespaced.is_filesystem());
}

#[test]
fn default_endpoint_matches_default_socket_path() {
    assert_eq!(
        get_default_endpoint().as_display_path(),
        get_default_socket_path()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_installed_socket_path_is_stable() {
    assert_eq!(get_default_socket_path(), "/run/playit/playitd.sock");
}

#[cfg(target_os = "windows")]
#[test]
fn windows_installed_pipe_path_is_stable() {
    assert_eq!(get_default_socket_path(), r"\\.\pipe\playitd-system");
    assert!(get_default_endpoint().is_windows_named_pipe());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_socket_stays_below_the_playit_config_directory() {
    assert_eq!(
        Path::new(get_default_socket_path()),
        playit_platform::paths::playit_config_dir().join("playitd.sock")
    );
}
