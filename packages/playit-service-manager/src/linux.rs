use std::process::Command;

use crate::ServiceManagerError;

const SYSTEMD_SERVICE_NAME: &str = "playit";
const OPENRC_SERVICE_NAME: &str = "playit";

pub(crate) fn start_systemd_service() -> Result<(), ServiceManagerError> {
    run_systemctl(&systemd_start_args(), ServiceManagerError::StartFailed)
}

pub(crate) fn start_openrc_service() -> Result<(), ServiceManagerError> {
    run_rc_service(&openrc_start_args(), ServiceManagerError::StartFailed)
}

pub(crate) fn is_systemd_service_active() -> Result<bool, ServiceManagerError> {
    let args = systemd_is_active_args();
    let output = Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|error| {
            ServiceManagerError::NotAvailable(format!("Failed to run systemctl: {error}"))
        })?;

    match output.status.code() {
        Some(0) => Ok(true),
        Some(3) | Some(4) => Ok(false),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                format!("exit status {}", output.status)
            };

            Err(ServiceManagerError::NotAvailable(format!(
                "systemctl {} failed: {detail}",
                args.join(" ")
            )))
        }
    }
}

pub(crate) fn is_openrc_service_active() -> Result<bool, ServiceManagerError> {
    let output = Command::new("rc-service")
        .args(openrc_status_args())
        .output()
        .map_err(|error| {
            ServiceManagerError::NotAvailable(format!("Failed to run rc-service: {error}"))
        })?;

    Ok(output.status.success())
}

pub(crate) fn stop_systemd_service() -> Result<(), ServiceManagerError> {
    run_systemctl(&systemd_stop_args(), ServiceManagerError::StopFailed)
}

pub(crate) fn stop_openrc_service() -> Result<(), ServiceManagerError> {
    run_rc_service(&openrc_stop_args(), ServiceManagerError::StopFailed)
}

fn systemd_start_args() -> [&'static str; 2] {
    ["start", SYSTEMD_SERVICE_NAME]
}

fn systemd_is_active_args() -> [&'static str; 3] {
    ["is-active", "--quiet", SYSTEMD_SERVICE_NAME]
}

fn systemd_stop_args() -> [&'static str; 2] {
    ["stop", SYSTEMD_SERVICE_NAME]
}

fn openrc_start_args() -> [&'static str; 2] {
    [OPENRC_SERVICE_NAME, "start"]
}

fn openrc_status_args() -> [&'static str; 2] {
    [OPENRC_SERVICE_NAME, "status"]
}

fn openrc_stop_args() -> [&'static str; 2] {
    [OPENRC_SERVICE_NAME, "stop"]
}

fn run_systemctl(
    args: &[&str],
    error: fn(String) -> ServiceManagerError,
) -> Result<(), ServiceManagerError> {
    run_command("systemctl", args, error)
}

fn run_rc_service(
    args: &[&str],
    error: fn(String) -> ServiceManagerError,
) -> Result<(), ServiceManagerError> {
    run_command("rc-service", args, error)
}

fn run_command(
    command: &str,
    args: &[&str],
    error: fn(String) -> ServiceManagerError,
) -> Result<(), ServiceManagerError> {
    let output = Command::new(command)
        .args(args)
        .output()
        .map_err(|cause| error(format!("Failed to run {command}: {cause}")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    };

    Err(error(format!(
        "{command} {} failed: {detail}",
        args.join(" ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_command_args_are_stable() {
        assert_eq!(systemd_start_args(), ["start", "playit"]);
        assert_eq!(systemd_stop_args(), ["stop", "playit"]);
    }

    #[test]
    fn openrc_command_args_are_stable() {
        assert_eq!(openrc_start_args(), ["playit", "start"]);
        assert_eq!(openrc_status_args(), ["playit", "status"]);
        assert_eq!(openrc_stop_args(), ["playit", "stop"]);
    }
}
