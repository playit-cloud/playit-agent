use std::io::Write;
use std::path::{Path, PathBuf};

/// Keeps ACME account credentials and issued certificates on disk.
///
/// The directory is created readable only by its owner on Unix. On Windows the files
/// inherit the directory's ACL, which the service install restricts to SYSTEM and
/// Administrators.
#[derive(Debug, Clone)]
pub struct CertStore {
    dir: PathBuf,
}

/// A certificate chain and its private key, both PEM encoded.
#[derive(Debug, Clone)]
pub struct StoredCert {
    pub chain_pem: String,
    pub key_pem: String,
}

const ACCOUNT_FILE: &str = "acme-account.json";
const CHAIN_FILE: &str = "fullchain.pem";
const KEY_FILE: &str = "privkey.pem";

impl CertStore {
    pub fn new(dir: PathBuf) -> Self {
        CertStore { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub async fn load_account(&self) -> Option<String> {
        tokio::fs::read_to_string(self.dir.join(ACCOUNT_FILE))
            .await
            .ok()
    }

    pub async fn save_account(&self, content: &str) -> std::io::Result<()> {
        self.write(PathBuf::from(ACCOUNT_FILE), content.as_bytes().to_vec())
            .await
    }

    pub async fn load_cert(&self, domain: &str) -> Option<StoredCert> {
        let dir = self.domain_dir(domain)?;
        let chain_pem = tokio::fs::read_to_string(dir.join(CHAIN_FILE)).await.ok()?;
        let key_pem = tokio::fs::read_to_string(dir.join(KEY_FILE)).await.ok()?;
        Some(StoredCert { chain_pem, key_pem })
    }

    pub async fn save_cert(&self, domain: &str, cert: &StoredCert) -> std::io::Result<()> {
        let Some(dir) = self.domain_dir(domain) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{domain:?} is not a valid domain name"),
            ));
        };
        let relative = dir.strip_prefix(&self.dir).unwrap_or(&dir).to_path_buf();
        // The key is written first so a crash between the writes leaves an old chain with
        // a new key, which fails to load and gets reissued, never a usable mismatched pair.
        self.write(relative.join(KEY_FILE), cert.key_pem.as_bytes().to_vec())
            .await?;
        self.write(
            relative.join(CHAIN_FILE),
            cert.chain_pem.as_bytes().to_vec(),
        )
        .await
    }

    /// Certificates are stored per domain; names that could escape the directory are refused.
    fn domain_dir(&self, domain: &str) -> Option<PathBuf> {
        is_valid_domain(domain).then(|| self.dir.join(domain.to_ascii_lowercase()))
    }

    async fn write(&self, relative: PathBuf, content: Vec<u8>) -> std::io::Result<()> {
        let root = self.dir.clone();
        tokio::task::spawn_blocking(move || write_private_file(&root, &relative, &content))
            .await
            .map_err(std::io::Error::other)?
    }
}

pub fn is_valid_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.len() <= 253
        && domain.split('.').count() >= 2
        // An IP address is not a domain name.
        && !domain
            .rsplit('.')
            .next()
            .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        // An existing directory may have been created with looser permissions.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// Writes to a temporary file and renames it into place so a crash never leaves a
/// truncated key or certificate behind.
fn write_private_file(root: &Path, relative: &Path, content: &[u8]) -> std::io::Result<()> {
    let path = root.join(relative);
    create_private_dir(root)?;
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let tmp_path = path.with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));

    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp_path)?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp_path, &path)
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "playit-cert-store-{name}-{}-{}",
            std::process::id(),
            crate::utils::now_milli()
        ))
    }

    #[test]
    fn domain_validation_rejects_path_tricks() {
        assert!(is_valid_domain("abc.playit.plus"));
        assert!(is_valid_domain("my-site.example.com"));
        assert!(!is_valid_domain("../etc"));
        assert!(!is_valid_domain("a/b.com"));
        assert!(!is_valid_domain("localhost"));
        assert!(!is_valid_domain("*.example.com"));
        assert!(!is_valid_domain("-bad.example.com"));
        assert!(!is_valid_domain(""));
        assert!(!is_valid_domain("147.185.221.1"));
    }

    #[tokio::test]
    async fn saves_and_loads_certificates_privately() {
        let dir = temp_dir("roundtrip");
        let store = CertStore::new(dir.join("certs"));

        assert!(store.load_cert("site.example.com").await.is_none());
        store
            .save_cert(
                "Site.Example.com",
                &StoredCert {
                    chain_pem: "chain".into(),
                    key_pem: "key".into(),
                },
            )
            .await
            .unwrap();
        let loaded = store.load_cert("site.example.com").await.unwrap();
        assert_eq!(loaded.chain_pem, "chain");
        assert_eq!(loaded.key_pem, "key");

        store.save_account("{}").await.unwrap();
        assert_eq!(store.load_account().await.as_deref(), Some("{}"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(store.dir()), 0o700);
            assert_eq!(mode(&store.dir().join("site.example.com")), 0o700);
            assert_eq!(
                mode(&store.dir().join("site.example.com").join(KEY_FILE)),
                0o600
            );
            assert_eq!(mode(&store.dir().join(ACCOUNT_FILE)), 0o600);
        }

        assert!(
            store
                .save_cert(
                    "../escape",
                    &StoredCert {
                        chain_pem: String::new(),
                        key_pem: String::new()
                    }
                )
                .await
                .is_err()
        );

        let _ = std::fs::remove_dir_all(dir);
    }
}
