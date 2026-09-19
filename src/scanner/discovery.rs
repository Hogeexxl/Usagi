//! Safe discovery of the two Codex rollout areas.
//!
//! Discovery only performs directory enumeration and metadata/stat calls. It
//! never opens a rollout body and never follows a filesystem symlink. The
//! scanner's identity and chunk-reader stages consume this snapshot later.

use std::{
    cmp::Ordering,
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::domain::{SourceArea, SourceRegionStatus};
use crate::platform::{file_identity, paths};

/// A discovered regular rollout file and its physical identity at discovery
/// time. The path is lexically normalized but is not canonicalized or read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub source_area: SourceArea,
    pub device_id: i64,
    pub inode: i64,
    pub size: i64,
    pub mtime_ns: i64,
    pub filename_thread_id_candidate: Option<String>,
}

impl DiscoveredFile {
    pub fn identity(&self) -> (i64, i64) {
        (self.device_id, self.inode)
    }
}

/// Privacy-safe discovery diagnostic. A path may be included because it is a
/// source identifier; no file contents or OS error text is retained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryDiagnostic {
    pub code: &'static str,
    pub source_area: SourceArea,
    pub path: Option<PathBuf>,
}

/// Snapshot of both physical Codex areas. `Unavailable` never means “empty”:
/// callers must preserve existing missing/present source state for that area.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoverySnapshot {
    pub started_at_ms: i64,
    pub sessions: SourceRegionStatus,
    pub archived_sessions: SourceRegionStatus,
    pub files: Vec<DiscoveredFile>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
}

impl DiscoverySnapshot {}

/// Stateless discovery adapter.
#[derive(Clone, Copy, Debug, Default)]
pub struct Discovery;

impl Discovery {
    pub fn discover_at<P: AsRef<Path>>(codex_home: P, started_at_ms: i64) -> DiscoverySnapshot {
        let mut diagnostics = Vec::new();
        let mut files = Vec::new();
        let codex_home = paths::normalize_absolute_path(codex_home.as_ref());
        let Some(codex_home) = codex_home else {
            return DiscoverySnapshot {
                started_at_ms: started_at_ms.max(0),
                sessions: unavailable("CODEX_HOME_UNAVAILABLE"),
                archived_sessions: unavailable("CODEX_HOME_UNAVAILABLE"),
                files,
                diagnostics,
            };
        };

        let sessions_root = codex_home.join("sessions");
        let archived_root = codex_home.join("archived_sessions");
        let sessions = discover_area(
            &sessions_root,
            SourceArea::Sessions,
            &mut files,
            &mut diagnostics,
        );
        let archived_sessions = discover_area(
            &archived_root,
            SourceArea::ArchivedSessions,
            &mut files,
            &mut diagnostics,
        );

        deduplicate_physical_aliases(&mut files, &mut diagnostics);
        files.sort_by(stable_file_order);
        DiscoverySnapshot {
            started_at_ms: started_at_ms.max(0),
            sessions,
            archived_sessions,
            files,
            diagnostics,
        }
    }
}

