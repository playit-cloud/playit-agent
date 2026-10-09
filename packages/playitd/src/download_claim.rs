use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[cfg(target_os = "windows")]
use std::fs::File;

const DOS_HEADER_SIZE: usize = 64;
const PE_SIGNATURE_SIZE: u64 = 4;
const COFF_HEADER_SIZE: u64 = 20;
const PE32_MAGIC: u16 = 0x10b;
const PE32_PLUS_MAGIC: u16 = 0x20b;
const PE32_DATA_DIRECTORIES_OFFSET: u64 = 96;
const PE32_PLUS_DATA_DIRECTORIES_OFFSET: u64 = 112;
const SECURITY_DIRECTORY_INDEX: u64 = 4;
const DATA_DIRECTORY_SIZE: u64 = 8;
const CERTIFICATE_HEADER_SIZE: u32 = 8;
const CERTIFICATE_REVISION_2: u16 = 0x0200;
const PLAYIT_CERTIFICATE_TYPE: u16 = 0x0f01;
const PLAYIT_CERTIFICATE_MAGIC: &[u8; 16] = b"PLAYIT_CLAIM_V1\0";
const DOWNLOAD_CLAIM_TOKEN_SIZE: usize = 32;
const PLAYIT_CERTIFICATE_SIZE: u32 = CERTIFICATE_HEADER_SIZE
    + PLAYIT_CERTIFICATE_MAGIC.len() as u32
    + DOWNLOAD_CLAIM_TOKEN_SIZE as u32;
const MAX_CERTIFICATE_TABLE_SIZE: u64 = 16 * 1024 * 1024;
const MAX_CERTIFICATE_COUNT: usize = 1024;

pub type DownloadClaimToken = [u8; DOWNLOAD_CLAIM_TOKEN_SIZE];

/// Extracts the download claim token the website put in an installer's file name.
///
/// The token is the only run of 64 hex digits in the name. Browsers may rename
/// downloads (`playit-setup-<token> (1).msi`), so the rest of the name is ignored.
pub fn token_from_file_name(file_name: &str) -> Option<DownloadClaimToken> {
    let bytes = file_name.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_hexdigit() {
            start += 1;
            continue;
        }

        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
            end += 1;
        }

        if end - start == DOWNLOAD_CLAIM_TOKEN_SIZE * 2 {
            let mut token = [0u8; DOWNLOAD_CLAIM_TOKEN_SIZE];
            if hex::decode_to_slice(&file_name[start..end], &mut token).is_ok() {
                return Some(token);
            }
        }

        start = end;
    }

    None
}

/// Token file written by the installer and consumed by playitd on first start.
pub struct DownloadClaimFile;

impl DownloadClaimFile {
    pub fn write(path: &Path, token: &DownloadClaimToken) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Failed to create {}: {error}", parent.display()))?;
        }
        std::fs::write(path, hex::encode(token))
            .map_err(|error| format!("Failed to write {}: {error}", path.display()))
    }

    pub fn read(path: &Path) -> Result<Option<DownloadClaimToken>, String> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("Failed to read {}: {error}", path.display())),
        };

        let mut token = [0u8; DOWNLOAD_CLAIM_TOKEN_SIZE];
        hex::decode_to_slice(content.trim(), &mut token)
            .map_err(|error| format!("{} holds an invalid token: {error}", path.display()))?;
        Ok(Some(token))
    }

    pub fn remove(path: &Path) -> Result<(), String> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("Failed to remove {}: {error}", path.display())),
        }
    }
}

pub struct EmbeddedDownloadClaim {
    token: DownloadClaimToken,
}

impl EmbeddedDownloadClaim {
    #[cfg(target_os = "windows")]
    pub fn read_from_current_executable() -> Result<Option<Self>, String> {
        let path = std::env::current_exe()
            .map_err(|error| format!("Failed to locate the running executable: {error}"))?;
        let mut file = File::open(&path)
            .map_err(|error| format!("Failed to open {}: {error}", path.display()))?;
        let file_len = file
            .metadata()
            .map_err(|error| format!("Failed to inspect {}: {error}", path.display()))?
            .len();

        Self::read_from(&mut file, file_len)
            .map_err(|error| format!("Failed to read {}: {error}", path.display()))
    }

    pub fn encoded_token(&self) -> String {
        hex::encode(self.token)
    }

