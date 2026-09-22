//! Antigravity configuration resolution and validation.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use crate::platform::paths;

/// Public error code for Antigravity configuration failures.
pub const ANTIGRAVITY_CONFIG_INVALID: &str = "ANTIGRAVITY_CONFIG_INVALID";

/// Resolved configuration for the Antigravity source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AntigravityConfig {
    home: PathBuf,
    canonical_root: PathBuf,
    summaries_path: PathBuf,
    conversations_dir: PathBuf,
    annotations_dir: PathBuf,
}

impl AntigravityConfig {
    /// Resolve the default Antigravity home (`~/.gemini/antigravity`).
    pub fn resolve_default() -> AntigravityConfigResolution {
        let home = paths::home_dir()
            .map(|h| h.join(".gemini").join("antigravity"))
            .unwrap_or_else(|| PathBuf::from(".gemini").join("antigravity"));
        Self::from_home(home)
    }

    /// Resolve an explicit candidate home directory.
    pub fn from_home(home: impl Into<PathBuf>) -> AntigravityConfigResolution {
        let home = home.into();
        Self::resolve_path(&home)
    }

    fn resolve_path(home: &Path) -> AntigravityConfigResolution {
        // [INV-SRC-04] / [INV-SRC-05] Home existence and symlink checks
        let metadata = match fs::symlink_metadata(home) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return AntigravityConfigResolution::NotInstalled;
            }
            Err(err) => {
                return AntigravityConfigResolution::Invalid(AntigravityConfigError::new(
                    format!("failed to read metadata for home {}: {err}", home.display()),
                    Some(home.to_path_buf()),
                ));
            }
        };

        if metadata.file_type().is_symlink() {
            return AntigravityConfigResolution::Invalid(AntigravityConfigError::new(
                format!("home cannot be a symlink: {}", home.display()),
                Some(home.to_path_buf()),
            ));
        }

        if !metadata.is_dir() {
            return AntigravityConfigResolution::Invalid(AntigravityConfigError::new(
                format!("home must be a directory: {}", home.display()),
                Some(home.to_path_buf()),
            ));
        }

        // Check directory readability
        if let Err(err) = fs::read_dir(home) {
            return AntigravityConfigResolution::Invalid(AntigravityConfigError::new(
                format!("failed to read home directory {}: {err}", home.display()),
                Some(home.to_path_buf()),
            ));
        }

        let canonical_root = match home.canonicalize() {
            Ok(canon) => canon,
            Err(err) => {
                return AntigravityConfigResolution::Invalid(AntigravityConfigError::new(
                    format!("failed to canonicalize home {}: {err}", home.display()),
                    Some(home.to_path_buf()),
                ));
            }
        };

        let summaries_path = home.join("conversation_summaries.db");
        let conversations_dir = home.join("conversations");
        let annotations_dir = home.join("annotations");

        // Validate summaries_path if it already exists
        if let Err(err) = validate_child_file_or_absent(&summaries_path, &canonical_root) {
            return AntigravityConfigResolution::Invalid(err);
        }

        // Validate conversations_dir if it already exists
        if let Err(err) = validate_child_dir_or_absent(&conversations_dir, &canonical_root) {
            return AntigravityConfigResolution::Invalid(err);
        }

        // Validate annotations_dir if it already exists
        if let Err(err) = validate_child_dir_or_absent(&annotations_dir, &canonical_root) {
            return AntigravityConfigResolution::Invalid(err);
        }

        AntigravityConfigResolution::Ready(AntigravityConfig {
            home: home.to_path_buf(),
            canonical_root,
            summaries_path,
            conversations_dir,
            annotations_dir,
        })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn canonical_root(&self) -> &Path {
        &self.canonical_root
    }

    pub fn summaries_path(&self) -> &Path {
        &self.summaries_path
    }

    pub fn conversations_dir(&self) -> &Path {
        &self.conversations_dir
    }

    pub fn annotations_dir(&self) -> &Path {
        &self.annotations_dir
    }

    /// Check containment of a canonical path within this config's canonical root.
    pub fn contains_canonical_path(&self, canonical_path: &Path) -> bool {
        canonical_path.starts_with(&self.canonical_root)
    }
}

