//! Platform-standard paths used by Usagi.

use std::{
    env, fs, io,
    path::{Component, Path, PathBuf},
};

use directories::BaseDirs;

const DATABASE_DIRECTORY: &str = "Usagi";
const LEGACY_DATABASE_DIRECTORY: &str = "MiniUsage";
const DATABASE_FILENAME: &str = "mu.sqlite3";

/// Return the current user's platform home directory.
pub fn home_dir() -> Option<PathBuf> {
    BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

/// Resolve the default Codex home without creating it.
pub fn default_codex_home() -> PathBuf {
    home_dir()
        .map(|home| home.join(".codex"))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

/// Resolve the canonical Usagi database path.
///
/// `BaseDirs::data_local_dir` maps to `~/Library/Application Support` on
/// macOS and `%LOCALAPPDATA%` on Windows.
pub fn default_database_path() -> PathBuf {
    let path = BaseDirs::new()
        .map(|dirs| {
            dirs.data_local_dir()
                .join(DATABASE_DIRECTORY)
                .join(DATABASE_FILENAME)
        })
        .unwrap_or_else(|| PathBuf::from(DATABASE_FILENAME));
    normalize_path(path).unwrap_or_else(|_| PathBuf::from(DATABASE_FILENAME))
}

/// Resolve the database path used by versions released before the Usagi rename.
///
/// This is migration-only compatibility state. New data is never intentionally
/// created in this location.
pub(crate) fn legacy_database_path() -> Option<PathBuf> {
    let path = BaseDirs::new()?
        .data_local_dir()
        .join(LEGACY_DATABASE_DIRECTORY)
        .join(DATABASE_FILENAME);
    normalize_path(path).ok()
}

/// Resolve an explicitly supplied or environment-selected Codex home.
pub fn resolve_codex_home(explicit: Option<PathBuf>) -> PathBuf {
    resolve_codex_home_with_env(
        explicit,
        non_empty_env_path("CODEX_HOME").map(PathBuf::into_os_string),
    )
}

/// Resolve a path to an absolute, stable representation for internal use.
/// Existing paths are canonicalized; missing paths use component-aware lexical
/// normalization and never string-rewrite Windows prefixes or UNC roots.
pub fn normalize_path(path: PathBuf) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path
    } else {
        env::current_dir()?.join(path)
    };
    let normalized = match fs::canonicalize(&absolute) {
        Ok(canonical) => canonical,
        Err(_) => lexically_normalize(&absolute),
    };
    Ok(simplify_verbatim_path(normalized))
}

/// Normalize an already borrowed path, returning `None` for relative paths.
pub fn normalize_absolute_path(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    Some(simplify_verbatim_path(lexically_normalize_strict(path)?))
}

/// Normalize a Codex source path into Usagi's one internal identity form.
/// Existing paths are canonicalized when possible; otherwise a strict lexical
/// normalization is used. Windows verbatim disk/UNC prefixes are simplified in
/// both cases so adapters and discovery compare the same representation.
pub fn normalize_source_path(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let normalized = fs::canonicalize(path)
        .ok()
        .map(simplify_verbatim_path)
        .or_else(|| normalize_absolute_path(path))?;
    Some(normalized)
}

/// Compare two absolute source paths after applying the same platform rules.
pub fn same_source_path(left: &Path, right: &Path) -> bool {
    match (normalize_source_path(left), normalize_source_path(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn resolve_codex_home_with_env(
    explicit: Option<PathBuf>,
    env_home: Option<std::ffi::OsString>,
) -> PathBuf {
    explicit
        .or_else(|| {
            env_home
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(default_codex_home)
}

fn lexically_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn lexically_normalize_strict(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    Some(normalized)
}

#[cfg(not(windows))]
fn simplify_verbatim_path(path: PathBuf) -> PathBuf {
    path
}

#[cfg(windows)]
fn simplify_verbatim_path(path: PathBuf) -> PathBuf {
    use std::{ffi::OsString, path::Prefix};

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path;
    };
    let mut result = match prefix.kind() {
        Prefix::VerbatimDisk(letter) => PathBuf::from(format!("{}:\\", letter as char)),
        Prefix::VerbatimUNC(server, share) => {
            let mut value = OsString::from(r"\\");
            value.push(server);
            value.push(r"\");
            value.push(share);
            value.push(r"\");
            PathBuf::from(value)
        }
        _ => return path,
    };
    for component in components {
        if matches!(component, Component::RootDir) {
            continue;
        }
        result.push(component.as_os_str());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t_dist_002_default_database_path_keeps_historical_suffix() {
        let path = default_database_path();
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("mu.sqlite3")
        );
        assert_eq!(
            path.parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
            Some("Usagi")
        );
        #[cfg(target_os = "macos")]
        assert!(
            path.to_string_lossy()
                .contains("Library/Application Support/Usagi")
        );
    }

    #[test]
    fn t_dist_002_explicit_env_default_precedence_and_empty_env() {
        assert_eq!(non_empty_env_path("USAGI_TEST_MISSING"), None);
        let explicit = PathBuf::from(if cfg!(windows) {
            r"C:\用户\.codex"
        } else {
            "/tmp/用户/.codex"
        });
        let env_home = PathBuf::from(if cfg!(windows) {
            r"C:\环境\.codex"
        } else {
            "/tmp/环境/.codex"
        });
        assert_eq!(
            resolve_codex_home_with_env(Some(explicit.clone()), Some(env_home.clone().into())),
            explicit
        );
        assert_eq!(
            resolve_codex_home_with_env(None, Some(env_home.clone().into())),
            env_home
        );
        assert_eq!(
            resolve_codex_home_with_env(None, Some(std::ffi::OsString::new())),
            default_codex_home()
        );
    }

    #[test]
    fn t_dist_002_unicode_and_parent_components_are_preserved() {
        let path = PathBuf::from(if cfg!(windows) {
            r"C:\用户\Usagi\..\Codex"
        } else {
            "/tmp/用户/Usagi/../Codex"
        });
        let normalized = normalize_absolute_path(&path).expect("absolute path");
        assert!(normalized.to_string_lossy().contains("用户"));
        assert!(normalized.ends_with("Codex"));
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_disk_and_unc_paths_have_one_internal_display_form() {
        let disk = normalize_absolute_path(Path::new(r"\\?\C:\Users\用户\Codex")).unwrap();
        assert_eq!(disk, PathBuf::from(r"C:\Users\用户\Codex"));
        let unc = normalize_absolute_path(Path::new(r"\\?\UNC\server\share\Codex")).unwrap();
        assert_eq!(unc, PathBuf::from(r"\\server\share\Codex"));
        assert!(same_source_path(
            Path::new(r"\\?\C:\Users\用户\Codex"),
            Path::new(r"C:\Users\用户\Codex")
        ));
    }
}