    fn read_from(file: &mut (impl Read + Seek), file_len: u64) -> Result<Option<Self>, String> {
        if file_len < DOS_HEADER_SIZE as u64 {
            return Ok(None);
        }

        let mut dos_header = [0u8; DOS_HEADER_SIZE];
        read_at(file, 0, &mut dos_header)?;
        if &dos_header[..2] != b"MZ" {
            return Ok(None);
        }

        let pe_offset = u32::from_le_bytes(
            dos_header[0x3c..0x40]
                .try_into()
                .expect("DOS header offset is four bytes"),
        ) as u64;
        let optional_header_offset = pe_offset
            .checked_add(PE_SIGNATURE_SIZE + COFF_HEADER_SIZE)
            .ok_or_else(|| "PE header offset overflowed".to_string())?;

        let mut pe_header = [0u8; 24];
        read_at(file, pe_offset, &mut pe_header)?;
        if &pe_header[..4] != b"PE\0\0" {
            return Ok(None);
        }

        let optional_header_size = u16::from_le_bytes(
            pe_header[20..22]
                .try_into()
                .expect("optional header size is two bytes"),
        ) as u64;
        if file_len < optional_header_offset
            || file_len - optional_header_offset < optional_header_size
        {
            return Err("optional PE header is truncated".to_string());
        }
        let mut optional_magic = [0u8; 2];
        read_at(file, optional_header_offset, &mut optional_magic)?;
        let data_directories_offset = match u16::from_le_bytes(optional_magic) {
            PE32_MAGIC => PE32_DATA_DIRECTORIES_OFFSET,
            PE32_PLUS_MAGIC => PE32_PLUS_DATA_DIRECTORIES_OFFSET,
            _ => return Ok(None),
        };
        let security_directory_offset = data_directories_offset
            .checked_add(SECURITY_DIRECTORY_INDEX * DATA_DIRECTORY_SIZE)
            .ok_or_else(|| "security directory offset overflowed".to_string())?;
        let security_directory_end = security_directory_offset
            .checked_add(DATA_DIRECTORY_SIZE)
            .ok_or_else(|| "security directory size overflowed".to_string())?;
        if optional_header_size < security_directory_end {
            return Ok(None);
        }

        let mut data_directory_count = [0u8; 4];
        read_at(
            file,
            optional_header_offset + data_directories_offset - 4,
            &mut data_directory_count,
        )?;
        if u32::from_le_bytes(data_directory_count) <= SECURITY_DIRECTORY_INDEX as u32 {
            return Ok(None);
        }

        let mut security_directory = [0u8; DATA_DIRECTORY_SIZE as usize];
        read_at(
            file,
            optional_header_offset
                .checked_add(security_directory_offset)
                .ok_or_else(|| "security directory file offset overflowed".to_string())?,
            &mut security_directory,
        )?;
        let certificate_table_offset = u32::from_le_bytes(
            security_directory[..4]
                .try_into()
                .expect("certificate offset is four bytes"),
        ) as u64;
        let certificate_table_size = u32::from_le_bytes(
            security_directory[4..]
                .try_into()
                .expect("certificate size is four bytes"),
        ) as u64;

        if certificate_table_size == 0 {
            return Ok(None);
        }
        if certificate_table_size > MAX_CERTIFICATE_TABLE_SIZE {
            return Err("certificate table exceeds the supported size".to_string());
        }
        if file_len < certificate_table_offset
            || file_len - certificate_table_offset < certificate_table_size
        {
            return Err("certificate table extends past the executable".to_string());
        }

        let certificate_table_end = certificate_table_offset
            .checked_add(certificate_table_size)
            .ok_or_else(|| "certificate table offset overflowed".to_string())?;
        let mut certificate_offset = certificate_table_offset;
        let mut certificate_count = 0usize;

        while certificate_offset < certificate_table_end {
            if certificate_count >= MAX_CERTIFICATE_COUNT {
                return Err("certificate table has too many entries".to_string());
            }
            certificate_count += 1;

            if certificate_table_end - certificate_offset < CERTIFICATE_HEADER_SIZE as u64 {
                return Err("certificate header is truncated".to_string());
            }

            let mut header = [0u8; CERTIFICATE_HEADER_SIZE as usize];
            read_at(file, certificate_offset, &mut header)?;
            let certificate_size = u32::from_le_bytes(
                header[..4]
                    .try_into()
                    .expect("certificate size is four bytes"),
            );
            let revision = u16::from_le_bytes(
                header[4..6]
                    .try_into()
                    .expect("certificate revision is two bytes"),
            );
            let certificate_type = u16::from_le_bytes(
                header[6..8]
                    .try_into()
                    .expect("certificate type is two bytes"),
            );

            if certificate_size < CERTIFICATE_HEADER_SIZE {
                return Err("certificate entry is smaller than its header".to_string());
            }
            if certificate_table_end - certificate_offset < certificate_size as u64 {
                return Err("certificate entry is truncated".to_string());
            }

            if certificate_size == PLAYIT_CERTIFICATE_SIZE
                && revision == CERTIFICATE_REVISION_2
                && certificate_type == PLAYIT_CERTIFICATE_TYPE
            {
                let mut payload = [0u8; PLAYIT_CERTIFICATE_MAGIC.len() + DOWNLOAD_CLAIM_TOKEN_SIZE];
                read_at(
                    file,
                    certificate_offset + CERTIFICATE_HEADER_SIZE as u64,
                    &mut payload,
                )?;
                if &payload[..PLAYIT_CERTIFICATE_MAGIC.len()] == PLAYIT_CERTIFICATE_MAGIC {
                    let token = payload[PLAYIT_CERTIFICATE_MAGIC.len()..]
                        .try_into()
                        .expect("download claim token is 32 bytes");
                    return Ok(Some(Self { token }));
                }
            }

            let aligned_size = certificate_size
                .checked_add(7)
                .map(|size| size & !7)
                .ok_or_else(|| "aligned certificate size overflowed".to_string())?;
            certificate_offset = certificate_offset
                .checked_add(aligned_size as u64)
                .ok_or_else(|| "certificate entry offset overflowed".to_string())?;
            if certificate_table_end < certificate_offset {
                return Err("certificate entry alignment extends past the table".to_string());
            }
        }

        Ok(None)
    }
}

