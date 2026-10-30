//! Per-user LaunchAgent management. The agent runs as the logged-in user, starts at login,
//! and keeps its secret, socket, and logs under that user's home directory, so installing it
//! needs no administrator password.

use std::{
    io,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use crate::manager::{INSTALLED_SERVICE_LABEL, InstalledServiceState, ServiceManagerError};

/// `launchctl` exit status when the job is not loaded in the domain.
const LAUNCHCTL_NO_SUCH_PROCESS: i32 = 113;

pub fn launch_agent_plist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| ".".into())
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{INSTALLED_SERVICE_LABEL}.plist"))
}

pub fn launch_agent_is_installed() -> bool {
    launch_agent_plist_path().exists()
}

/// The `playitd` binary shipped next to the running executable. The path is not canonicalized,
/// so a Homebrew symlink stays stable across upgrades.
pub fn bundled_playitd_path() -> Result<PathBuf, ServiceManagerError> {
    let exe = std::env::current_exe()?;
    let path = exe.with_file_name("playitd");
    if path.is_file() {
        return Ok(path);
    }

    Err(ServiceManagerError::NotAvailable(format!(
        "playitd was not found at {}. Keep playit and playitd in the same folder, or reinstall playit from https://playit.gg/download",
        path.display()
    )))
}

/// Writes the LaunchAgent plist if it is missing or out of date. Returns true when the plist
/// changed. A loaded job keeps its old arguments until it is reloaded.
pub fn write_launch_agent(playitd_path: &Path) -> Result<bool, ServiceManagerError> {
    let plist_path = launch_agent_plist_path();
    let contents = launch_agent_plist(playitd_path);

    if std::fs::read_to_string(&plist_path).ok().as_deref() == Some(contents.as_str()) {
        return Ok(false);
    }

    std::fs::create_dir_all(crate::macos_launch_agent_log_dir())?;
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&plist_path, contents)?;
    Ok(true)
}

/// Installs the LaunchAgent for `playitd_path` (or updates it) and starts it.
pub fn install_and_start_launch_agent(playitd_path: &Path) -> Result<(), ServiceManagerError> {
    let changed = write_launch_agent(playitd_path)?;
    if changed && launch_agent_is_loaded()? {
        bootout()?;
    }
    start_launch_agent()
}

pub fn start_launch_agent() -> Result<(), ServiceManagerError> {
    if !launch_agent_is_installed() {
        return Err(ServiceManagerError::NotFound);
    }

    if !launch_agent_is_loaded()? {
        let plist_path = launch_agent_plist_path();
        let plist = plist_path.to_string_lossy();
        run_launchctl(
            &["bootstrap", &gui_domain(), &plist],
            ServiceManagerError::StartFailed,
        )?;
    }

    // RunAtLoad starts a freshly bootstrapped job; kickstart covers a loaded job that exited.
    run_launchctl(
        &["kickstart", &service_target()],
        ServiceManagerError::StartFailed,
    )
}

/// Unloads the job. It loads again at the next login, or with `playit start`.
pub fn stop_launch_agent() -> Result<(), ServiceManagerError> {
    if !launch_agent_is_loaded()? {
        return Ok(());
    }
    bootout()
}

