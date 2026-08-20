#[cfg(target_os = "linux")]
#[test]
fn linux_service_definitions_preserve_installed_paths() {
    let systemd = include_str!("../../../linux/playit.service");
    let openrc = include_str!("../../../linux/playit.openrc");
    let expected_arguments = "--secret-path /etc/playit/playit.toml --socket-path /run/playit/playitd.sock -l /var/log/playit/playit.log";

    assert!(
        systemd
            .lines()
            .any(|line| { line == format!("ExecStart=/opt/playit/playitd {expected_arguments}") })
    );
    assert!(
        openrc
            .lines()
            .any(|line| line == format!("command_args=\"{expected_arguments}\""))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_package_manifests_preserve_executable_destinations() {
    for manifest in [
        include_str!("../../../build-scripts/nfpm.yaml"),
        include_str!("../../../build-scripts/nfpm-openrc.yaml"),
    ] {
        assert!(manifest.contains("dst: /opt/playit/agent"));
        assert!(manifest.contains("dst: /opt/playit/playitd"));
        assert!(manifest.contains("dst: /usr/bin/playit"));
        assert!(manifest.contains("dst: /usr/bin/playitd"));
        assert!(manifest.contains("dst: /etc/playit"));
    }
}

#[cfg(target_os = "windows")]
#[test]
fn windows_service_paths_and_label_are_stable() {
    use std::path::Path;

    assert_eq!(playit_platform::paths::WINDOWS_SERVICE_NAME, "playitd");
    assert!(playit_platform::paths::windows_service_data_dir().ends_with("playit_gg"));
    assert!(
        playit_platform::paths::windows_service_secret_path()
            .ends_with(Path::new("playit_gg").join("playit.toml"))
    );
    assert!(
        playit_platform::paths::windows_service_log_path()
            .ends_with(Path::new("playit_gg").join("logs").join("playitd.log"))
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_launch_agent_paths_are_stable() {
    assert_eq!(
        playit_platform::paths::macos_launch_agent_secret_path(),
        playit_platform::paths::macos_launch_agent_data_dir().join("playit.toml")
    );
    assert_eq!(
        playit_platform::paths::macos_launch_agent_socket_path(),
        playit_platform::paths::macos_launch_agent_data_dir().join("playitd.sock")
    );
    assert!(
        playit_platform::paths::macos_launch_agent_log_path()
            .ends_with("Library/Logs/playit/playitd.log")
    );
}
