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
        Self::resolve_default_from_home(paths::home_dir())
    }

    fn resolve_default_from_home(home: Option<PathBuf>) -> AntigravityConfigResolution {
        match home {
            Some(home) => Self::from_home(home.join(".gemini").join("antigravity")),
            None => AntigravityConfigResolution::Invalid(AntigravityConfigError::new(
                "could not resolve the user home directory",
                None,
            )),
        }
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

        // Check directory readability.  The mode check keeps this stable when
        // tests run as root (where read_dir(000) would otherwise succeed).
        if !directory_has_read_permission(home) {
            return AntigravityConfigResolution::Invalid(AntigravityConfigError::new(
                format!("home directory is not readable: {}", home.display()),
                Some(home.to_path_buf()),
            ));
        }
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

    /// Revalidate the optional summary DB immediately before a scan opens it.
    pub(crate) fn summaries_path_for_scan(
        &self,
    ) -> Result<Option<PathBuf>, AntigravityConfigError> {
        validate_child_file_or_absent(&self.summaries_path, &self.canonical_root)
    }

    /// Revalidate the optional conversations directory immediately before a scan reads it.
    pub(crate) fn conversations_dir_for_scan(
        &self,
    ) -> Result<Option<PathBuf>, AntigravityConfigError> {
        validate_child_dir_or_absent(&self.conversations_dir, &self.canonical_root)
    }

    /// Validate a discovered conversation DB and return its canonical path.
    pub(crate) fn conversation_db_path_for_scan(
        &self,
        path: &Path,
    ) -> Result<PathBuf, AntigravityConfigError> {
        let metadata = fs::symlink_metadata(path).map_err(|err| {
            AntigravityConfigError::new(
                format!(
                    "failed to read conversation DB metadata {}: {err}",
                    path.display()
                ),
                Some(path.to_path_buf()),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(AntigravityConfigError::new(
                format!(
                    "conversation DB path cannot be a symlink: {}",
                    path.display()
                ),
                Some(path.to_path_buf()),
            ));
        }

        validate_child_file_or_absent_with(path, &self.canonical_root, false, |canonical| {
            fs::File::open(canonical).map(drop)
        })?
        .ok_or_else(|| {
            AntigravityConfigError::new(
                format!(
                    "conversation DB disappeared during discovery: {}",
                    path.display()
                ),
                Some(path.to_path_buf()),
            )
        })
    }
}

fn validate_child_file_or_absent(
    path: &Path,
    canonical_root: &Path,
) -> Result<Option<PathBuf>, AntigravityConfigError> {
    validate_child_file_or_absent_with(path, canonical_root, true, |canonical| {
        fs::File::open(canonical).map(drop)
    })
}

fn validate_child_file_or_absent_with(
    path: &Path,
    canonical_root: &Path,
    allow_symlink: bool,
    check_readable: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<Option<PathBuf>, AntigravityConfigError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(AntigravityConfigError::new(
                format!("failed to read child metadata {}: {err}", path.display()),
                Some(path.to_path_buf()),
            ));
        }
    };

    if !allow_symlink && meta.file_type().is_symlink() {
        return Err(AntigravityConfigError::new(
            format!("path cannot be a symlink: {}", path.display()),
            Some(path.to_path_buf()),
        ));
    }

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

    let canonical_metadata = fs::metadata(&canonical).map_err(|err| {
        AntigravityConfigError::new(
            format!(
                "failed to read canonical child metadata {}: {err}",
                canonical.display()
            ),
            Some(path.to_path_buf()),
        )
    })?;
    if !canonical_metadata.is_file() {
        return Err(AntigravityConfigError::new(
            format!("canonical child is not a file: {}", canonical.display()),
            Some(path.to_path_buf()),
        ));
    }

    if !file_has_read_permission(&canonical_metadata) {
        return Err(AntigravityConfigError::new(
            format!("child file is not readable: {}", path.display()),
            Some(path.to_path_buf()),
        ));
    }
    check_readable(&canonical).map_err(|err| {
        AntigravityConfigError::new(
            format!("child file is not readable {}: {err}", path.display()),
            Some(path.to_path_buf()),
        )
    })?;

    Ok(Some(canonical))
}

