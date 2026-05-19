#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
mod permissions;
#[cfg(any(target_os = "windows", test))]
mod secret_migration;
#[cfg(any(target_os = "windows", test))]
mod sid;
#[cfg(target_os = "windows")]
mod startup_shortcut;

#[cfg(any(target_os = "windows", test))]
mod setup_log;

#[cfg(target_os = "windows")]
const COMMANDS: &[&str] = &[
    "apply-installer-permissions",
    "ensure-startup-shortcut",
    "migrate-v17-secret",
    "remove-startup-shortcut",
    "write-installed-user-sid",
];

#[cfg(target_os = "windows")]
fn main() {
    if let Err(error) = run_and_log() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "windows")]
fn run_and_log() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let Some(command) = args.next() else {
        return setup_log::log_command_result(
            "<missing>",
            Err(format!("Missing command.\n{}", usage())),
        );
    };
    let command_text = command.to_string_lossy().into_owned();
    let extra_args = args.collect::<Vec<_>>();

    let result = match command_text.as_str() {
        "apply-installer-permissions" => {
            if extra_args.len() > 1 {
                Err(unexpected_extra_arguments(&extra_args))
            } else {
                let installed_user_sid = extra_args.first().map(|arg| arg.to_string_lossy());
                permissions::apply_installer_permissions(installed_user_sid.as_deref())
            }
        }
        "ensure-startup-shortcut" => require_no_extra_arguments(&extra_args)
            .and_then(|()| startup_shortcut::ensure_startup_shortcut()),
        "migrate-v17-secret" => {
            if extra_args.len() > 1 {
                Err(unexpected_extra_arguments(&extra_args))
            } else {
                let installed_user_sid = extra_args.first().map(|arg| arg.to_string_lossy());
                secret_migration::migrate_v17_secret(installed_user_sid.as_deref())
            }
        }
        "remove-startup-shortcut" => require_no_extra_arguments(&extra_args)
            .and_then(|()| startup_shortcut::remove_startup_shortcut()),
        "write-installed-user-sid" => {
            require_no_extra_arguments(&extra_args)?;
            playitd::windows::write_current_user_sid()
                .map_err(|error| format!("Failed to write installed user SID: {error}"))?;
            Ok(())
        }
        other => Err(format!(
            "Unsupported playitd-windows-setup command: {other}"
        )),
    };

    setup_log::log_command_result(&command_text, result)
}

#[cfg(target_os = "windows")]
fn require_no_extra_arguments(args: &[std::ffi::OsString]) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(unexpected_extra_arguments(args))
    }
}

#[cfg(target_os = "windows")]
fn unexpected_extra_arguments(args: &[std::ffi::OsString]) -> String {
    format!(
        "Unexpected extra argument(s) for playitd-windows-setup: {}",
        args.iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    )
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("playitd-windows-setup is only supported on Windows");
    std::process::exit(1);
}

#[cfg(target_os = "windows")]
fn usage() -> String {
    format!(
        "Usage: playitd-windows-setup <command>\nCommands:\n  {}",
        COMMANDS.join("\n  ")
    )
}
