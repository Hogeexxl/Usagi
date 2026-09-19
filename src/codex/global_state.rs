//! Read-only adapter for Codex Desktop's global state file.
//!
//! Only the two fields needed to identify projectless threads are retained.
//! The parser intentionally drops every other JSON value, including prompt,
//! message, and preview fields, before returning a snapshot.

use std::{
    collections::BTreeSet,
    fs::File,
    io::{self, Read},
    path::Path,
};

use serde::{
    Deserialize, Deserializer,
    de::{IgnoredAny, MapAccess, Visitor},
};

use super::DiagnosticSeverity;

const SOURCE_KIND: &str = "codex_global_state_json";
const PROJECTLESS_FIELD: &str = "projectless-thread-ids";
const ASSIGNMENTS_FIELD: &str = "thread-project-assignments";

/// The read outcome for `.codex-global-state.json`.
///
/// `NotPresent` is the expected state on installations that do not use
/// Codex Desktop.  The other non-complete states are deliberately distinct:
/// malformed JSON/schema must not be treated as an empty, valid snapshot, and
/// an I/O failure must not be confused with a missing file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalStateStatus {
    Complete,
    NotPresent,
    Malformed,
    Unreadable,
}

impl GlobalStateStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::NotPresent => "not_present",
            Self::Malformed => "malformed",
            Self::Unreadable => "unreadable",
        }
    }

    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// A privacy-safe diagnostic for the global-state side source.
///
/// Diagnostics contain only fixed codes, field names, and source kind.  In
/// particular, they never carry parser error text or raw JSON values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalStateDiagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub field: Option<String>,
    pub source_kind: &'static str,
}

impl GlobalStateDiagnostic {
    fn new(code: &'static str, severity: DiagnosticSeverity) -> Self {
        Self {
            code: code.to_owned(),
            severity,
            field: None,
            source_kind: SOURCE_KIND,
        }
    }

    fn field(mut self, field: &'static str) -> Self {
        self.field = Some(field.to_owned());
        self
    }
}

/// The minimal typed view of Codex Desktop global state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalStateSnapshot {
    pub status: GlobalStateStatus,
    pub projectless_thread_ids: Vec<String>,
    pub thread_project_assignments: BTreeSet<String>,
    pub diagnostics: Vec<GlobalStateDiagnostic>,
}

impl GlobalStateSnapshot {
    pub fn unavailable(status: GlobalStateStatus, diagnostics: Vec<GlobalStateDiagnostic>) -> Self {
        debug_assert!(!status.is_complete());
        Self {
            status,
            projectless_thread_ids: Vec::new(),
            thread_project_assignments: BTreeSet::new(),
            diagnostics,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.status.is_complete()
    }

    pub fn is_projectless(&self, thread_id: &str) -> bool {
        self.projectless_thread_ids
            .iter()
            .any(|candidate| candidate == thread_id)
    }

    pub fn has_assignment(&self, thread_id: &str) -> bool {
        self.thread_project_assignments.contains(thread_id)
    }
}

/// Read-only global-state adapter.
#[derive(Clone, Copy, Debug, Default)]
pub struct GlobalStateReader;

impl GlobalStateReader {
    pub const fn new() -> Self {
        Self
    }

    /// Read one snapshot from a path.  All outcomes, including missing and
    /// unreadable files, are returned as typed status values.
    pub fn read_snapshot<P: AsRef<Path>>(path: P) -> GlobalStateSnapshot {
        Self::new().read_path(path)
    }

