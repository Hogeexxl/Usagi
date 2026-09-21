//! Resolution and validation of the Codex source configuration.
//!
//! Codex configuration is deliberately resolved outside the generic storage
//! layer.  The resulting value is an immutable input to `CodexAdapter`, so a
//! scan cannot accidentally observe a different home halfway through a run.

use std::{
    env, fmt, fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::platform::paths;

const STATE_INDEX_FILENAME: &str = "state_5.sqlite";
const SESSION_INDEX_FILENAME: &str = "session_index.jsonl";
const GLOBAL_STATE_FILENAME: &str = ".codex-global-state.json";

#[derive(Clone, Debug)]
pub struct CodexConfig {
    home: PathBuf,
    home_fingerprint: String,
    metadata: CodexMetadataPaths,
}

#[derive(Clone, Debug)]
pub struct CodexMetadataPaths {
    state_index_path: PathBuf,
    session_index_path: PathBuf,
    global_state_path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct CodexConfigError {
    code: &'static str,
    attempted_home: PathBuf,
    resolved_home: Option<PathBuf>,
    metadata_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub enum CodexConfigResolution {
    Ready(CodexConfig),
    Invalid(CodexConfigError),
}

impl CodexConfig {
    /// Resolve the configured Codex home from `CODEX_HOME`, the platform home,
    /// or the historical relative `.codex` fallback.
    pub fn resolve_default() -> CodexConfigResolution {
        let candidate = env::var_os("CODEX_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| paths::home_dir().map(|home| home.join(".codex")))
            .unwrap_or_else(|| PathBuf::from(".codex"));
        Self::from_candidate(candidate)
    }

    /// Resolve an explicitly supplied home.  The environment is intentionally
    /// ignored for this path so tests and callers can inject a stable seam.
    pub fn from_home(home: impl Into<PathBuf>) -> CodexConfigResolution {
        Self::from_candidate(home.into())
    }

    #[cfg(test)]
    pub(crate) fn with_metadata_paths(
        home: impl Into<PathBuf>,
        metadata: CodexMetadataPaths,
    ) -> CodexConfigResolution {
        let attempted_home = home.into();
        let resolved_home = match paths::normalize_path(attempted_home.clone()) {
            Ok(home) => home,
            Err(_) => {
                return CodexConfigResolution::Invalid(CodexConfigError {
                    code: "CODEX_HOME_RESOLUTION_FAILED",
                    attempted_home,
                    resolved_home: None,
                    metadata_path: None,
                });
            }
        };
        if !resolved_home.is_absolute() {
            return CodexConfigResolution::Invalid(CodexConfigError {
                code: "CODEX_HOME_NOT_ABSOLUTE",
                attempted_home,
                resolved_home: Some(resolved_home),
                metadata_path: None,
            });
        }
        let metadata_paths = [
            metadata.state_index_path.as_path(),
            metadata.session_index_path.as_path(),
            metadata.global_state_path.as_path(),
        ];
        for path in metadata_paths {
            if !metadata_path_within_home(path, &resolved_home) {
                return CodexConfigResolution::Invalid(CodexConfigError {
                    code: "CODEX_METADATA_HOME_MISMATCH",
                    attempted_home,
                    resolved_home: Some(resolved_home),
                    metadata_path: Some(path.to_path_buf()),
                });
            }
        }
        CodexConfigResolution::Ready(Self {
            home_fingerprint: fingerprint(&resolved_home),
            home: resolved_home,
            metadata,
        })
    }

    fn from_candidate(attempted_home: PathBuf) -> CodexConfigResolution {
        let resolved_home = match paths::normalize_path(attempted_home.clone()) {
            Ok(home) => home,
            Err(_) => {
                return CodexConfigResolution::Invalid(CodexConfigError {
                    code: "CODEX_HOME_RESOLUTION_FAILED",
                    attempted_home,
                    resolved_home: None,
                    metadata_path: None,
                });
            }
        };
        if !resolved_home.is_absolute() {
            return CodexConfigResolution::Invalid(CodexConfigError {
                code: "CODEX_HOME_NOT_ABSOLUTE",
                attempted_home,
                resolved_home: Some(resolved_home),
                metadata_path: None,
            });
        }
        let metadata = CodexMetadataPaths::from_home(&resolved_home);
        for path in [
            metadata.state_index_path(),
            metadata.session_index_path(),
            metadata.global_state_path(),
        ] {
            if !metadata_path_within_home(path, &resolved_home) {
                return CodexConfigResolution::Invalid(CodexConfigError {
                    code: "CODEX_METADATA_HOME_MISMATCH",
                    attempted_home,
                    resolved_home: Some(resolved_home),
                    metadata_path: Some(path.to_path_buf()),
                });
            }
        }
        CodexConfigResolution::Ready(Self {
            home_fingerprint: fingerprint(&resolved_home),
            home: resolved_home,
            metadata,
        })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn home_fingerprint(&self) -> &str {
        &self.home_fingerprint
    }

    pub fn metadata(&self) -> &CodexMetadataPaths {
        &self.metadata
    }
}

impl CodexMetadataPaths {
    fn from_home(home: &Path) -> Self {
        Self {
            state_index_path: home.join(STATE_INDEX_FILENAME),
            session_index_path: home.join(SESSION_INDEX_FILENAME),
            global_state_path: home.join(GLOBAL_STATE_FILENAME),
        }
    }

    pub fn state_index_path(&self) -> &Path {
        &self.state_index_path
    }

    pub fn session_index_path(&self) -> &Path {
        &self.session_index_path
    }

    pub fn global_state_path(&self) -> &Path {
        &self.global_state_path
    }
}

impl CodexConfigError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn attempted_home(&self) -> &Path {
        &self.attempted_home
    }

    pub fn resolved_home(&self) -> Option<&Path> {
        self.resolved_home.as_deref()
    }

    pub fn metadata_path(&self) -> Option<&Path> {
        self.metadata_path.as_deref()
    }
}

impl fmt::Display for CodexConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for CodexConfigError {}

impl CodexConfigResolution {
    pub fn quota_home(&self) -> &Path {
        match self {
            Self::Ready(config) => config.home(),
            Self::Invalid(error) => error.resolved_home().unwrap_or(error.attempted_home()),
        }
    }
}

fn fingerprint(home: &Path) -> String {
    let digest = Sha256::digest(home.to_string_lossy().as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Validate metadata containment using canonical filesystem identities.  A
/// metadata file may not exist yet, so the nearest existing prefix is checked
/// and every existing symlink component is resolved before accepting it.
fn metadata_path_within_home(path: &Path, home: &Path) -> bool {
    let Some(home) = paths::normalize_absolute_path(home) else {
        return false;
    };
    let Some(path) = paths::normalize_absolute_path(path) else {
        return false;
    };
    let Ok(canonical_home) = fs::canonicalize(&home) else {
        return false;
    };
    if let Ok(canonical_path) = fs::canonicalize(&path) {
        return canonical_path.starts_with(&canonical_home);
    }

    let mut existing = path.clone();
    loop {
        match fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !existing.pop() {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    let Ok(existing_metadata) = fs::symlink_metadata(&existing) else {
        return false;
    };
    if !existing_metadata.is_dir() && !existing_metadata.file_type().is_symlink() {
        return false;
    }
    let Ok(canonical_existing) = fs::canonicalize(&existing) else {
        return false;
    };
    if !canonical_existing.starts_with(&canonical_home) {
        return false;
    }

    let mut component_path = PathBuf::new();
    for component in path.components() {
        component_path.push(component.as_os_str());
        let metadata = match fs::symlink_metadata(&component_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return false,
        };
        if metadata.file_type().is_symlink() {
            let Ok(resolved) = fs::canonicalize(&component_path) else {
                return false;
            };
            let is_home_alias = !component_path.starts_with(&canonical_home)
                && canonical_home.starts_with(&resolved);
            if !is_home_alias {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::Arc,
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use crate::{
        ingestion::{IngestionConfig, IngestionCoordinator},
        source::SourceRegistry,
        storage::{Ledger, LedgerOptions},
    };

    #[test]
    fn relative_candidate_is_normalized_before_absolute_guard() {
        let resolution = CodexConfig::from_home(PathBuf::from(".codex"));
        assert!(matches!(
            &resolution,
            CodexConfigResolution::Invalid(error)
                if error.code() != "CODEX_HOME_NOT_ABSOLUTE"
                    && error.resolved_home().is_some_and(Path::is_absolute)
        ));
    }

    #[test]
    fn fingerprint_is_lowercase_sha256_of_normalized_home() {
        let home = std::env::temp_dir().join("usagi-codex-config-fingerprint");
        fs::create_dir_all(&home).unwrap();
        let resolution = CodexConfig::from_home(&home);
        let CodexConfigResolution::Ready(config) = resolution else {
            panic!("expected ready config")
        };
        assert_eq!(config.home_fingerprint().len(), 64);
        assert!(
            config
                .home_fingerprint()
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn phase2_c11_legacy_codex_binding_failure_is_failed_not_skipped() {
        let root = std::env::temp_dir().join(format!(
            "usagi-codex-binding-mismatch-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = root.join("codex");
        let outside = root.join("metadata-outside");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let metadata = CodexMetadataPaths {
            state_index_path: outside.join("state_5.sqlite"),
            session_index_path: home.join("session_index.jsonl"),
            global_state_path: home.join(".codex-global-state.json"),
        };
        let resolution = CodexConfig::with_metadata_paths(&home, metadata);
        assert!(matches!(
            resolution,
            CodexConfigResolution::Invalid(ref error)
                if error.code() == "CODEX_METADATA_HOME_MISMATCH"
        ));

        let ledger = Arc::new(Ledger::open(LedgerOptions::new(root.join("mu.sqlite3"))).unwrap());
        let mut registry = SourceRegistry::new();
        registry
            .register(crate::codex::CodexAdapter::new(resolution))
            .unwrap();
        let scanner =
            IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
                .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while scanner.source_reports().is_empty() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for source report"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            ledger
                .app_state()
                .unwrap()
                .scan
                .last_scan_error_code
                .as_deref(),
            Some("SOURCE_RUN_FAILED")
        );
        let report = scanner
            .source_reports()
            .into_iter()
            .find(|report| report.source.as_str() == "codex")
            .unwrap();
        assert_eq!(report.state.as_str(), "failed");
        assert_eq!(
            report.error_code.as_deref(),
            Some("CODEX_METADATA_HOME_MISMATCH")
        );
        scanner.shutdown().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn metadata_path_rejects_external_symlink_with_missing_child() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join("usagi-codex-config-symlink");
        let home = root.join("codex");
        let outside = root.join("outside");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, home.join("alias")).unwrap();
        let metadata = CodexMetadataPaths {
            state_index_path: home.join("alias/missing.sqlite"),
            session_index_path: home.join("state.jsonl"),
            global_state_path: home.join("global.json"),
        };
        assert!(matches!(
            CodexConfig::with_metadata_paths(&home, metadata),
            CodexConfigResolution::Invalid(error)
                if error.code() == "CODEX_METADATA_HOME_MISMATCH"
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn metadata_path_accepts_private_var_alias_for_missing_child() {
        let root = std::env::temp_dir().join("usagi-codex-config-private-var");
        fs::create_dir_all(&root).unwrap();
        let canonical_home = fs::canonicalize(&root).unwrap();
        let relative = canonical_home
            .strip_prefix("/private/var")
            .expect("temporary directory is under /private/var");
        let var_home = PathBuf::from("/var").join(relative);
        let private_home = PathBuf::from("/private/var").join(relative);
        assert!(metadata_path_within_home(
            &var_home.join("missing.sqlite"),
            &private_home
        ));
        assert!(metadata_path_within_home(
            &private_home.join("missing.sqlite"),
            &var_home
        ));
        let _ = fs::remove_dir_all(root);
    }
}
