mod installer;
mod shortcut;
mod sid;

pub use installer::{apply_installer_permissions, migrate_v17_secret};
pub use shortcut::{
    ensure_tray_startup_shortcut, remove_tray_startup_shortcut, tray_startup_shortcut_exists,
    tray_startup_shortcut_path,
};
pub use sid::{
    current_process_user_sid, normalize_sid, pipe_security_sddl, read_installed_user_sid,
    restricted_pipe_security_descriptor, write_current_user_sid,
};
