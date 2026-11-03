//! "Open at Login" is a per-user LaunchAgent that starts the menu bar app with `--autostart`.
//! It is on by default. Turning it off removes the LaunchAgent and leaves a marker file so
//! the next launch does not add it back.

use std::path::{Path, PathBuf};

use playitd::macos::{APP_BUNDLE_ID, user_launch_agent_path, xml_escape};

const LABEL: &str = "gg.playit.tray";

pub(super) fn enabled() -> bool {
    !opt_out_marker_path().exists()
}

/// Points the LaunchAgent at this executable, or removes it if the user opted out. The job
/// is not loaded now, since that would start a second copy; launchd loads it at next login.
pub(super) fn sync() -> Result<(), String> {
    let plist_path = user_launch_agent_path(LABEL);
    if !enabled() {
        return remove_file_if_present(&plist_path);
    }

    let exe = std::env::current_exe()
        .map_err(|error| format!("Could not find the playit app: {error}"))?;
    let contents = plist(&exe);
    if std::fs::read_to_string(&plist_path).ok().as_deref() == Some(contents.as_str()) {
        return Ok(());
    }

    std::fs::create_dir_all(playitd::macos_launch_agent_log_dir())
        .map_err(|error| format!("Could not create the logs folder: {error}"))?;
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    std::fs::write(&plist_path, contents)
        .map_err(|error| format!("Could not write {}: {error}", plist_path.display()))
}

pub(super) fn set_enabled(enabled: bool) -> Result<(), String> {
    let marker = opt_out_marker_path();
    if enabled {
        remove_file_if_present(&marker)?;
    } else {
        if let Some(parent) = marker.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        std::fs::write(&marker, b"")
            .map_err(|error| format!("Could not save the Open at Login setting: {error}"))?;
    }
    sync()
}

fn opt_out_marker_path() -> PathBuf {
    playit_ipc::paths::playit_config_dir().join("tray-login-disabled")
}

fn remove_file_if_present(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not remove {}: {error}", path.display())),
    }
}

fn plist(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>--autostart</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
    <key>AssociatedBundleIdentifiers</key>
    <string>{APP_BUNDLE_ID}</string>
</dict>
</plist>
"#,
        exe = xml_escape(&exe.to_string_lossy()),
        stderr = xml_escape(
            &playitd::macos_launch_agent_log_dir()
                .join("tray.log")
                .to_string_lossy()
        ),
    )
}
