use std::ffi::OsString;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Output};

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, REG_VALUE_TYPE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::core::PCWSTR;

use super::normalize_sid;
use crate::migration::migrate_v17_secret_from_profile;
use crate::paths::{
    WINDOWS_SERVICE_NAME, windows_installed_user_sid_path, windows_service_log_path,
    windows_service_secret_path,
};

const CREATE_NO_WINDOW: u32 = 0x08000000;
const AUTHENTICATED_USERS_ICACLS_SID: &str = "*S-1-5-11";
const SERVICE_ACCESS_ACE: &str = "(A;;LCRPWPLO;;;AU)";
const PROFILE_LIST_REGISTRY_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList";
const PROFILE_IMAGE_PATH_VALUE: &str = "ProfileImagePath";

pub fn apply_installer_permissions(installed_user_sid: Option<&str>) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Err(error) = grant_log_folder_permissions() {
        errors.push(error);
    }
    if let Err(error) = grant_service_permissions() {
        errors.push(error);
    }
    if let Err(error) = write_installed_user_sid(installed_user_sid) {
        errors.push(error);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub fn migrate_v17_secret(installed_user_sid: Option<&str>) -> Result<(), String> {
    let sid = required_sid(installed_user_sid)?;
    let new_path = windows_service_secret_path();
    if new_path.exists() {
        return Ok(());
    }
    let Some(profile_dir) = profile_dir_for_sid(sid)? else {
        return Ok(());
    };
    migrate_v17_secret_from_profile(&new_path, &profile_dir).map(|_| ())
}

fn required_sid(value: Option<&str>) -> Result<&str, String> {
    let value = value
        .map(str::trim)
        .filter(|sid| !sid.is_empty())
        .ok_or_else(|| "MSI did not provide the installing user's SID".to_owned())?;
    normalize_sid(value)
        .ok_or_else(|| format!("MSI provided an invalid installing user SID: {value}"))
}

fn write_installed_user_sid(value: Option<&str>) -> Result<(), String> {
    let sid = required_sid(value)?;
    let path = windows_installed_user_sid_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create installed user SID directory at {}: {error}",
                parent.display()
            )
        })?;
    }
    fs::write(&path, format!("{sid}\n")).map_err(|error| {
        format!(
            "Failed to write installed user SID to {}: {error}",
            path.display()
        )
    })
}

fn grant_log_folder_permissions() -> Result<(), String> {
    let log_dir = windows_service_log_path()
        .parent()
        .ok_or_else(|| "Failed to resolve playit service log directory".to_owned())?
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
}

fn grant_service_permissions() -> Result<(), String> {
    let current = service_security_descriptor()?;
    let updated = add_service_access_ace(&current)?;
    if updated == current {
        return Ok(());
    }
    run_command(
        "sc.exe",
        vec![
            OsString::from("sdset"),
            OsString::from(WINDOWS_SERVICE_NAME),
            OsString::from(updated),
        ],
    )
}

fn service_security_descriptor() -> Result<String, String> {
    let output = run_command_with_output(
        "sc.exe",
        vec![
            OsString::from("sdshow"),
            OsString::from(WINDOWS_SERVICE_NAME),
        ],
    )?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("D:"))
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "Failed to find a DACL in sc.exe output: {}",
                output_text(&output)
            )
        })
}

fn add_service_access_ace(sddl: &str) -> Result<String, String> {
    if sddl.contains(SERVICE_ACCESS_ACE) {
        return Ok(sddl.to_owned());
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

fn profile_dir_for_sid(sid: &str) -> Result<Option<PathBuf>, String> {
    let subkey = wide_null(&format!("{PROFILE_LIST_REGISTRY_KEY}\\{sid}"));
    let value = wide_null(PROFILE_IMAGE_PATH_VALUE);
    let flags = RRF_RT_REG_EXPAND_SZ | RRF_RT_REG_SZ;
    let mut value_type = REG_VALUE_TYPE::default();
    let mut bytes = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            flags,
            Some(&mut value_type),
            None,
            Some(&mut bytes),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
        return Err(format!(
            "Failed to read Windows profile path size: {}",
            status.0
        ));
    }
    let mut buffer = vec![0u16; bytes.div_ceil(2) as usize];
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            flags,
            Some(&mut value_type),
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(format!("Failed to read Windows profile path: {}", status.0));
    }
    buffer.truncate(bytes.div_ceil(2) as usize);
    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    let value = String::from_utf16(&buffer)
        .map_err(|error| format!("Failed to decode Windows profile path: {error}"))?;
    expand_environment_strings(&value)
        .map(PathBuf::from)
        .map(Some)
}

fn expand_environment_strings(value: &str) -> Result<String, String> {
    let source = wide_null(value);
    let needed = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), None) };
    if needed == 0 {
        return Err(format!(
            "Failed to expand environment variables in profile path {value}: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut buffer = vec![0u16; needed as usize];
    let written = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), Some(&mut buffer)) };
    if written == 0 || written as usize > buffer.len() {
        return Err(format!("Failed to expand profile path {value}"));
    }
    buffer.truncate(written.saturating_sub(1) as usize);
    String::from_utf16(&buffer)
        .map_err(|error| format!("Failed to decode expanded profile path: {error}"))
}

fn run_command(program: &str, args: Vec<OsString>) -> Result<(), String> {
    let output = run_command_with_output(program, args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output_text(&output))
    }
}

fn run_command_with_output(program: &str, args: Vec<OsString>) -> Result<Output, String> {
    Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("Failed to run {program}: {error}"))
}

fn output_text(output: &Output) -> String {
    format!(
        "exit_status={} stdout=\"{}\" stderr=\"{}\"",
        output.status,
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_access_ace_is_idempotent_and_precedes_sacl() {
        let source = "D:(A;;LCRP;;;SY)S:(AU;FA;LCRP;;;WD)";
        let updated = add_service_access_ace(source).unwrap();
        assert_eq!(
            updated,
            "D:(A;;LCRP;;;SY)(A;;LCRPWPLO;;;AU)S:(AU;FA;LCRP;;;WD)"
        );
        assert_eq!(add_service_access_ace(&updated).unwrap(), updated);
    }
}
