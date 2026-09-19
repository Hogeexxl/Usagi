//! Physical file identity and metadata adapters.

use std::{
    fs::{File, Metadata},
    io,
    path::Path,
};

#[cfg(not(unix))]
use std::time::UNIX_EPOCH;

/// Stable storage-compatible physical identity.
///
/// Unix stores device and inode directly. Windows stores a stable blake3
/// mapping of the volume serial and file index in the same two non-negative
/// SQLite slots. The mapping contains no path data, so renames preserve it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlatformFileIdentity {
    pub device_id: u64,
    pub inode: u64,
}

impl PlatformFileIdentity {
    pub const fn new(device_id: u64, inode: u64) -> Self {
        Self { device_id, inode }
    }

    pub fn from_file(file: &File) -> io::Result<Self> {
        identity_from_file(file)
    }

    pub fn from_path(path: &Path) -> io::Result<Self> {
        identity_from_path(path)
    }

    pub fn storage_slots(self) -> Option<(i64, i64)> {
        Some((
            i64::try_from(self.device_id).ok()?,
            i64::try_from(self.inode).ok()?,
        ))
    }
}

/// Size, modification time and physical identity observed together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileMetadata {
    pub identity: PlatformFileIdentity,
    pub size: u64,
    pub mtime_ns: i64,
}

impl FileMetadata {
    pub fn from_file(file: &File) -> io::Result<Self> {
        metadata_from_file(file)
    }
}

/// Obtain physical identity from an already-open handle.
pub fn identity_from_file(file: &File) -> io::Result<PlatformFileIdentity> {
    identity_from_handle(file)
}

/// Open a path and obtain its physical identity without reading its body.
pub fn identity_from_path(path: &Path) -> io::Result<PlatformFileIdentity> {
    let file = File::open(path)?;
    identity_from_file(&file)
}

/// Obtain a complete metadata snapshot from an open handle.
pub fn metadata_from_file(file: &File) -> io::Result<FileMetadata> {
    let metadata = file.metadata()?;
    Ok(FileMetadata {
        identity: identity_from_file(file)?,
        size: metadata.len(),
        mtime_ns: modified_ns(&metadata)?,
    })
}

/// Read modification time in nanoseconds since the Unix epoch.
pub fn modified_ns(metadata: &Metadata) -> io::Result<i64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let seconds = metadata.mtime();
        let nanos = metadata.mtime_nsec();
        if seconds < 0 || nanos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "negative modification time",
            ));
        }
        seconds
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(nanos))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "modification time overflow"))
    }
    #[cfg(not(unix))]
    {
        metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "modification time before epoch")
            })?
            .as_nanos()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "modification time overflow"))
    }
}

fn identity_from_handle(file: &File) -> io::Result<PlatformFileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        let device_id = i64::try_from(metadata.dev()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "device identity exceeds storage range",
            )
        })?;
        let inode = i64::try_from(metadata.ino()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "inode identity exceeds storage range",
            )
        })?;
        Ok(PlatformFileIdentity::new(
            u64::try_from(device_id).expect("non-negative Unix device identity"),
            u64::try_from(inode).expect("non-negative Unix inode identity"),
        ))
    }
    #[cfg(windows)]
    {
        windows_identity(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "physical file identity is unsupported on this platform",
        ))
    }
}

#[cfg(windows)]
fn windows_identity(file: &File) -> io::Result<PlatformFileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle},
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    let result =
        unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut information) };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    let file_index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    let mut bytes = [0_u8; 12];
    bytes[..4].copy_from_slice(&information.dwVolumeSerialNumber.to_le_bytes());
    bytes[4..].copy_from_slice(&file_index.to_le_bytes());
    let digest = blake3::hash(&bytes);
    let mut device_bytes = [0_u8; 8];
    let mut inode_bytes = [0_u8; 8];
    device_bytes.copy_from_slice(&digest.as_bytes()[..8]);
    inode_bytes.copy_from_slice(&digest.as_bytes()[8..16]);
    let mut device_id = u64::from_le_bytes(device_bytes) & i64::MAX as u64;
    let mut inode = u64::from_le_bytes(inode_bytes) & i64::MAX as u64;
    if device_id == 0 && inode == 0 {
        inode = 1;
    }
    if device_id == 0 && inode == 0 {
        device_id = 1;
    }
    Ok(PlatformFileIdentity::new(device_id, inode))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "usagi-platform-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn t_dist_003_identity_is_stable_and_does_not_use_path() {
        let first = temp_path("identity-a");
        let second = temp_path("identity-b");
        fs::write(&first, b"body").unwrap();
        let first_identity = identity_from_path(&first).unwrap();
        assert_eq!(first_identity, identity_from_path(&first).unwrap());
        fs::rename(&first, &second).unwrap();
        assert_eq!(first_identity, identity_from_path(&second).unwrap());
        fs::remove_file(second).unwrap();
    }

    #[test]
    fn t_dist_003_replacement_gets_a_new_identity() {
        let path = temp_path("identity-replacement");
        fs::write(&path, b"one").unwrap();
        let before = identity_from_path(&path).unwrap();
        let replacement = path.with_extension("replacement");
        fs::write(&replacement, b"two").unwrap();
        #[cfg(windows)]
        fs::remove_file(&path).unwrap();
        fs::rename(replacement, &path).unwrap();
        let after = identity_from_path(&path).unwrap();
        assert_ne!(before, after);
        assert!(after.storage_slots().is_some_and(|slots| slots != (0, 0)));
        fs::remove_file(path).unwrap();
    }
}