/// Unloads the job and deletes its plist. The secret and logs are left in place.
pub fn uninstall_launch_agent() -> Result<(), ServiceManagerError> {
    stop_launch_agent()?;
    match std::fs::remove_file(launch_agent_plist_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn launch_agent_state() -> Result<InstalledServiceState, ServiceManagerError> {
    if !launch_agent_is_installed() {
        return Ok(InstalledServiceState::NotInstalled);
    }

    let output = launchctl(&["print", &service_target()])?;
    if output.status.code() == Some(LAUNCHCTL_NO_SUCH_PROCESS) {
        return Ok(InstalledServiceState::Stopped);
    }
    if !output.status.success() {
        return Err(ServiceManagerError::NotAvailable(format!(
            "launchctl print failed: {}",
            command_detail(&output)
        )));
    }

    Ok(parse_print_state(&String::from_utf8_lossy(&output.stdout)))
}

fn launch_agent_is_loaded() -> Result<bool, ServiceManagerError> {
    let output = launchctl(&["print", &service_target()])?;
    Ok(output.status.success())
}

fn bootout() -> Result<(), ServiceManagerError> {
    let output = launchctl(&["bootout", &service_target()])?;
    if output.status.success() || output.status.code() == Some(LAUNCHCTL_NO_SUCH_PROCESS) {
        return Ok(());
    }
    Err(ServiceManagerError::StopFailed(format!(
        "launchctl bootout failed: {}",
        command_detail(&output)
    )))
}

fn parse_print_state(output: &str) -> InstalledServiceState {
    let state = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("state = "))
        .unwrap_or_default();

    match state {
        "running" => InstalledServiceState::Running,
        "spawn scheduled" | "spawning" | "xpcproxy" => InstalledServiceState::Starting,
        "exiting" => InstalledServiceState::Stopping,
        "not running" | "waiting" => InstalledServiceState::Stopped,
        _ => InstalledServiceState::Unknown,
    }
}

fn launch_agent_plist(playitd_path: &Path) -> String {
    let arguments = [
        playitd_path.to_path_buf(),
        "--secret-path".into(),
        crate::macos_launch_agent_secret_path(),
        "--socket-path".into(),
        crate::macos_launch_agent_socket_path(),
        "--log-path".into(),
        crate::macos_launch_agent_log_path(),
    ]
    .iter()
    .map(|argument| {
        format!(
            "        <string>{}</string>\n",
            xml_escape(&argument.to_string_lossy())
        )
    })
    .collect::<String>();

    // Exiting cleanly (`playit stop`) leaves it stopped; crashes are restarted.
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{arguments}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#,
        label = INSTALLED_SERVICE_LABEL,
        stderr = xml_escape(&crate::macos_launch_agent_stderr_path().to_string_lossy()),
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn gui_domain() -> String {
    format!("gui/{}", crate::unix_account::effective_uid())
}

fn service_target() -> String {
    format!("{}/{INSTALLED_SERVICE_LABEL}", gui_domain())
}

fn launchctl(args: &[&str]) -> Result<Output, ServiceManagerError> {
    Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|error| {
            ServiceManagerError::NotAvailable(format!("Failed to run launchctl: {error}"))
        })
}

fn run_launchctl(
    args: &[&str],
    err_builder: fn(String) -> ServiceManagerError,
) -> Result<(), ServiceManagerError> {
    let output = launchctl(args)?;
    if output.status.success() {
        return Ok(());
    }
    Err(err_builder(format!(
        "launchctl {} failed: {}",
        args.join(" "),
        command_detail(&output)
    )))
}

fn command_detail(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_paths_and_lists_daemon_arguments() {
        let plist = launch_agent_plist(Path::new("/Applications/A&B/playitd"));

        assert!(plist.contains("<string>/Applications/A&amp;B/playitd</string>"));
        assert!(plist.contains("<string>--secret-path</string>"));
        assert!(plist.contains("<string>--socket-path</string>"));
        assert!(plist.contains("<string>--log-path</string>"));
        assert!(plist.contains(&format!("<string>{INSTALLED_SERVICE_LABEL}</string>")));
    }

    #[test]
    fn parses_launchctl_print_states() {
        let running = "gui/501/gg.playit.playitd = {\n\tactive count = 1\n\tstate = running\n";
        assert_eq!(parse_print_state(running), InstalledServiceState::Running);
        assert_eq!(
            parse_print_state("\tstate = not running\n"),
            InstalledServiceState::Stopped
        );
        assert_eq!(parse_print_state(""), InstalledServiceState::Unknown);
    }
}
