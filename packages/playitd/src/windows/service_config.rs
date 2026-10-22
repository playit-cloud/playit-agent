//! The service configures itself every time it starts. Doing this from code running as
//! SYSTEM is idempotent and works no matter how playit was installed (MSI, winget,
//! upgrade, repair), unlike installer custom actions that fail silently.

use std::io;
use std::path::Path;
use std::ptr::null_mut;
use std::time::Duration;

use widestring::U16CString;
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceFailureActions,
    ServiceFailureResetPeriod,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    UNPROTECTED_DACL_SECURITY_INFORMATION,
};
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows_sys::Win32::System::Services::SetServiceObjectSecurity;

use crate::manager::INSTALLED_SERVICE_LABEL;
use crate::{windows_service_data_dir, windows_service_log_dir};

/// Default service permissions plus start/stop/query for every signed-in user, so the tray
/// and CLI can start and stop the service without elevation. Users never get
/// SERVICE_CHANGE_CONFIG, which would allow running arbitrary code as SYSTEM.
const SERVICE_SDDL: &str = "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWRPWPLOCRRC;;;AU)(A;;CCLCSWLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)";

/// %ProgramData%\playit_gg holds the agent secret. Only SYSTEM and Administrators may read
/// files in it; users may list the folder so they can reach the logs.
/// Owned by Administrators so a user who created the folder first loses WRITE_DAC.
const DATA_DIR_SDDL: &str = "O:BAD:(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;;0x1200a9;;;AU)";

/// Logs inherit the data directory permissions and are additionally readable by users.
const LOG_DIR_SDDL: &str = "O:BAD:(A;OICI;0x1200a9;;;AU)";

pub(super) fn configure_installed_service() -> Result<(), String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|error| format!("failed to open the service manager: {error}"))?;
    let service = manager
        .open_service(
            INSTALLED_SERVICE_LABEL,
            ServiceAccess::CHANGE_CONFIG
                | ServiceAccess::START
                | ServiceAccess::READ_CONTROL
                | ServiceAccess::WRITE_DAC,
        )
        .map_err(|error| {
            format!("failed to open the {INSTALLED_SERVICE_LABEL} service: {error}")
        })?;

    let restart = |seconds| ServiceAction {
        action_type: ServiceActionType::Restart,
        delay: Duration::from_secs(seconds),
    };
    service
        .update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 60 * 60)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![restart(5), restart(15), restart(60)]),
        })
        .map_err(|error| format!("failed to set recovery actions: {error}"))?;
    service
        .set_failure_actions_on_non_crash_failures(true)
        .map_err(|error| format!("failed to enable recovery on error exits: {error}"))?;

    let descriptor = SecurityDescriptor::from_sddl(SERVICE_SDDL)?;
    let applied = unsafe {
        SetServiceObjectSecurity(
            service.raw_handle(),
            DACL_SECURITY_INFORMATION,
            descriptor.as_ptr(),
        )
    };
    if applied == 0 {
        return Err(format!(
            "failed to set service permissions: {}",
            io::Error::last_os_error()
        ));
    }

    Ok(())
}

pub(super) fn secure_data_directories() -> Result<(), String> {
    let data_dir = windows_service_data_dir();
    let log_dir = windows_service_log_dir();

    // Users can create folders in ProgramData. If one planted a junction here, SYSTEM
    // would rewrite permissions on whatever it points to, so refuse to touch it.
    for dir in [&data_dir, &log_dir] {
        if is_reparse_point(dir) {
            return Err(format!(
                "{} is a junction or symbolic link; delete it and restart the playit service",
                dir.display()
            ));
        }
    }
    std::fs::create_dir_all(&log_dir)
        .map_err(|error| format!("failed to create {}: {error}", log_dir.display()))?;

    set_directory_dacl(&data_dir, DATA_DIR_SDDL, true)?;
    set_directory_dacl(&log_dir, LOG_DIR_SDDL, false)?;

    // Written by the installer custom actions of 1.0.x releases; no longer used.
    let _ = std::fs::remove_file(data_dir.join("installed_user.sid"));
    Ok(())
}

fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;

    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(false)
}

fn set_directory_dacl(path: &Path, sddl: &str, protected: bool) -> Result<(), String> {
    let descriptor = SecurityDescriptor::from_sddl(sddl)?;
    let dacl = descriptor.dacl()?;
    let owner = descriptor.owner()?;
    let path_wide = U16CString::from_os_str(path.as_os_str())
        .map_err(|error| format!("invalid path {}: {error}", path.display()))?;
    let inheritance = if protected {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    };

    // SetNamedSecurityInfoW also propagates inheritable entries to existing children.
    let status = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | inheritance,
            owner,
            null_mut(),
            dacl,
            null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!(
            "failed to set permissions on {}: {}",
            path.display(),
            io::Error::from_raw_os_error(status as i32)
        ));
    }
    Ok(())
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn from_sddl(sddl: &str) -> Result<Self, String> {
        let sddl_wide = U16CString::from_str(sddl).map_err(|error| error.to_string())?;
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl_wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        };
        if converted == 0 {
            return Err(format!(
                "invalid security descriptor {sddl}: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(Self(descriptor))
    }

    fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }

    fn dacl(&self) -> Result<*mut ACL, String> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = null_mut();
        let ok =
            unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) };
        if ok == 0 || present == 0 {
            return Err(format!(
                "security descriptor has no DACL: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(dacl)
    }

    fn owner(&self) -> Result<PSID, String> {
        let mut owner: PSID = null_mut();
        let mut defaulted = 0;
        let ok = unsafe { GetSecurityDescriptorOwner(self.0, &mut owner, &mut defaulted) };
        if ok == 0 || owner.is_null() {
            return Err(format!(
                "security descriptor has no owner: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(owner)
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
