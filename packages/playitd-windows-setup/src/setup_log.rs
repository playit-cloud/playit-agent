#[cfg(target_os = "windows")]
use std::env;
#[cfg(target_os = "windows")]
use std::fs::{self, OpenOptions};
#[cfg(target_os = "windows")]
use std::io::{self, Write};
#[cfg(target_os = "windows")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(target_os = "windows")]
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_RELATIVE_PATH: &[&str] = &["playit_gg", "logs", "playit-installer.log"];
#[cfg(target_os = "windows")]
const PROGRAMDATA_FALLBACK: &str = r"C:\ProgramData";

#[cfg(target_os = "windows")]
pub(crate) fn log_command_result(command: &str, result: Result<(), String>) -> Result<(), String> {
    let status = if result.is_ok() { "success" } else { "failure" };
    let detail = match result.as_ref() {
        Ok(()) => "completed".to_string(),
        Err(error) => error.clone(),
    };

    let line = format_log_line(
        system_time_millis(SystemTime::now()),
        command,
        status,
        &detail,
    );
    let _ = append_log_line(&default_log_path(), &line);

    result
}

#[cfg(target_os = "windows")]
fn default_log_path() -> PathBuf {
    let base = env::var_os("PROGRAMDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(PROGRAMDATA_FALLBACK));
    log_path_from_base(base)
}

fn log_path_from_base(base: impl Into<PathBuf>) -> PathBuf {
    let mut path = base.into();
    for segment in LOG_RELATIVE_PATH {
        path.push(segment);
    }
    path
}

#[cfg(target_os = "windows")]
fn append_log_line(path: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

fn format_log_line(timestamp_millis: u128, command: &str, status: &str, detail: &str) -> String {
    format!(
        "timestamp_ms={timestamp_millis} command=\"{}\" status={status} detail=\"{}\"",
        escape_log_value(command),
        escape_log_value(detail)
    )
}

fn escape_log_value(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect(),
            '\r' => "\\r".chars().collect(),
            '\n' => "\\n".chars().collect(),
            '\t' => "\\t".chars().collect(),
            ch => vec![ch],
        })
        .collect()
}

#[cfg(target_os = "windows")]
fn system_time_millis(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{format_log_line, log_path_from_base};
    use std::path::PathBuf;

    #[test]
    fn log_path_uses_program_data_base() {
        let path = log_path_from_base(PathBuf::from(r"D:\ProgramData"));

        assert_eq!(
            path,
            PathBuf::from(r"D:\ProgramData")
                .join("playit_gg")
                .join("logs")
                .join("playit-installer.log")
        );
    }

    #[test]
    fn log_line_escapes_values() {
        let line = format_log_line(
            123,
            "ensure-startup-shortcut",
            "failure",
            "failed \"badly\"\nnext line",
        );

        assert_eq!(
            line,
            "timestamp_ms=123 command=\"ensure-startup-shortcut\" status=failure detail=\"failed \\\"badly\\\"\\nnext line\""
        );
    }
}