fn read_at(file: &mut (impl Read + Seek), offset: u64, output: &mut [u8]) -> Result<(), String> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("seek failed: {error}"))?;
    file.read_exact(output)
        .map_err(|error| format!("read failed: {error}"))
}

#[cfg(test)]
mod test {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn token_is_found_in_installer_file_names() {
        let token = [0xa5; DOWNLOAD_CLAIM_TOKEN_SIZE];
        let encoded = hex::encode(token);

        // Name as served, name after a browser de-duplicated it, and an upper case copy.
        for file_name in [
            format!("playit-windows-x86_64-signed-{encoded}.msi"),
            format!("playit-windows-x86_64-signed-{encoded} (1).msi"),
            format!("PLAYIT-{}.MSI", encoded.to_uppercase()),
        ] {
            assert_eq!(token_from_file_name(&file_name), Some(token), "{file_name}");
        }

        // Too short, too long, and no token at all.
        for file_name in [
            format!("playit-{}.msi", &encoded[..62]),
            format!("playit-{encoded}00.msi"),
            "playit-windows-x86_64-signed.msi".to_string(),
        ] {
            assert_eq!(token_from_file_name(&file_name), None, "{file_name}");
        }
    }

    #[test]
    fn token_file_round_trips_and_removes() {
        let token = [0x5a; DOWNLOAD_CLAIM_TOKEN_SIZE];
        let dir =
            std::env::temp_dir().join(format!("playitd-download-claim-{}", std::process::id()));
        let path = dir.join("nested").join("download_claim.token");

        assert_eq!(DownloadClaimFile::read(&path).unwrap(), None);
        DownloadClaimFile::write(&path, &token).unwrap();
        assert_eq!(DownloadClaimFile::read(&path).unwrap(), Some(token));
        DownloadClaimFile::remove(&path).unwrap();
        assert_eq!(DownloadClaimFile::read(&path).unwrap(), None);
        DownloadClaimFile::remove(&path).unwrap();

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_download_claim_after_authenticode_certificate() {
        let expected_token = [0xa5; DOWNLOAD_CLAIM_TOKEN_SIZE];
        let executable = executable_with_download_claim(expected_token);
        let mut reader = Cursor::new(&executable);

        let claim = EmbeddedDownloadClaim::read_from(&mut reader, executable.len() as u64)
            .unwrap()
            .expect("download claim should be present");

        assert_eq!(claim.encoded_token(), hex::encode(expected_token));
    }

    #[test]
    fn rejects_oversized_certificate_table_without_allocating() {
        let mut executable = executable_with_download_claim([0xa5; DOWNLOAD_CLAIM_TOKEN_SIZE]);
        let security_directory_offset = 0x80 + 24 + PE32_PLUS_DATA_DIRECTORIES_OFFSET as usize + 32;
        executable[security_directory_offset + 4..security_directory_offset + 8]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        let mut reader = Cursor::new(&executable);

        let error = EmbeddedDownloadClaim::read_from(&mut reader, executable.len() as u64)
            .err()
            .expect("oversized certificate table should fail");

        assert_eq!(error, "certificate table exceeds the supported size");
    }

    fn executable_with_download_claim(token: [u8; DOWNLOAD_CLAIM_TOKEN_SIZE]) -> Vec<u8> {
        const PE_OFFSET: usize = 0x80;
        const OPTIONAL_HEADER_SIZE: usize = 0xf0;
        const CERTIFICATE_TABLE_OFFSET: usize = 0x200;
        const AUTHENTICODE_CERTIFICATE_SIZE: usize = 16;

        let certificate_table_size =
            AUTHENTICODE_CERTIFICATE_SIZE + PLAYIT_CERTIFICATE_SIZE as usize;
        let mut executable = vec![0u8; CERTIFICATE_TABLE_OFFSET + certificate_table_size];
        executable[..2].copy_from_slice(b"MZ");
        executable[0x3c..0x40].copy_from_slice(&(PE_OFFSET as u32).to_le_bytes());
        executable[PE_OFFSET..PE_OFFSET + 4].copy_from_slice(b"PE\0\0");
        executable[PE_OFFSET + 20..PE_OFFSET + 22]
            .copy_from_slice(&(OPTIONAL_HEADER_SIZE as u16).to_le_bytes());

        let optional_header_offset = PE_OFFSET + 24;
        executable[optional_header_offset..optional_header_offset + 2]
            .copy_from_slice(&PE32_PLUS_MAGIC.to_le_bytes());
        let data_directory_count_offset =
            optional_header_offset + PE32_PLUS_DATA_DIRECTORIES_OFFSET as usize - 4;
        executable[data_directory_count_offset..data_directory_count_offset + 4]
            .copy_from_slice(&16u32.to_le_bytes());
        let security_directory_offset =
            optional_header_offset + PE32_PLUS_DATA_DIRECTORIES_OFFSET as usize + 32;
        executable[security_directory_offset..security_directory_offset + 4]
            .copy_from_slice(&(CERTIFICATE_TABLE_OFFSET as u32).to_le_bytes());
        executable[security_directory_offset + 4..security_directory_offset + 8]
            .copy_from_slice(&(certificate_table_size as u32).to_le_bytes());

        executable[CERTIFICATE_TABLE_OFFSET..CERTIFICATE_TABLE_OFFSET + 4]
            .copy_from_slice(&(AUTHENTICODE_CERTIFICATE_SIZE as u32).to_le_bytes());
        executable[CERTIFICATE_TABLE_OFFSET + 4..CERTIFICATE_TABLE_OFFSET + 6]
            .copy_from_slice(&CERTIFICATE_REVISION_2.to_le_bytes());
        executable[CERTIFICATE_TABLE_OFFSET + 6..CERTIFICATE_TABLE_OFFSET + 8]
            .copy_from_slice(&2u16.to_le_bytes());

        let playit_certificate_offset = CERTIFICATE_TABLE_OFFSET + AUTHENTICODE_CERTIFICATE_SIZE;
        executable[playit_certificate_offset..playit_certificate_offset + 4]
            .copy_from_slice(&PLAYIT_CERTIFICATE_SIZE.to_le_bytes());
        executable[playit_certificate_offset + 4..playit_certificate_offset + 6]
            .copy_from_slice(&CERTIFICATE_REVISION_2.to_le_bytes());
        executable[playit_certificate_offset + 6..playit_certificate_offset + 8]
            .copy_from_slice(&PLAYIT_CERTIFICATE_TYPE.to_le_bytes());
        executable[playit_certificate_offset + 8..playit_certificate_offset + 24]
            .copy_from_slice(PLAYIT_CERTIFICATE_MAGIC);
        executable[playit_certificate_offset + 24..playit_certificate_offset + 56]
            .copy_from_slice(&token);
        executable
    }
}