fn validate_child_file_or_absent(
    path: &Path,
    canonical_root: &Path,
) -> Result<(), AntigravityConfigError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(AntigravityConfigError::new(
                format!("failed to read child metadata {}: {err}", path.display()),
                Some(path.to_path_buf()),
            ));
        }
    };

    if !meta.is_file() && !meta.file_type().is_symlink() {
        return Err(AntigravityConfigError::new(
            format!("path exists but is not a file: {}", path.display()),
            Some(path.to_path_buf()),
        ));
    }

    let canonical = path.canonicalize().map_err(|err| {
        AntigravityConfigError::new(
            format!("failed to canonicalize child {}: {err}", path.display()),
            Some(path.to_path_buf()),
        )
    })?;

    if !canonical.starts_with(canonical_root) {
        return Err(AntigravityConfigError::new(
            format!(
                "child path {} escapes canonical root {}",
                canonical.display(),
                canonical_root.display()
            ),
            Some(path.to_path_buf()),
        ));
    }

    Ok(())
}

fn validate_child_dir_or_absent(
    path: &Path,
    canonical_root: &Path,
) -> Result<(), AntigravityConfigError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(AntigravityConfigError::new(
                format!("failed to read child metadata {}: {err}", path.display()),
                Some(path.to_path_buf()),
            ));
        }
    };

    if !meta.is_dir() && !meta.file_type().is_symlink() {
        return Err(AntigravityConfigError::new(
            format!("path exists but is not a directory: {}", path.display()),
            Some(path.to_path_buf()),
        ));
    }

    let canonical = path.canonicalize().map_err(|err| {
        AntigravityConfigError::new(
            format!("failed to canonicalize child {}: {err}", path.display()),
            Some(path.to_path_buf()),
        )
    })?;

    if !canonical.starts_with(canonical_root) {
        return Err(AntigravityConfigError::new(
            format!(
                "child path {} escapes canonical root {}",
                canonical.display(),
                canonical_root.display()
            ),
            Some(path.to_path_buf()),
        ));
    }

    // Check directory readability
    if let Err(err) = fs::read_dir(path) {
        return Err(AntigravityConfigError::new(
            format!("child directory is not readable {}: {err}", path.display()),
            Some(path.to_path_buf()),
        ));
    }

    Ok(())
}

/// Pure helper for classifying permission errors.
pub(crate) fn is_permission_denied(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::PermissionDenied
}

/// Diagnostic configuration error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AntigravityConfigError {
    message: String,
    path: Option<PathBuf>,
}

impl AntigravityConfigError {
    pub(crate) fn new(message: impl Into<String>, path: Option<PathBuf>) -> Self {
        Self {
            message: message.into(),
            path,
        }
    }

    pub fn not_a_directory(path: PathBuf) -> Self {
        Self::new(
            format!("home must be a directory: {}", path.display()),
            Some(path),
        )
    }

    /// Return the public error code, fixed to `ANTIGRAVITY_CONFIG_INVALID`.
    pub const fn code(&self) -> &'static str {
        ANTIGRAVITY_CONFIG_INVALID
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl fmt::Display for AntigravityConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for AntigravityConfigError {}

/// Result of resolving the Antigravity configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AntigravityConfigResolution {
    Ready(AntigravityConfig),
    NotInstalled,
    Invalid(AntigravityConfigError),
}

impl AntigravityConfigResolution {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    pub fn is_not_installed(&self) -> bool {
        matches!(self, Self::NotInstalled)
    }

