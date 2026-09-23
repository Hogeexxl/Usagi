//! Workspace URI parsing and Project evaluation for Antigravity.

use url::Url;

use crate::platform::paths;

/// Result of evaluating a workspace reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceEvaluation {
    /// Authoritative workspace is explicitly empty.
    Projectless,
    /// Exactly one valid local path meeting all invariants.
    Project {
        project_name: String,
        project_path: String,
    },
    /// Malformed, remote host, non-file, multiple URIs, root without basename, etc.
    Unknown,
    /// SQL NULL or a wrong SQLite type in `workspace_uris`.
    Keep,
}

/// Parse and evaluate a single workspace URI string.
pub fn evaluate_workspace_uri(uri_str: &str) -> WorkspaceEvaluation {
    let trimmed = uri_str.trim();
    if trimmed.is_empty() {
        return WorkspaceEvaluation::Projectless;
    }

    let url = match Url::parse(trimmed) {
        Ok(u) => u,
        Err(_) => return WorkspaceEvaluation::Unknown,
    };

    // [INV-PROJECT-01] only file: scheme accepted
    if url.scheme() != "file" {
        return WorkspaceEvaluation::Unknown;
    }

    // Host must be empty or localhost
    match url.host_str() {
        None | Some("") | Some("localhost") => {}
        _ => return WorkspaceEvaluation::Unknown,
    }

    // [INV-PROJECT-02] percent decoding and platform path
    let file_path = match url.to_file_path() {
        Ok(p) => p,
        Err(_) => return WorkspaceEvaluation::Unknown,
    };

    let normalized = match paths::normalize_source_path(&file_path) {
        Some(p) => p,
        None => return WorkspaceEvaluation::Unknown,
    };

    if !normalized.is_absolute() {
        return WorkspaceEvaluation::Unknown;
    }

    // Path must be valid UTF-8 and contain no control characters
    let path_str = match normalized.to_str() {
        Some(s) => s,
        None => return WorkspaceEvaluation::Unknown,
    };

    if path_str.chars().any(char::is_control) {
        return WorkspaceEvaluation::Unknown;
    }

    // Basename must exist, be valid UTF-8, trim non-empty, and have no control characters
    let file_name = match normalized.file_name() {
        Some(n) => n,
        None => return WorkspaceEvaluation::Unknown,
    };

    let name_str = match file_name.to_str() {
        Some(s) => s,
        None => return WorkspaceEvaluation::Unknown,
    };

    let trimmed_name = name_str.trim();
    if trimmed_name.is_empty() || trimmed_name.chars().any(char::is_control) {
        return WorkspaceEvaluation::Unknown;
    }

    WorkspaceEvaluation::Project {
        project_name: trimmed_name.to_string(),
        project_path: path_str.to_string(),
    }
}

/// Evaluate `workspace_uris` from `conversation_summaries.db`.
pub fn evaluate_summary_workspace(workspace_uris: Option<&str>) -> WorkspaceEvaluation {
    let raw = match workspace_uris {
        Some(r) => r,
        None => return WorkspaceEvaluation::Keep,
    };

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return WorkspaceEvaluation::Projectless;
    }

    // Try parsing as JSON array
    match serde_json::from_str::<Vec<String>>(trimmed) {
        Ok(uris) => {
            if uris.is_empty() {
                WorkspaceEvaluation::Projectless
            } else if uris.len() == 1 {
                evaluate_workspace_uri(&uris[0])
            } else {
                WorkspaceEvaluation::Unknown
            }
        }
        Err(_) => {
            // Malformed serialization
            WorkspaceEvaluation::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_td_p1_uri_01_cross_platform_uris() {
        // Explicit empty -> Projectless
        assert_eq!(evaluate_workspace_uri(""), WorkspaceEvaluation::Projectless);
        assert_eq!(
            evaluate_workspace_uri("   "),
            WorkspaceEvaluation::Projectless
        );

        // Non-file scheme -> Unknown
        assert_eq!(
            evaluate_workspace_uri("http://localhost/path"),
            WorkspaceEvaluation::Unknown
        );
        assert_eq!(
            evaluate_workspace_uri("ftp://localhost/path"),
            WorkspaceEvaluation::Unknown
        );

        // Remote host -> Unknown
        assert_eq!(
            evaluate_workspace_uri("file://remote-host/path"),
            WorkspaceEvaluation::Unknown
        );

        // Path with control characters -> Unknown
        assert_eq!(
            evaluate_workspace_uri("file:///path/with\x00null"),
            WorkspaceEvaluation::Unknown
        );

        #[cfg(not(windows))]
        {
            // Valid POSIX file URI with percent-decoded Unicode basename
            let res = evaluate_workspace_uri("file:///synthetic/Antigravity%20%E9%A1%B9%E7%9B%AE");
            match res {
                WorkspaceEvaluation::Project {
                    project_name,
                    project_path,
                } => {
                    assert_eq!(project_name, "Antigravity 项目");
                    assert_eq!(project_path, "/synthetic/Antigravity 项目");
                }
                other => panic!("expected Project, got {other:?}"),
            }

            // Percent-encoded URI
            let res_encoded = evaluate_workspace_uri("file:///synthetic/My%20Project");
            match res_encoded {
                WorkspaceEvaluation::Project {
                    project_name,
                    project_path,
                } => {
                    assert_eq!(project_name, "My Project");
                    assert_eq!(project_path, "/synthetic/My Project");
                }
                other => panic!("expected Project, got {other:?}"),
            }

            // Root path without basename -> Unknown
            assert_eq!(
                evaluate_workspace_uri("file:///"),
                WorkspaceEvaluation::Unknown
            );
        }

        #[cfg(windows)]
        {
            // Valid Windows drive URI
            let res =
                evaluate_workspace_uri("file:///C:/synthetic/Antigravity%20%E9%A1%B9%E7%9B%AE");
            match res {
                WorkspaceEvaluation::Project {
                    project_name,
                    project_path,
                } => {
                    assert_eq!(project_name, "Antigravity 项目");
                    assert!(project_path.contains("Antigravity"));
                }
                other => panic!("expected Project, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_td_p1_workspace_auth_01_summary_matrix() {
        // 1. Summary exists, explicit empty JSON array
        assert_eq!(
            evaluate_summary_workspace(Some("[]")),
            WorkspaceEvaluation::Projectless
        );
        assert_eq!(
            evaluate_summary_workspace(Some("")),
            WorkspaceEvaluation::Projectless
        );

        // 2. Summary exists, single valid URI
        #[cfg(not(windows))]
        {
            assert_eq!(
                evaluate_summary_workspace(Some(
                    "[\"file:///synthetic/Antigravity%20%E9%A1%B9%E7%9B%AE\"]"
                )),
                WorkspaceEvaluation::Project {
                    project_name: "Antigravity 项目".into(),
                    project_path: "/synthetic/Antigravity 项目".into()
                }
            );
        }

        // 3. Summary exists, multiple URIs -> Unknown
        assert_eq!(
            evaluate_summary_workspace(Some("[\"file:///a\", \"file:///b\"]")),
            WorkspaceEvaluation::Unknown
        );

        // 4. Summary exists, malformed serialization -> Unknown
        assert_eq!(
            evaluate_summary_workspace(Some("not-a-json")),
            WorkspaceEvaluation::Unknown
        );

        // 5. Summary exists, SQL NULL -> Keep
        assert_eq!(evaluate_summary_workspace(None), WorkspaceEvaluation::Keep);
    }
}