    pub fn read_path<P: AsRef<Path>>(self, path: P) -> GlobalStateSnapshot {
        let path = path.as_ref();
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && matches!(path.try_exists(), Ok(false)) =>
            {
                return GlobalStateSnapshot::unavailable(
                    GlobalStateStatus::NotPresent,
                    vec![GlobalStateDiagnostic::new(
                        "file_not_present",
                        DiagnosticSeverity::Info,
                    )],
                );
            }
            Err(_) => {
                return GlobalStateSnapshot::unavailable(
                    GlobalStateStatus::Unreadable,
                    vec![GlobalStateDiagnostic::new(
                        "file_unreadable",
                        DiagnosticSeverity::Warning,
                    )],
                );
            }
        };

        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return GlobalStateSnapshot::unavailable(
                GlobalStateStatus::Unreadable,
                vec![GlobalStateDiagnostic::new(
                    "file_unreadable",
                    DiagnosticSeverity::Warning,
                )],
            );
        }
        self.parse_bytes(&bytes)
    }

    /// Parse bytes without retaining the source buffer.  This is useful for
    /// deterministic adapter tests and keeps file I/O out of the parser.
    pub fn parse_bytes(&self, bytes: &[u8]) -> GlobalStateSnapshot {
        let parsed = match serde_json::from_slice::<RawGlobalState>(bytes) {
            Ok(parsed) => parsed,
            Err(_) => {
                return GlobalStateSnapshot::unavailable(
                    GlobalStateStatus::Malformed,
                    vec![GlobalStateDiagnostic::new(
                        "invalid_json",
                        DiagnosticSeverity::Warning,
                    )],
                );
            }
        };
        parse_state(parsed)
    }
}

#[derive(Debug, Deserialize)]
struct RawGlobalState {
    #[serde(rename = "projectless-thread-ids")]
    projectless_thread_ids: Option<Vec<String>>,
    #[serde(rename = "thread-project-assignments")]
    thread_project_assignments: Option<RawAssignmentKeys>,
}

#[derive(Debug)]
struct RawAssignmentKeys {
    keys: BTreeSet<String>,
    duplicate_keys: Vec<String>,
}

impl<'de> Deserialize<'de> for RawAssignmentKeys {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AssignmentKeysVisitor;

        impl<'de> Visitor<'de> for AssignmentKeysVisitor {
            type Value = RawAssignmentKeys;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object mapping thread identifiers to assignments")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut keys = BTreeSet::new();
                let mut duplicate_keys = Vec::new();
                while let Some(thread_id) = map.next_key::<String>()? {
                    map.next_value::<IgnoredAny>()?;
                    let thread_id = thread_id.trim().to_owned();
                    if !keys.insert(thread_id.clone()) {
                        duplicate_keys.push(thread_id);
                    }
                }
                Ok(RawAssignmentKeys {
                    keys,
                    duplicate_keys,
                })
            }
        }

        deserializer.deserialize_map(AssignmentKeysVisitor)
    }
}

fn parse_state(parsed: RawGlobalState) -> GlobalStateSnapshot {
    let Some(raw_projectless_ids) = parsed.projectless_thread_ids else {
        return GlobalStateSnapshot::unavailable(
            GlobalStateStatus::Malformed,
            vec![
                GlobalStateDiagnostic::new("missing_field", DiagnosticSeverity::Warning)
                    .field(PROJECTLESS_FIELD),
            ],
        );
    };
    let Some(RawAssignmentKeys {
        keys: thread_project_assignments,
        duplicate_keys,
    }) = parsed.thread_project_assignments
    else {
        return GlobalStateSnapshot::unavailable(
            GlobalStateStatus::Malformed,
            vec![
                GlobalStateDiagnostic::new("missing_field", DiagnosticSeverity::Warning)
                    .field(ASSIGNMENTS_FIELD),
            ],
        );
    };
    let thread_project_assignments = match validate_assignment_ids(thread_project_assignments) {
        Ok(assignments) => assignments,
        Err(diagnostics) => {
            return GlobalStateSnapshot::unavailable(GlobalStateStatus::Malformed, diagnostics);
        }
    };

    let (projectless_thread_ids, mut diagnostics) = normalize_projectless_ids(raw_projectless_ids);
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "invalid_thread_id")
    {
        return GlobalStateSnapshot::unavailable(GlobalStateStatus::Malformed, diagnostics);
    }
    diagnostics.extend(duplicate_keys.into_iter().map(|_| {
        GlobalStateDiagnostic::new("duplicate_thread_id", DiagnosticSeverity::Info)
            .field(ASSIGNMENTS_FIELD)
    }));

    GlobalStateSnapshot {
        status: GlobalStateStatus::Complete,
        projectless_thread_ids,
        thread_project_assignments,
        diagnostics,
    }
}

