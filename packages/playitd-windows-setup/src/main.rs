#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
mod startup_shortcut;

#[cfg(any(target_os = "windows", test))]
mod setup_log;

#[cfg(target_os = "windows")]
const COMMANDS: &[&str] = &[
    "ensure-startup-shortcut",
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

    if let Some(extra) = args.next() {
        return setup_log::log_command_result(
            &command_text,
            Err(format!(
                "Unexpected extra argument for playitd-windows-setup: {}",
                extra.to_string_lossy()
            )),
        );
    }

    let result = match command_text.as_str() {
        "ensure-startup-shortcut" => startup_shortcut::ensure_startup_shortcut(),
        "remove-startup-shortcut" => startup_shortcut::remove_startup_shortcut(),
        "write-installed-user-sid" => {
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
