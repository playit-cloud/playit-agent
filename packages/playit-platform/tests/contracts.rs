use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use playit_platform::paths::*;

#[test]
fn installed_names_paths_and_file_formats_are_stable() {
    assert_eq!(LINUX_SERVICE_NAME, "playit");
    assert_eq!(WINDOWS_SERVICE_NAME, "playitd");
    assert_eq!(NATIVE_SERVICE_LABEL, "gg.playit.playitd");
    assert_eq!(LINUX_INSTALLED_SECRET_PATH, "/etc/playit/playit.toml");
    assert_eq!(LINUX_INSTALLED_SOCKET_PATH, "/run/playit/playitd.sock");
    assert_eq!(LINUX_INSTALLED_LOG_PATH, "/var/log/playit/playit.log");
    assert_eq!(WINDOWS_DATA_DIR_NAME, "playit_gg");
    assert_eq!(WINDOWS_TRAY_SHORTCUT_NAME, "Playit Tray.lnk");
}

#[test]
fn windows_mechanisms_are_cfg_guarded_and_shortcut_has_one_owner() {
    let service = include_str!("../src/service.rs");
    let shortcut = include_str!("../src/windows/shortcut.rs");
    let sid = include_str!("../src/windows/sid.rs");

    assert!(service.contains("OpenSCManagerW"));
    assert!(service.contains("QueryServiceStatusEx"));
    assert_eq!(shortcut.matches("CoCreateInstance").count(), 2);
    assert!(shortcut.contains("WINDOWS_TRAY_SHORTCUT_NAME"));
    assert!(sid.contains("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)"));
}

#[test]
fn atomic_secret_write_replaces_content_and_leaves_no_temp_file() {
    let temp = TempDir::new("secret-contract");
    let path = temp.path().join("playit.toml");
    playit_platform::secret::atomic_write_secret_blocking(&path, b"secret_key = \"first\"\n")
        .unwrap();
    playit_platform::secret::atomic_write_secret_blocking(&path, b"secret_key = \"second\"\n")
        .unwrap();

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "secret_key = \"second\"\n"
    );
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "playit-platform-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
