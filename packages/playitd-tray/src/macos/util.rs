use std::fs::File;
use std::os::fd::AsRawFd;

use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};

/// Goes to ~/Library/Logs/playit/tray.log when started by the login item.
pub(super) fn log(message: &str) {
    eprintln!("{message}");
}

pub(super) fn show_error(title: &str, message: &str) {
    log(&format!("{title}: {message}"));
    MessageDialog::new()
        .set_level(MessageLevel::Error)
        .set_title(title)
        .set_description(message)
        .set_buttons(MessageButtons::Ok)
        .show();
}

/// Asks before a destructive action. Cancel is the default.
pub(super) fn confirm_warning(title: &str, message: &str, confirm_label: &str) -> bool {
    let result = MessageDialog::new()
        .set_level(MessageLevel::Warning)
        .set_title(title)
        .set_description(message)
        .set_buttons(MessageButtons::OkCancelCustom(
            confirm_label.to_string(),
            "Cancel".to_string(),
        ))
        .show();
    matches!(result, MessageDialogResult::Custom(label) if label == confirm_label)
}

/// Holds an exclusive lock so the login item and a Finder launch don't both show an icon.
pub(super) struct SingleInstance {
    _file: File,
}

impl SingleInstance {
    pub(super) fn acquire() -> Result<Option<Self>, String> {
        let dir = playit_ipc::paths::playit_config_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("Could not create {}: {error}", dir.display()))?;
        let path = dir.join("tray.lock");
        let file = File::create(&path)
            .map_err(|error| format!("Could not open {}: {error}", path.display()))?;

        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(Self { _file: file }));
        }

        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(format!("Could not lock {}: {error}", path.display()))
        }
    }
}
