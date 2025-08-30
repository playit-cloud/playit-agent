use std::{env::current_exe, path::PathBuf};

pub fn install_service() {
    let program_path =
        current_exe().expect("failed to determine path of currently running program");
}