fn discover_area(
    root: &Path,
    area: SourceArea,
    files: &mut Vec<DiscoveredFile>,
    diagnostics: &mut Vec<DiscoveryDiagnostic>,
) -> SourceRegionStatus {
    let root = paths::normalize_absolute_path(root);
    let Some(root) = root else {
        diagnostics.push(DiscoveryDiagnostic {
            code: "SOURCE_AREA_UNAVAILABLE",
            source_area: area,
            path: None,
        });
        return unavailable("SOURCE_AREA_UNAVAILABLE");
    };
    let metadata = match fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return complete(),
        Err(_) => {
            diagnostics.push(DiscoveryDiagnostic {
                code: "SOURCE_AREA_UNAVAILABLE",
                source_area: area,
                path: Some(root),
            });
            return unavailable("SOURCE_AREA_UNAVAILABLE");
        }
    };
    if metadata.file_type().is_symlink() {
        diagnostics.push(DiscoveryDiagnostic {
            code: "SOURCE_SYMLINK_REJECTED",
            source_area: area,
            path: Some(root),
        });
        return unavailable("SOURCE_SYMLINK_REJECTED");
    }
    if !metadata.is_dir() {
        diagnostics.push(DiscoveryDiagnostic {
            code: "SOURCE_AREA_UNAVAILABLE",
            source_area: area,
            path: Some(root),
        });
        return unavailable("SOURCE_AREA_UNAVAILABLE");
    }

    let mut stack = vec![root.clone()];
    let mut area_complete = true;
    while let Some(directory) = stack.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                area_complete = false;
                diagnostics.push(DiscoveryDiagnostic {
                    code: "SOURCE_AREA_UNAVAILABLE",
                    source_area: area,
                    path: Some(directory),
                });
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    area_complete = false;
                    diagnostics.push(DiscoveryDiagnostic {
                        code: "SOURCE_STAT_FAILED",
                        source_area: area,
                        path: None,
                    });
                    continue;
                }
            };
            let path = match paths::normalize_absolute_path(&entry.path()) {
                Some(path) if path.starts_with(&root) => path,
                _ => {
                    diagnostics.push(DiscoveryDiagnostic {
                        code: "SOURCE_PATH_INVALID",
                        source_area: area,
                        path: None,
                    });
                    continue;
                }
            };
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    diagnostics.push(DiscoveryDiagnostic {
                        code: "SOURCE_STAT_FAILED",
                        source_area: area,
                        path: Some(path),
                    });
                    continue;
                }
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                diagnostics.push(DiscoveryDiagnostic {
                    code: "SOURCE_SYMLINK_REJECTED",
                    source_area: area,
                    path: Some(path),
                });
                continue;
            }
            if metadata.is_dir() {
                stack.push(path);
                continue;
            }
            if !metadata.is_file() || !is_rollout_filename(&path) {
                continue;
            }
            let file = match fs::File::open(&path) {
                Ok(file) => file,
                Err(_) => {
                    area_complete = false;
                    diagnostics.push(DiscoveryDiagnostic {
                        code: "SOURCE_STAT_FAILED",
                        source_area: area,
                        path: Some(path),
                    });
                    continue;
                }
            };
            let Some(file_metadata) = file_identity::metadata_from_file(&file).ok() else {
                area_complete = false;
                diagnostics.push(DiscoveryDiagnostic {
                    code: "SOURCE_STAT_FAILED",
                    source_area: area,
                    path: Some(path),
                });
                continue;
            };
            let Some(device_id) = i64::try_from(file_metadata.identity.device_id).ok() else {
                area_complete = false;
                diagnostics.push(DiscoveryDiagnostic {
                    code: "SOURCE_STAT_FAILED",
                    source_area: area,
                    path: Some(path),
                });
                continue;
            };
            let Some(inode) = i64::try_from(file_metadata.identity.inode).ok() else {
                area_complete = false;
                diagnostics.push(DiscoveryDiagnostic {
                    code: "SOURCE_STAT_FAILED",
                    source_area: area,
                    path: Some(path),
                });
                continue;
            };
            let Some(size) = i64::try_from(file_metadata.size).ok() else {
                area_complete = false;
                diagnostics.push(DiscoveryDiagnostic {
                    code: "SOURCE_STAT_FAILED",
                    source_area: area,
                    path: Some(path),
                });
                continue;
            };
            files.push(DiscoveredFile {
                filename_thread_id_candidate: filename_thread_id(&path),
                path,
                source_area: area,
                device_id,
                inode,
                size,
                mtime_ns: file_metadata.mtime_ns,
            });
        }
    }
    if area_complete {
        complete()
    } else {
        unavailable("SOURCE_AREA_UNAVAILABLE")
    }
}

fn deduplicate_physical_aliases(
    files: &mut Vec<DiscoveredFile>,
    diagnostics: &mut Vec<DiscoveryDiagnostic>,
) {
    let mut winners: HashMap<(i64, i64), usize> = HashMap::new();
    let mut keep = vec![true; files.len()];
    for index in 0..files.len() {
        let identity = files[index].identity();
        let Some(previous) = winners.get(&identity).copied() else {
            winners.insert(identity, index);
            continue;
        };
        let preferred = compare_alias(&files[index], &files[previous]) == Ordering::Less;
        let (winner, loser) = if preferred {
            keep[previous] = false;
            (index, previous)
        } else {
            keep[index] = false;
            (previous, index)
        };
        winners.insert(identity, winner);
        diagnostics.push(DiscoveryDiagnostic {
            code: "DUPLICATE_PHYSICAL_ALIAS",
            source_area: files[loser].source_area,
            path: Some(files[loser].path.clone()),
        });
    }
    let mut index = 0;
    files.retain(|_| {
        let retain = keep[index];
        index += 1;
        retain
    });
}

fn compare_alias(left: &DiscoveredFile, right: &DiscoveredFile) -> Ordering {
    area_priority(left.source_area)
        .cmp(&area_priority(right.source_area))
        .then_with(|| left.path.cmp(&right.path))
}

fn stable_file_order(left: &DiscoveredFile, right: &DiscoveredFile) -> Ordering {
    area_priority(left.source_area)
        .cmp(&area_priority(right.source_area))
        .then_with(|| right.mtime_ns.cmp(&left.mtime_ns))
        .then_with(|| left.path.cmp(&right.path))
}

fn area_priority(area: SourceArea) -> u8 {
    match area {
        SourceArea::Sessions => 0,
        SourceArea::ArchivedSessions => 1,
    }
}

fn is_rollout_filename(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with("rollout-") && name.ends_with(".jsonl") && name.len() > "rollout-.jsonl".len()
}

fn filename_thread_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let candidate = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    Uuid::parse_str(candidate).ok().map(|uuid| uuid.to_string())
}

fn complete() -> SourceRegionStatus {
    SourceRegionStatus::Complete
}

