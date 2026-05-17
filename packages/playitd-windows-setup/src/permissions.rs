use std::ffi::OsString;
use std::fs;
use std::os::windows::process::CommandExt;
use std::process::{Command, Output};

use playitd::manager::INSTALLED_SERVICE_LABEL;

const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(test)]
const AUTHENTICATED_USERS_SDDL_ALIAS: &str = "AU";
const AUTHENTICATED_USERS_ICACLS_SID: &str = "*S-1-5-11";
const SERVICE_ACCESS_ACE: &str = "(A;;LCRPWPLO;;;AU)";

pub(crate) fn apply_installer_permissions() -> Result<(), String> {
    let mut errors = Vec::new();

    if let Err(error) = grant_log_folder_permissions() {
        errors.push(error);
    }

    if let Err(error) = grant_service_permissions() {
        errors.push(error);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn grant_log_folder_permissions() -> Result<(), String> {
    let log_dir = playitd::windows_service_log_path()
        .parent()
        .ok_or_else(|| "Failed to resolve playit service log directory".to_string())?
        .to_path_buf();

    fs::create_dir_all(&log_dir).map_err(|error| {
        format!(
            "Failed to create playit service log directory at {}: {error}",
            log_dir.display()
        )
    })?;

    run_command(
        "icacls.exe",
        vec![
            log_dir.as_os_str().to_os_string(),
            OsString::from("/grant"),
            OsString::from(format!("{AUTHENTICATED_USERS_ICACLS_SID}:(OI)(CI)M")),
        ],
    )
    .map_err(|error| {
        format!(
            "Failed to grant Authenticated Users modify access to {}: {error}",
            log_dir.display()
        )
    })
}

fn grant_service_permissions() -> Result<(), String> {
    let current_sddl = service_security_descriptor()?;
    let updated_sddl = add_service_access_ace(&current_sddl)?;

    if updated_sddl == current_sddl {
        return Ok(());
    }

    run_command(
        "sc.exe",
        vec![
            OsString::from("sdset"),
            OsString::from(INSTALLED_SERVICE_LABEL),
            OsString::from(updated_sddl),
        ],
    )
    .map_err(|error| {
        format!(
            "Failed to grant Authenticated Users access to the {INSTALLED_SERVICE_LABEL} service: {error}"
        )
    })
}

fn service_security_descriptor() -> Result<String, String> {
    let output = run_command_with_output(
        "sc.exe",
        vec![
            OsString::from("sdshow"),
            OsString::from(INSTALLED_SERVICE_LABEL),
        ],
    )
    .map_err(|error| {
        format!("Failed to read the {INSTALLED_SERVICE_LABEL} service security descriptor: {error}")
    })?;

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("D:"))
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "Failed to find a DACL in sc.exe sdshow output: {}",
                command_output_text(&output)
            )
        })
}

fn add_service_access_ace(sddl: &str) -> Result<String, String> {
    if sddl.contains(SERVICE_ACCESS_ACE) {
        return Ok(sddl.to_string());
    }

    if !sddl.starts_with("D:") {
        return Err(format!("Service security descriptor has no DACL: {sddl}"));
    }

    let insert_at = sddl.find("S:").unwrap_or(sddl.len());
    let mut updated = String::with_capacity(sddl.len() + SERVICE_ACCESS_ACE.len());
    updated.push_str(&sddl[..insert_at]);
    updated.push_str(SERVICE_ACCESS_ACE);
    updated.push_str(&sddl[insert_at..]);
    Ok(updated)
}

fn run_command(program: &str, args: Vec<OsString>) -> Result<(), String> {
    let output = run_command_with_output(program, args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_output_text(&output))
    }
}

fn run_command_with_output(program: &str, args: Vec<OsString>) -> Result<Output, String> {
    Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("Failed to run {program}: {error}"))
}

fn command_output_text(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!(
        "exit_status={} stdout=\"{}\" stderr=\"{}\"",
        output.status,
        stdout.trim(),
        stderr.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::{add_service_access_ace, AUTHENTICATED_USERS_SDDL_ALIAS, SERVICE_ACCESS_ACE};

    #[test]
    fn service_ace_uses_authenticated_users_sddl_alias() {
        assert!(SERVICE_ACCESS_ACE.ends_with(&format!(";;;{})", AUTHENTICATED_USERS_SDDL_ALIAS)));
    }

    #[test]
    fn add_service_access_ace_appends_to_dacl_before_sacl() {
        let sddl = "D:(A;;LCRP;;;SY)S:(AU;FA;LCRP;;;WD)";

        let updated = add_service_access_ace(sddl).unwrap();

        assert_eq!(
            updated,
            "D:(A;;LCRP;;;SY)(A;;LCRPWPLO;;;AU)S:(AU;FA;LCRP;;;WD)"
        );
    }

    #[test]
    fn add_service_access_ace_is_idempotent() {
        let sddl = "D:(A;;LCRP;;;SY)(A;;LCRPWPLO;;;AU)";

        assert_eq!(add_service_access_ace(sddl).unwrap(), sddl);
    }

    #[test]
    fn add_service_access_ace_rejects_missing_dacl() {
        assert!(add_service_access_ace("S:(AU;FA;LCRP;;;WD)").is_err());
    }
}