fn validate_child_dir_or_absent(
    path: &Path,
    canonical_root: &Path,
) -> Result<Option<PathBuf>, AntigravityConfigError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
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

    let canonical_metadata = fs::metadata(&canonical).map_err(|err| {
        AntigravityConfigError::new(
            format!(
                "failed to read canonical child metadata {}: {err}",
                canonical.display()
            ),
            Some(path.to_path_buf()),
        )
    })?;
    if !canonical_metadata.is_dir() {
        return Err(AntigravityConfigError::new(
            format!(
                "canonical child is not a directory: {}",
                canonical.display()
            ),
            Some(path.to_path_buf()),
        ));
    }

    // Check directory readability
    if !directory_has_read_permission(&canonical) {
        return Err(AntigravityConfigError::new(
            format!("child directory is not readable: {}", path.display()),
            Some(path.to_path_buf()),
        ));
    }
    if let Err(err) = fs::read_dir(&canonical) {
        return Err(AntigravityConfigError::new(
            format!("child directory is not readable {}: {err}", path.display()),
            Some(path.to_path_buf()),
        ));
    }

    Ok(Some(canonical))
}

#[cfg(unix)]
fn file_has_read_permission(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o444 != 0
}

#[cfg(not(unix))]
fn file_has_read_permission(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn directory_has_read_permission(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o444 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn directory_has_read_permission(_path: &Path) -> bool {
    true
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
    fn default_resolution_without_home_is_invalid() {
        let res = AntigravityConfig::resolve_default_from_home(None);
        match res {
            AntigravityConfigResolution::Invalid(err) => {
                assert_eq!(err.code(), ANTIGRAVITY_CONFIG_INVALID);
                assert!(err.message().contains("home directory"));
            }
            other => panic!("expected Invalid without a home directory, got {other:?}"),
        }
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

    #[test]
    fn existing_summary_file_read_permission_error_is_invalid() {
        let home = create_temp_home("summary-permission-injected");
        let summary = home.join("conversation_summaries.db");
        fs::write(&summary, b"summary db").unwrap();
        let canonical_root = home.canonicalize().unwrap();

        let err = validate_child_file_or_absent_with(&summary, &canonical_root, true, |_| {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .unwrap_err();
        assert_eq!(err.code(), ANTIGRAVITY_CONFIG_INVALID);
        assert!(err.message().contains("not readable"));

        // A missing optional file remains distinguishable from an access error.
        let missing = home.join("missing.db");
        let missing_result =
            validate_child_file_or_absent_with(&missing, &canonical_root, true, |_| {
                panic!("readability check must not run for a missing file")
            })
            .unwrap();
        assert_eq!(missing_result, None);

        let _ = fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn existing_unreadable_summary_file_makes_config_invalid() {
        use std::os::unix::fs::PermissionsExt;

        struct RestoreGuard(PathBuf);
        impl Drop for RestoreGuard {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o644));
            }
        }

        let home = create_temp_home("summary-permission");
        let summary = home.join("conversation_summaries.db");
        fs::write(&summary, b"summary db").unwrap();
        let guard = RestoreGuard(summary.clone());
        fs::set_permissions(&summary, fs::Permissions::from_mode(0o000)).unwrap();

        match AntigravityConfig::from_home(&home) {
            AntigravityConfigResolution::Invalid(err) => {
                assert_eq!(err.code(), ANTIGRAVITY_CONFIG_INVALID);
                assert!(err.message().contains("not readable"));
            }
            other => panic!("expected Invalid for unreadable summary DB, got {other:?}"),
        }

        drop(guard);
        let _ = fs::remove_dir_all(&home);
    }
}