fn normalize_projectless_ids(values: Vec<String>) -> (Vec<String>, Vec<GlobalStateDiagnostic>) {
    let mut ids = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for value in values {
        let id = value.trim();
        if !valid_identifier(id) {
            diagnostics.push(
                GlobalStateDiagnostic::new("invalid_thread_id", DiagnosticSeverity::Warning)
                    .field(PROJECTLESS_FIELD),
            );
            continue;
        }
        if !ids.insert(id.to_owned()) {
            diagnostics.push(
                GlobalStateDiagnostic::new("duplicate_thread_id", DiagnosticSeverity::Info)
                    .field(PROJECTLESS_FIELD),
            );
        }
    }
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "invalid_thread_id")
    {
        return (Vec::new(), diagnostics);
    }
    (ids.into_iter().collect(), diagnostics)
}

fn validate_assignment_ids(
    assignments: BTreeSet<String>,
) -> Result<BTreeSet<String>, Vec<GlobalStateDiagnostic>> {
    let mut diagnostics = Vec::new();
    for thread_id in &assignments {
        let thread_id = thread_id.trim();
        if !valid_identifier(thread_id) {
            diagnostics.push(
                GlobalStateDiagnostic::new("invalid_thread_id", DiagnosticSeverity::Warning)
                    .field(ASSIGNMENTS_FIELD),
            );
        }
    }
    if !diagnostics.is_empty() {
        Err(diagnostics)
    } else {
        Ok(assignments)
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(input: impl AsRef<[u8]>) -> GlobalStateSnapshot {
        GlobalStateReader::new().parse_bytes(input.as_ref())
    }

    #[test]
    fn t_s02_001_global_state_matrix_is_typed_and_privacy_safe() {
        let complete = read(
            br#"{
                "projectless-thread-ids": ["thread-b", "thread-a", "thread-a"],
                "thread-project-assignments": {
                    "thread-c": {"projectKind": "local", "path": "/workspace/c"},
                    "thread-a": {"projectKind": "local", "path": "/workspace/a"}
                },
                "prompt_history": "SECRET PROMPT",
                "preview": "SECRET PREVIEW"
            }"#,
        );
        assert_eq!(complete.status, GlobalStateStatus::Complete);
        assert_eq!(
            complete.projectless_thread_ids,
            vec!["thread-a".to_owned(), "thread-b".to_owned()]
        );
        assert!(complete.is_projectless("thread-a"));
        assert!(complete.has_assignment("thread-a"));
        assert!(format!("{complete:?}").contains("thread-a"));
        assert!(!format!("{complete:?}").contains("SECRET PROMPT"));
        assert!(!format!("{complete:?}").contains("SECRET PREVIEW"));
        assert!(
            complete
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "duplicate_thread_id")
        );

        let malformed =
            read(br#"{"projectless-thread-ids":"not-an-array","thread-project-assignments":{}}"#);
        assert_eq!(malformed.status, GlobalStateStatus::Malformed);
        assert!(malformed.projectless_thread_ids.is_empty());
        assert!(malformed.thread_project_assignments.is_empty());
        assert!(!format!("{malformed:?}").contains("not-an-array"));
    }

    #[test]
    fn t_s02_001_missing_and_unreadable_sources_are_distinct() {
        let missing = GlobalStateReader::read_snapshot(
            std::env::temp_dir().join(format!("usagi-global-state-missing-{}", std::process::id())),
        );
        assert_eq!(missing.status, GlobalStateStatus::NotPresent);

        let unreadable = GlobalStateReader::read_snapshot(std::env::temp_dir());
        assert_eq!(unreadable.status, GlobalStateStatus::Unreadable);
    }
}