fn unavailable(code: &'static str) -> SourceRegionStatus {
    SourceRegionStatus::Unavailable(code.to_owned())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempHome(PathBuf);

    impl TempHome {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = NEXT_TEMP.fetch_add(1, AtomicOrdering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "usagi-discovery-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let sessions = self.0.join("sessions");
                if sessions.exists() {
                    let _ = fs::set_permissions(&sessions, fs::Permissions::from_mode(0o700));
                }
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn recursively_discovers_both_areas_filters_entries_and_validates_filename_ids() {
        let home = TempHome::new();
        let sessions = home.path().join("sessions/nested");
        let archived = home.path().join("archived_sessions/year");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(&archived).unwrap();
        let valid_id = "018f47a0-1e9b-7abc-8000-000000000001";
        fs::write(
            sessions.join(format!("rollout-{valid_id}.jsonl")),
            b"main\n",
        )
        .unwrap();
        fs::write(sessions.join("rollout-not-a-uuid.jsonl"), b"legacy\n").unwrap();
        fs::write(sessions.join("rollout-wrong.txt"), b"ignored\n").unwrap();
        fs::create_dir(sessions.join("rollout-directory.jsonl")).unwrap();
        fs::write(archived.join("rollout-archived.jsonl"), b"archived\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            sessions.join(format!("rollout-{valid_id}.jsonl")),
            sessions.join("rollout-link.jsonl"),
        )
        .unwrap();

        let snapshot = Discovery::discover_at(home.path(), 10);
        assert_eq!(snapshot.sessions, SourceRegionStatus::Complete);
        assert_eq!(snapshot.archived_sessions, SourceRegionStatus::Complete);
        assert_eq!(snapshot.files.len(), 3);
        assert_eq!(snapshot.files[0].source_area, SourceArea::Sessions);
        assert_eq!(snapshot.files[2].source_area, SourceArea::ArchivedSessions);
        assert!(snapshot.files.iter().all(|file| file.path.is_absolute()));
        assert_eq!(
            snapshot
                .files
                .iter()
                .find(|file| {
                    file.path.file_name().and_then(|name| name.to_str())
                        == Some(format!("rollout-{valid_id}.jsonl").as_str())
                })
                .unwrap()
                .filename_thread_id_candidate
                .as_deref(),
            Some(valid_id)
        );
        assert!(
            snapshot
                .files
                .iter()
                .find(|file| file.path.ends_with("rollout-not-a-uuid.jsonl"))
                .unwrap()
                .filename_thread_id_candidate
                .is_none()
        );
        #[cfg(unix)]
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "SOURCE_SYMLINK_REJECTED")
        );
    }

    #[test]
    fn t_dist_004_discovery_uses_real_identity_without_reading_body() {
        let home = TempHome::new();
        let sessions = home.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join("rollout-018f47a0-1e9b-7abc-8000-000000000001.jsonl");
        fs::write(&path, b"DISCOVERY_BODY_SENTINEL\n").unwrap();

        let snapshot = Discovery::discover_at(home.path(), 100);
        let file = snapshot
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_ne!(file.identity(), (0, 0));
        assert_eq!(file.size, b"DISCOVERY_BODY_SENTINEL\n".len() as i64);
        assert!(file.mtime_ns >= 0);
    }

    #[test]
    fn missing_roots_are_complete_and_an_unreadable_root_is_unavailable() {
        let home = TempHome::new();
        let missing = Discovery::discover_at(home.path(), 10);
        assert_eq!(missing.sessions, SourceRegionStatus::Complete);
        assert_eq!(missing.archived_sessions, SourceRegionStatus::Complete);

        let sessions = home.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&sessions, fs::Permissions::from_mode(0o000)).unwrap();
            let unavailable = Discovery::discover_at(home.path(), 11);
            assert!(matches!(
                unavailable.sessions,
                SourceRegionStatus::Unavailable(_)
            ));
            fs::set_permissions(&sessions, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    #[test]
    fn physical_alias_choice_and_output_order_are_deterministic() {
        let home = TempHome::new();
        let sessions = home.path().join("sessions");
        let archived = home.path().join("archived_sessions");
        fs::create_dir(&sessions).unwrap();
        fs::create_dir(&archived).unwrap();
        let primary = sessions.join("rollout-primary.jsonl");
        fs::write(&primary, b"same\n").unwrap();
        fs::hard_link(&primary, archived.join("rollout-alias.jsonl")).unwrap();
        fs::write(sessions.join("rollout-other.jsonl"), b"other\n").unwrap();

        let first = Discovery::discover_at(home.path(), 10);
        let second = Discovery::discover_at(home.path(), 11);
        assert_eq!(first.files, second.files);
        assert_eq!(first.files.len(), 2);
        assert!(
            first
                .files
                .iter()
                .any(|file| file.path == primary && file.source_area == SourceArea::Sessions)
        );
        assert!(
            first
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "DUPLICATE_PHYSICAL_ALIAS")
        );
        assert!(
            first
                .files
                .windows(2)
                .all(|pair| stable_file_order(&pair[0], &pair[1]) != Ordering::Greater)
        );
    }
}
