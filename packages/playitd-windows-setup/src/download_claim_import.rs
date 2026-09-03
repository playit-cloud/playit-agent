use std::path::Path;

use playitd::download_claim::{DownloadClaimFile, token_from_file_name};

/// Copies the download claim token out of the installer's file name so playitd
/// can exchange it for an agent secret on first start. Installers downloaded
/// without a token are the normal case and are not an error.
#[cfg(target_os = "windows")]
pub(crate) fn import_download_claim(installer_path: &str) -> Result<(), String> {
    let installer_path = installer_path.trim();
    if installer_path.is_empty() {
        return Err("MSI did not provide its own path".to_string());
    }

    import_download_claim_to(
        Path::new(installer_path),
        &playitd::windows_download_claim_path(),
    )
}

fn import_download_claim_to(installer_path: &Path, claim_path: &Path) -> Result<(), String> {
    let Some(file_name) = installer_path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let Some(token) = token_from_file_name(file_name) else {
        return Ok(());
    };

    DownloadClaimFile::write(claim_path, &token)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use playitd::download_claim::DownloadClaimFile;

    use super::import_download_claim_to;

    #[test]
    fn installer_name_with_token_writes_claim_file() {
        let token = [0x3c; 32];
        let dir = temp_dir("with-token");
        let claim_path = dir.join("download_claim.token");
        let installer = PathBuf::from(format!(
            r"C:\Users\Alice\Downloads\playit-windows-x86_64-signed-{} (1).msi",
            hex::encode(token)
        ));

        import_download_claim_to(&installer, &claim_path).unwrap();

        assert_eq!(DownloadClaimFile::read(&claim_path).unwrap(), Some(token));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn installer_name_without_token_writes_nothing() {
        let dir = temp_dir("without-token");
        let claim_path = dir.join("download_claim.token");

        import_download_claim_to(
            Path::new(r"C:\Users\Alice\Downloads\playit-windows-x86_64-signed.msi"),
            &claim_path,
        )
        .unwrap();

        assert!(!claim_path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "playitd-windows-setup-download-claim-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
