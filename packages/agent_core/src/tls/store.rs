use std::io::Write;
use std::path::{Path, PathBuf};

/// Keeps ACME account credentials and issued certificates on disk.
///
/// Directories the store creates, and the files it writes, are accessible only to the
/// current user: mode 0700/0600 on Unix, and on Windows a protected ACL that also admits
/// SYSTEM and Administrators. An existing root directory keeps its permissions.
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

/// Creates the store's root directory if it is missing. An existing root is left as it is,
/// since `--cert-dir` may point at a shared folder whose permissions are the user's choice.
fn ensure_root_dir(root: &Path) -> std::io::Result<()> {
    if root.is_dir() {
        return Ok(());
    }
    if let Some(parent) = root.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    create_private_dir(root, false)
}

/// Creates a directory that only the current user (and on Windows, SYSTEM and
/// Administrators) can access. With `restrict_existing`, a directory that already exists is
/// locked down too; that is only done for directories the store owns entirely.
fn create_private_dir(path: &Path, restrict_existing: bool) -> std::io::Result<()> {
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    match builder.create(path) {
        Ok(()) => restrict_dir(path),
        // `symlink_metadata` so a symlink planted in a shared `--cert-dir` isn't followed.
        Err(error)
            if error.kind() == std::io::ErrorKind::AlreadyExists
                && std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir()) =>
        {
            if restrict_existing {
                restrict_dir(path)
            } else {
                Ok(())
            }
        }
        Err(error) => Err(error),
    }
}

fn restrict_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(windows)]
    {
        windows_acl::restrict(path, true)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

fn restrict_file(file: &std::fs::File, path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = path;
        // The mode given at creation doesn't apply to a leftover temporary file.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(windows)]
    {
        let _ = file;
        windows_acl::restrict(path, false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, path);
        Ok(())
    }
}

/// Writes to a temporary file and renames it into place so a crash never leaves a
/// truncated key or certificate behind.
fn write_private_file(root: &Path, relative: &Path, content: &[u8]) -> std::io::Result<()> {
    let path = root.join(relative);
    ensure_root_dir(root)?;
    if let Some(parent) = path.parent()
        && parent != root
    {
        create_private_dir(parent, true)?;
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    // A random name opened with `create_new` (O_EXCL) never reuses or follows something
    // another user placed in a shared directory; `rename` then replaces a planted symlink
    // rather than writing through it.
    let tmp_path = path.with_file_name(format!(
        ".{file_name}.tmp-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));

    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp_path)?;
        // Restricted before the content is written so the key is never readable by others.
        restrict_file(&file, &tmp_path)?;
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

/// Gives a file or directory a protected DACL (no inherited entries) that grants full
/// access to the current user, SYSTEM and Administrators only.
#[cfg(windows)]
mod windows_acl {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    pub fn restrict(path: &Path, is_dir: bool) -> std::io::Result<()> {
        let user = current_user_sid()?;
        let sddl = dacl_sddl(&user, is_dir);
        let sddl: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();

        unsafe {
            let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }

            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let result =
                if GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
                    == 0
                {
                    Err(std::io::Error::last_os_error())
                } else {
                    let status = SetNamedSecurityInfoW(
                        wide_path.as_ptr(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        dacl,
                        std::ptr::null(),
                    );
                    if status == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::from_raw_os_error(status as i32))
                    }
                };
            LocalFree(descriptor);
            result
        }
    }

    /// SY is SYSTEM and BA the Administrators group. Directory entries are inherited by
    /// the files created inside.
    pub(super) fn dacl_sddl(user_sid: &str, is_dir: bool) -> String {
        let flags = if is_dir { "OICI" } else { "" };
        format!("D:P(A;{flags};FA;;;SY)(A;{flags};FA;;;BA)(A;{flags};FA;;;{user_sid})")
    }

    fn current_user_sid() -> std::io::Result<String> {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(std::io::Error::last_os_error());
            }

            let result = (|| {
                let mut size = 0u32;
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
                if size == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // u64 storage keeps the buffer aligned for TOKEN_USER.
                let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
                if GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    size,
                    &mut size,
                ) == 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                let user = &*(buffer.as_ptr() as *const TOKEN_USER);

                let mut wide: *mut u16 = std::ptr::null_mut();
                if ConvertSidToStringSidW(user.User.Sid, &mut wide) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let len = (0..).take_while(|&i| *wide.add(i) != 0).count();
                let sid = String::from_utf16_lossy(std::slice::from_raw_parts(wide, len));
                LocalFree(wide.cast());
                Ok(sid)
            })();

            CloseHandle(token);
            result
        }
    }
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

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_domain_directory_is_refused() {
        let dir = temp_dir("symlink");
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let store = CertStore::new(dir.join("certs"));
        store.save_account("{}").await.unwrap();
        std::os::unix::fs::symlink(&elsewhere, store.dir().join("site.example.com")).unwrap();

        let cert = StoredCert {
            chain_pem: "chain".into(),
            key_pem: "key".into(),
        };
        assert!(store.save_cert("site.example.com", &cert).await.is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);

        let _ = std::fs::remove_dir_all(dir);
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

    #[cfg(unix)]
    #[tokio::test]
    async fn existing_root_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        let dir = temp_dir("shared-root");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A per-domain directory left with loose permissions is the store's own and is tightened.
        let domain_dir = dir.join("site.example.com");
        std::fs::create_dir(&domain_dir).unwrap();
        std::fs::set_permissions(&domain_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let store = CertStore::new(dir.clone());
        store.save_account("{}").await.unwrap();
        store
            .save_cert(
                "site.example.com",
                &StoredCert {
                    chain_pem: "chain".into(),
                    key_pem: "key".into(),
                },
            )
            .await
            .unwrap();

        assert_eq!(mode(&dir), 0o755);
        assert_eq!(mode(&dir.join(ACCOUNT_FILE)), 0o600);
        assert_eq!(mode(&domain_dir), 0o700);
        assert_eq!(mode(&domain_dir.join(KEY_FILE)), 0o600);
        assert_eq!(mode(&domain_dir.join(CHAIN_FILE)), 0o600);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_parents_of_the_root_are_not_restricted() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        let dir = temp_dir("nested");
        let store = CertStore::new(dir.join("parent").join("certs"));
        store.save_account("{}").await.unwrap();

        assert_eq!(mode(&dir.join("parent").join("certs")), 0o700);
        assert_ne!(mode(&dir.join("parent")), 0o700);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(windows)]
    #[test]
    fn windows_acl_grants_only_user_system_and_admins() {
        assert_eq!(
            windows_acl::dacl_sddl("S-1-5-21-1", true),
            "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;S-1-5-21-1)"
        );
        assert_eq!(
            windows_acl::dacl_sddl("S-1-5-21-1", false),
            "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;S-1-5-21-1)"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn writes_files_with_a_restricted_acl() {
        let dir = temp_dir("acl");
        let store = CertStore::new(dir.join("certs"));
        store.save_account("{}").await.unwrap();
        assert_eq!(store.load_account().await.as_deref(), Some("{}"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