    pub fn is_invalid(&self) -> bool {
        matches!(self, Self::Invalid(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let c = COUNTER.fetch_add(1, Ordering::Relaxed);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{t}-{c}")
    }

    fn create_temp_home(suffix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("usagi-ag-test-{}-{}", suffix, unique_id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn test_td_p1_config_01_absent_home_is_not_installed() {
        let missing = std::env::temp_dir().join(format!("nonexistent-ag-{}", unique_id()));
        let res = AntigravityConfig::from_home(&missing);
        assert_eq!(res, AntigravityConfigResolution::NotInstalled);
    }

    #[test]
    fn test_td_p1_config_01_missing_optional_child_is_ready() {
        let home = create_temp_home("optional");
        // No summaries, no conversations, no annotations
        let res = AntigravityConfig::from_home(&home);
        match res {
            AntigravityConfigResolution::Ready(cfg) => {
                assert_eq!(cfg.home(), home);
                assert!(!cfg.conversations_dir().exists());
                assert!(!cfg.summaries_path().exists());
            }
            other => panic!("expected Ready, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn test_td_p1_config_01_discovered_db_symlink_escape() {
        let home = create_temp_home("escape");
        let external = create_temp_home("outside");

        // conversations dir symlink pointing outside
        let conv_dir = home.join("conversations");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&external, &conv_dir).unwrap();
            let res = AntigravityConfig::from_home(&home);
            match res {
                AntigravityConfigResolution::Invalid(err) => {
                    assert_eq!(err.code(), ANTIGRAVITY_CONFIG_INVALID);
                    assert!(err.message().contains("escapes canonical root"));
                }
                other => panic!("expected Invalid, got {other:?}"),
            }
        }
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&external);
    }

    #[test]
    fn test_td_p1_config_code_01_four_invalid_types() {
        // 1. Path type invalid (home is a file)
        let file_home = std::env::temp_dir().join(format!("ag-file-home-{}", unique_id()));
        fs::write(&file_home, b"not a dir").unwrap();
        let res = AntigravityConfig::from_home(&file_home);
        match res {
            AntigravityConfigResolution::Invalid(err) => {
                assert_eq!(err.code(), ANTIGRAVITY_CONFIG_INVALID);
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
        let _ = fs::remove_file(&file_home);

        // 2. Home is a symlink
        let real_home = create_temp_home("real");
        let symlink_home = std::env::temp_dir().join(format!("ag-symlink-home-{}", unique_id()));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real_home, &symlink_home).unwrap();
            let res = AntigravityConfig::from_home(&symlink_home);
            match res {
                AntigravityConfigResolution::Invalid(err) => {
                    assert_eq!(err.code(), ANTIGRAVITY_CONFIG_INVALID);
                    assert!(err.message().contains("cannot be a symlink"));
                }
                other => panic!("expected Invalid, got {other:?}"),
            }
            let _ = fs::remove_file(&symlink_home);
        }
        let _ = fs::remove_dir_all(&real_home);

        // 3. Child path is wrong type (conversations is a file)
        let home = create_temp_home("child-type");
        fs::write(home.join("conversations"), b"file not dir").unwrap();
        let res = AntigravityConfig::from_home(&home);
        match res {
            AntigravityConfigResolution::Invalid(err) => {
                assert_eq!(err.code(), ANTIGRAVITY_CONFIG_INVALID);
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn test_td_p1_permission_01_permission_denied() {
        // Test private classifier
        let perm_err = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(is_permission_denied(&perm_err));
        let not_found_err = io::Error::from(io::ErrorKind::NotFound);
        assert!(!is_permission_denied(&not_found_err));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let home = create_temp_home("perm");
            let conv_dir = home.join("conversations");
            fs::create_dir_all(&conv_dir).unwrap();

            // Guard to restore permissions
            struct RestoreGuard(PathBuf);
            impl Drop for RestoreGuard {
                fn drop(&mut self) {
                    let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
                }
            }
            let _guard = RestoreGuard(conv_dir.clone());

            fs::set_permissions(&conv_dir, fs::Permissions::from_mode(0o000)).unwrap();
            let res = AntigravityConfig::from_home(&home);
            match res {
                AntigravityConfigResolution::Invalid(err) => {
                    assert_eq!(err.code(), ANTIGRAVITY_CONFIG_INVALID);
                }
                other => panic!("expected Invalid for unreadable dir, got {other:?}"),
            }
            drop(_guard);
            let _ = fs::remove_dir_all(&home);
        }
    }
}
