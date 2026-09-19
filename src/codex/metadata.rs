//! Deterministic Thread relationship and metadata resolution.
//!
//! The resolver consumes only the allow-listed facts produced by the other
//! Codex adapters. It never receives raw JSON, database rows, message text, or
//! tool payloads.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::Path,
};

use crate::domain::{
    AgentRole, ExistingThreadProjection, FileStatus, MetadataQualityStatus, Patch, ProjectKind,
    ResolvedThreadPatch, SourceArea, SourceFileState,
};

use super::{
    GlobalStateSnapshot, GlobalStateStatus,
    rollout::{
        AgentRoleProvenance, CwdProvenance, OwnershipConfidence, ParentHintProvenance,
        RolloutThreadFact, latest_context_order_ms, normalize_agent_path,
    },
    session_index::SessionNameSnapshot,
    state_index::{StateSnapshot, StateThreadFact},
};

const MAX_PARENT_DEPTH: usize = 256;

/// Existing normalized row used only for change detection and safe `Keep`
/// decisions. This is not a storage row and carries no SQL representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExistingThread {
    pub thread_id: String,
    pub parent_thread_id: Option<String>,
    pub root_session_id: Option<String>,
    pub agent_role: AgentRole,
    pub title: Option<String>,
    pub project_name: Option<String>,
    pub project_path: Option<String>,
    pub project_kind: ProjectKind,
    pub metadata_model: Option<String>,
    pub created_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub archived: bool,
    pub current_rollout_path: Option<String>,
    pub metadata_quality_status: MetadataQualityStatus,
}

impl From<ExistingThreadProjection> for ExistingThread {
    fn from(projection: ExistingThreadProjection) -> Self {
        Self {
            thread_id: projection.thread_id,
            parent_thread_id: projection.parent_thread_id,
            root_session_id: projection.root_session_id,
            agent_role: projection.agent_role,
            title: projection.title,
            project_name: projection.project_name,
            project_path: projection.project_path,
            project_kind: projection.project_kind,
            metadata_model: projection.metadata_model,
            created_at_ms: projection.created_at_ms,
            updated_at_ms: projection.updated_at_ms,
            archived: projection.archived,
            current_rollout_path: projection.current_rollout_path,
            metadata_quality_status: projection.metadata_quality_status,
        }
    }
}

/// All source snapshots needed for one deterministic resolution view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionInput {
    pub state_snapshot: StateSnapshot,
    pub session_name_snapshot: SessionNameSnapshot,
    pub global_state_snapshot: GlobalStateSnapshot,
    pub rollout_facts: Vec<RolloutThreadFact>,
    pub source_file_observations: Vec<SourceFileState>,
    pub existing_threads: Vec<ExistingThread>,
    pub resolved_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MetadataDiagnosticSeverity {
    Warning,
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MetadataDiagnosticCode {
    InvalidResolutionTime,
    MissingSafeFact,
    FactBindingConflict,
    FieldConflict,
    ParentConflict,
    RoleConflict,
    ParentMissing,
    ParentCycle,
    ParentDepthExceeded,
    ProjectAssignmentConflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MetadataSourceKind {
    StateIndex,
    SessionIndex,
    Rollout,
    SourceObservation,
    RelationshipGraph,
    GlobalState,
}

/// Privacy-safe resolver diagnostic. Fields are identifiers, offsets, and
/// fixed enums only; no source value or raw record can be attached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataDiagnostic {
    pub code: MetadataDiagnosticCode,
    pub severity: MetadataDiagnosticSeverity,
    pub thread_id: Option<String>,
    pub source_file_id: Option<i64>,
    pub source_start_offset: Option<u64>,
    pub field: Option<&'static str>,
    pub source_kind: MetadataSourceKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionResult {
    pub patches: Vec<ResolvedThreadPatch>,
    pub diagnostics: Vec<MetadataDiagnostic>,
    pub affected_thread_ids: Vec<String>,
}

pub struct ThreadMetadataResolver;

impl ThreadMetadataResolver {
    pub fn resolve(input: ResolutionInput) -> ResolutionResult {
        Resolver::new(input).resolve()
    }
}

#[derive(Clone, Debug)]
enum ParentChoice {
    Confirmed(String),
    NoneConfirmed,
    Unresolved,
}

#[derive(Clone, Debug)]
struct Relationship {
    parent: ParentChoice,
    explicit_main: bool,
    explicit_subagent: bool,
    conflict: bool,
}

impl Default for Relationship {
    fn default() -> Self {
        Self {
            parent: ParentChoice::NoneConfirmed,
            explicit_main: false,
            explicit_subagent: false,
            conflict: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootState {
    Resolved,
    MissingParent,
    Conflict,
    Unknown,
}

struct Resolver {
    input: ResolutionInput,
    diagnostics: Vec<MetadataDiagnostic>,
    affected: BTreeSet<String>,
    state_facts: BTreeMap<String, StateThreadFact>,
    rollout_by_thread: BTreeMap<String, Vec<RolloutThreadFact>>,
    observations_by_thread: BTreeMap<String, Vec<SourceFileState>>,
    existing: BTreeMap<String, ExistingThread>,
    blocked_threads: BTreeSet<String>,
}

impl Resolver {
    fn new(input: ResolutionInput) -> Self {
        Self {
            input,
            diagnostics: Vec::new(),
            affected: BTreeSet::new(),
            state_facts: BTreeMap::new(),
            rollout_by_thread: BTreeMap::new(),
            observations_by_thread: BTreeMap::new(),
            existing: BTreeMap::new(),
            blocked_threads: BTreeSet::new(),
        }
    }

    fn resolve(mut self) -> ResolutionResult {
        self.index_inputs();
        if self.input.resolved_at_ms < 0 {
            self.diagnostic(
                MetadataDiagnosticCode::InvalidResolutionTime,
                MetadataDiagnosticSeverity::Warning,
                None,
                None,
                None,
                MetadataSourceKind::RelationshipGraph,
            );
            return self.finish(Vec::new());
        }

        let mut relationships = self.resolve_relationships();
        let cycle_nodes = self.detect_cycles(&relationships);
        for thread_id in &cycle_nodes {
            if let Some(relationship) = relationships.get_mut(thread_id) {
                relationship.conflict = true;
            }
            self.diagnostic(
                MetadataDiagnosticCode::ParentCycle,
                MetadataDiagnosticSeverity::Conflict,
                Some(thread_id),
                None,
                Some("parent_thread_id"),
                MetadataSourceKind::RelationshipGraph,
            );
        }

        let roles = self.resolve_roles(&relationships, &cycle_nodes);
        let roots = self.resolve_roots(&relationships, &roles, &cycle_nodes);
        let mut patches = Vec::new();
        let ids = self.affected.iter().cloned().collect::<Vec<_>>();
        for thread_id in ids {
            if self.blocked_threads.contains(&thread_id) {
                continue;
            }
            if let Some(patch) =
                self.resolve_thread(&thread_id, &relationships, &roles, &roots, &cycle_nodes)
            {
                patches.push(patch);
            }
        }
        patches.sort_by(|left, right| left.thread_id.cmp(&right.thread_id));
        self.finish(patches)
    }

    fn finish(mut self, patches: Vec<ResolvedThreadPatch>) -> ResolutionResult {
        self.diagnostics.sort_by(|left, right| {
            left.thread_id
                .cmp(&right.thread_id)
                .then(left.code.cmp(&right.code))
                .then(left.source_file_id.cmp(&right.source_file_id))
                .then(left.source_start_offset.cmp(&right.source_start_offset))
                .then(left.field.cmp(&right.field))
        });
        ResolutionResult {
            patches,
            diagnostics: self.diagnostics,
            affected_thread_ids: self.affected.into_iter().collect(),
        }
    }

    fn index_inputs(&mut self) {
        let state_threads = self.input.state_snapshot.threads.clone();
        for fact in state_threads {
            self.affected.insert(fact.thread_id.clone());
            match self.state_facts.get(&fact.thread_id) {
                None => {
                    self.state_facts.insert(fact.thread_id.clone(), fact);
                }
                Some(current) => {
                    let ordering = compare_state_fact(&fact, current);
                    if state_fact_value_key(&fact) != state_fact_value_key(current) {
                        self.diagnostic(
                            MetadataDiagnosticCode::FieldConflict,
                            MetadataDiagnosticSeverity::Conflict,
                            Some(&fact.thread_id),
                            None,
                            None,
                            MetadataSourceKind::StateIndex,
                        );
                    }
                    if ordering == Ordering::Less {
                        self.state_facts.insert(fact.thread_id.clone(), fact);
                    }
                }
            }
        }

        for edge in &self.input.state_snapshot.spawn_edges {
            self.affected.insert(edge.parent_thread_id.clone());
            self.affected.insert(edge.child_thread_id.clone());
        }
        for fact in &self.input.session_name_snapshot.facts {
            self.affected.insert(fact.thread_id.clone());
        }

        for fact in self.input.rollout_facts.clone() {
            self.affected.insert(fact.owning_thread_id.clone());
            self.rollout_by_thread
                .entry(fact.owning_thread_id.clone())
                .or_default()
                .push(fact);
        }
        for facts in self.rollout_by_thread.values_mut() {
            facts.sort_by_key(|fact| fact.source_file_id);
        }

        for observation in self.input.source_file_observations.clone() {
            if let Some(thread_id) = observation.thread_id.clone() {
                self.affected.insert(thread_id.clone());
                self.observations_by_thread
                    .entry(thread_id)
                    .or_default()
                    .push(observation);
            }
        }
        for observations in self.observations_by_thread.values_mut() {
            observations.sort_by_key(|source| source.source_file_id);
        }

        for thread in self.input.existing_threads.clone() {
            self.affected.insert(thread.thread_id.clone());
            self.existing.insert(thread.thread_id.clone(), thread);
        }

        self.find_incomplete_rollout_groups();
    }

    fn find_incomplete_rollout_groups(&mut self) {
        let observations = self.observations_by_thread.clone();
        for (thread_id, sources) in observations {
            for source in sources
                .iter()
                .filter(|source| source.file_status == FileStatus::Present)
            {
                let matching = self.rollout_by_thread.get(&thread_id).and_then(|facts| {
                    facts.iter().find(|fact| {
                        fact.source_file_id == source.source_file_id
                            && fact.owning_thread_id == thread_id
                            && fact.ownership_boundary.confidence == OwnershipConfidence::Confirmed
                    })
                });
                if matching.is_none() {
                    self.blocked_threads.insert(thread_id.clone());
                    self.diagnostic(
                        MetadataDiagnosticCode::MissingSafeFact,
                        MetadataDiagnosticSeverity::Warning,
                        Some(&thread_id),
                        Some(source.source_file_id),
                        None,
                        MetadataSourceKind::SourceObservation,
                    );
                }
            }
        }

        let rollout_facts = self.input.rollout_facts.clone();
        for fact in rollout_facts {
            let observed_binding = self
                .input
                .source_file_observations
                .iter()
                .find(|source| source.source_file_id == fact.source_file_id)
                .and_then(|source| source.thread_id.as_deref());
            if observed_binding.is_some_and(|binding| binding != fact.owning_thread_id) {
                self.blocked_threads.insert(fact.owning_thread_id.clone());
                self.diagnostic(
                    MetadataDiagnosticCode::FactBindingConflict,
                    MetadataDiagnosticSeverity::Conflict,
                    Some(&fact.owning_thread_id),
                    Some(fact.source_file_id),
                    None,
                    MetadataSourceKind::Rollout,
                );
            }
        }
    }

    fn resolve_relationships(&mut self) -> BTreeMap<String, Relationship> {
        let mut relationships = self
            .affected
            .iter()
            .cloned()
            .map(|id| (id, Relationship::default()))
            .collect::<BTreeMap<_, _>>();

        let mut state_parents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for edge in &self.input.state_snapshot.spawn_edges {
            state_parents
                .entry(edge.child_thread_id.clone())
                .or_default()
                .insert(edge.parent_thread_id.clone());
        }

        let ids = self.affected.iter().cloned().collect::<Vec<_>>();
        for thread_id in ids {
            let mut direct_parents = BTreeSet::new();
            let mut nested_parents = BTreeSet::new();
            let mut forked_parents = BTreeSet::new();
            let mut explicit_main = false;
            let mut explicit_subagent = false;
            let mut role_values = BTreeSet::new();
            let rollout_facts = self
                .rollout_by_thread
                .get(&thread_id)
                .cloned()
                .unwrap_or_default();
            for fact in &rollout_facts {
                if fact.has_conflict {
                    self.diagnostic(
                        MetadataDiagnosticCode::FieldConflict,
                        MetadataDiagnosticSeverity::Conflict,
                        Some(&thread_id),
                        Some(fact.source_file_id),
                        None,
                        MetadataSourceKind::Rollout,
                    );
                }
                if fact.relationship_conflict {
                    relationships.get_mut(&thread_id).unwrap().conflict = true;
                }
                if let Some(parent) = &fact.parent_thread_id_hint {
                    match parent.provenance {
                        ParentHintProvenance::SessionMetaParent => {
                            direct_parents.insert(parent.value.clone());
                        }
                        ParentHintProvenance::SubagentSource => {
                            nested_parents.insert(parent.value.clone());
                        }
                        ParentHintProvenance::ForkedFromId => {
                            forked_parents.insert(parent.value.clone());
                        }
                    }
                }
                if let Some(role) = &fact.agent_role_hint {
                    role_values.insert(role.value.to_ascii_lowercase());
                    explicit_main |= role.provenance == AgentRoleProvenance::SessionMetaRole
                        && role.value.eq_ignore_ascii_case("main");
                    explicit_subagent |= role.provenance == AgentRoleProvenance::SubagentSource
                        || role.value.eq_ignore_ascii_case("subagent");
                }
            }
            if let Some(role) = self
                .state_facts
                .get(&thread_id)
                .and_then(|fact| fact.agent_role_hint.as_deref())
            {
                if role.eq_ignore_ascii_case("subagent") {
                    explicit_subagent = true;
                } else if role.eq_ignore_ascii_case("main") {
                    explicit_main = true;
                }
            }
            if role_values.contains("main") && role_values.contains("subagent") {
                relationships.get_mut(&thread_id).unwrap().conflict = true;
                self.diagnostic(
                    MetadataDiagnosticCode::RoleConflict,
                    MetadataDiagnosticSeverity::Conflict,
                    Some(&thread_id),
                    None,
                    Some("agent_role"),
                    MetadataSourceKind::Rollout,
                );
            }

            let state = state_parents.get(&thread_id).cloned().unwrap_or_default();
            let (parent, parent_conflict) = if state.len() > 1 {
                (ParentChoice::Unresolved, true)
            } else if let Some(parent) = state.iter().next().cloned() {
                let lower_conflict = direct_parents
                    .iter()
                    .chain(nested_parents.iter())
                    .chain(forked_parents.iter())
                    .any(|candidate| candidate != &parent);
                (ParentChoice::Confirmed(parent), lower_conflict)
            } else if direct_parents.len() > 1 {
                (ParentChoice::Unresolved, true)
            } else if let Some(parent) = direct_parents.iter().next().cloned() {
                let lower_conflict = nested_parents
                    .iter()
                    .chain(forked_parents.iter())
                    .any(|candidate| candidate != &parent);
                (ParentChoice::Confirmed(parent), lower_conflict)
            } else if nested_parents.len() > 1 {
                (ParentChoice::Unresolved, true)
            } else if let Some(parent) = nested_parents.iter().next().cloned() {
                let lower_conflict = forked_parents.iter().any(|candidate| candidate != &parent);
                (ParentChoice::Confirmed(parent), lower_conflict)
            } else if forked_parents.len() > 1 {
                (ParentChoice::Unresolved, true)
            } else if let Some(parent) = forked_parents.iter().next().cloned() {
                (ParentChoice::Confirmed(parent), false)
            } else {
                (ParentChoice::NoneConfirmed, false)
            };

            let relationship = relationships.get_mut(&thread_id).unwrap();
            relationship.parent = parent;
            relationship.explicit_main = explicit_main;
            relationship.explicit_subagent = explicit_subagent
                || !direct_parents.is_empty()
                || !nested_parents.is_empty()
                || !forked_parents.is_empty();
            relationship.conflict |= parent_conflict;
            if parent_conflict {
                self.diagnostic(
                    MetadataDiagnosticCode::ParentConflict,
                    MetadataDiagnosticSeverity::Conflict,
                    Some(&thread_id),
                    None,
                    Some("parent_thread_id"),
                    MetadataSourceKind::RelationshipGraph,
                );
            }
            if matches!(relationship.parent, ParentChoice::Confirmed(_)) && explicit_main {
                relationship.conflict = true;
                self.diagnostic(
                    MetadataDiagnosticCode::RoleConflict,
                    MetadataDiagnosticSeverity::Conflict,
                    Some(&thread_id),
                    None,
                    Some("agent_role"),
                    MetadataSourceKind::RelationshipGraph,
                );
            }
        }
        relationships
    }

    fn detect_cycles(&self, relationships: &BTreeMap<String, Relationship>) -> BTreeSet<String> {
        let mut cycle_nodes = BTreeSet::new();
        for start in relationships.keys() {
            let mut positions = HashMap::new();
            let mut path = Vec::new();
            let mut current = start.as_str();
            for _ in 0..=MAX_PARENT_DEPTH {
                if let Some(position) = positions.get(current).copied() {
                    cycle_nodes.extend(path[position..].iter().cloned());
                    break;
                }
                positions.insert(current.to_owned(), path.len());
                path.push(current.to_owned());
                let Some(Relationship {
                    parent: ParentChoice::Confirmed(parent),
                    ..
                }) = relationships.get(current)
                else {
                    break;
                };
                current = parent;
            }
        }
        cycle_nodes
    }

    fn resolve_roles(
        &self,
        relationships: &BTreeMap<String, Relationship>,
        cycle_nodes: &BTreeSet<String>,
    ) -> BTreeMap<String, Option<AgentRole>> {
        relationships
            .iter()
            .map(|(thread_id, relationship)| {
                let trusted_identity = self.state_facts.contains_key(thread_id)
                    || self.rollout_by_thread.contains_key(thread_id);
                let role = if relationship.conflict || cycle_nodes.contains(thread_id) {
                    Some(AgentRole::Unknown)
                } else if matches!(relationship.parent, ParentChoice::Confirmed(_)) {
                    Some(AgentRole::Subagent)
                } else if relationship.explicit_subagent {
                    Some(AgentRole::Unknown)
                } else if relationship.explicit_main
                    || (self.input.state_snapshot.status.is_complete()
                        && self.input.state_snapshot.spawn_edges_status.is_complete()
                        && trusted_identity)
                {
                    Some(AgentRole::Main)
                } else {
                    None
                };
                (thread_id.clone(), role)
            })
            .collect()
    }

    fn resolve_roots(
        &mut self,
        relationships: &BTreeMap<String, Relationship>,
        roles: &BTreeMap<String, Option<AgentRole>>,
        cycle_nodes: &BTreeSet<String>,
    ) -> BTreeMap<String, (Option<String>, RootState)> {
        let mut roots = BTreeMap::new();
        let trusted_ids = self
            .state_facts
            .keys()
            .chain(self.rollout_by_thread.keys())
            .chain(self.existing.keys())
            .chain(
                self.input
                    .session_name_snapshot
                    .facts
                    .iter()
                    .map(|fact| &fact.thread_id),
            )
            .cloned()
            .collect::<BTreeSet<_>>();
        let ids = relationships.keys().cloned().collect::<Vec<_>>();
        for thread_id in ids {
            let (root, state) =
                walk_root(&thread_id, relationships, roles, cycle_nodes, &trusted_ids);
            match state {
                RootState::MissingParent => self.diagnostic(
                    MetadataDiagnosticCode::ParentMissing,
                    MetadataDiagnosticSeverity::Warning,
                    Some(&thread_id),
                    None,
                    Some("root_session_id"),
                    MetadataSourceKind::RelationshipGraph,
                ),
                RootState::Conflict => {}
                RootState::Unknown => {}
                RootState::Resolved => {}
            }
            roots.insert(thread_id, (root, state));
        }
        roots
    }

    fn resolve_thread(
        &mut self,
        thread_id: &str,
        relationships: &BTreeMap<String, Relationship>,
        roles: &BTreeMap<String, Option<AgentRole>>,
        roots: &BTreeMap<String, (Option<String>, RootState)>,
        cycle_nodes: &BTreeSet<String>,
    ) -> Option<ResolvedThreadPatch> {
        let existing = self.existing.get(thread_id).cloned();
        let state = self.state_facts.get(thread_id).cloned();
        let rollouts = self
            .rollout_by_thread
            .get(thread_id)
            .cloned()
            .unwrap_or_default();
        let observations = self
            .observations_by_thread
            .get(thread_id)
            .cloned()
            .unwrap_or_default();
        let relationship = relationships.get(thread_id).cloned().unwrap_or_default();
        let role = roles.get(thread_id).copied().flatten();
        let (root, root_state) = roots
            .get(thread_id)
            .cloned()
            .unwrap_or((None, RootState::Unknown));

        let source_view_complete = self.input.state_snapshot.status.is_complete()
            && self.input.state_snapshot.spawn_edges_status.is_complete()
            && self.input.session_name_snapshot.status.is_complete()
            && !self.blocked_threads.contains(thread_id);

        let mut has_conflict = relationship.conflict
            || cycle_nodes.contains(thread_id)
            || rollouts.iter().any(|fact| fact.has_conflict);
        let mut has_partial = !source_view_complete
            || role.is_none()
            || role == Some(AgentRole::Unknown)
            || root_state != RootState::Resolved;

        let title = resolve_thread_title(
            self.input.state_snapshot.status.is_complete(),
            role,
            state.as_ref(),
            self.input.session_name_snapshot.get(thread_id),
            &rollouts,
            existing.as_ref().and_then(|thread| thread.title.as_ref()),
        );

        let (project_path, cwd_conflict) = select_rollout_cwd(&rollouts);
        has_conflict |= cwd_conflict;
        let project_path = project_path
            .or_else(|| state.as_ref().and_then(|fact| fact.cwd.clone()))
            .filter(|path| has_valid_project_path(Some(path.as_str())));
        let project_name = project_path.as_deref().and_then(project_name_from_path);
        let (project_kind, project_kind_conflict) =
            self.resolve_project_kind(thread_id, project_path.as_deref(), existing.as_ref());
        has_conflict |= project_kind_conflict;

        let (rollout_model, model_conflict) = select_rollout_model(&rollouts);
        has_conflict |= model_conflict;
        let metadata_model = if self.input.state_snapshot.status.is_complete() {
            state
                .as_ref()
                .and_then(|fact| fact.metadata_model.clone())
                .or(rollout_model)
        } else {
            None
        };

        let rollout_created_at = rollouts.iter().filter_map(|fact| fact.created_at_ms).min();
        let created_at_ms = if self.input.state_snapshot.status.is_complete() {
            state
                .as_ref()
                .and_then(|fact| fact.created_at_ms)
                .or(rollout_created_at)
        } else {
            None
        };

        let latest_rollout_at = rollouts
            .iter()
            .filter_map(|fact| fact.latest_context_at_ms)
            .max();
        let session_updated_at = self
            .input
            .session_name_snapshot
            .get(thread_id)
            .and_then(|fact| fact.updated_at_ms);
        let updated_at_ms = if self.input.state_snapshot.status.is_complete() {
            state
                .as_ref()
                .and_then(|fact| fact.updated_at_ms)
                .or_else(|| latest_rollout_at.max(session_updated_at))
        } else {
            None
        };

        let physical = select_physical_source(&observations);
        let archived = if self.input.state_snapshot.status.is_complete() {
            state.as_ref().and_then(|fact| fact.archived).or_else(|| {
                physical
                    .as_ref()
                    .map(|(area, _)| *area == SourceArea::ArchivedSessions)
            })
        } else {
            None
        };
        let current_rollout_path = if self.input.state_snapshot.status.is_complete() {
            state
                .as_ref()
                .and_then(|fact| fact.rollout_path.clone())
                .or_else(|| physical.map(|(_, path)| path))
        } else {
            None
        };

        if title.is_none()
            || project_path.is_none()
            || created_at_ms.is_none()
            || updated_at_ms.is_none()
        {
            has_partial = true;
        }
        let quality = if has_conflict {
            MetadataQualityStatus::Conflict
        } else if has_partial {
            MetadataQualityStatus::Partial
        } else {
            MetadataQualityStatus::Complete
        };

        let mut patch = ResolvedThreadPatch::new(thread_id, self.input.resolved_at_ms).ok()?;
        patch.full_resolution = source_view_complete;
        patch.metadata_quality_status = quality;

        match (relationship.parent, role) {
            (ParentChoice::NoneConfirmed, Some(AgentRole::Main))
                if matches!(root_state, RootState::Resolved) =>
            {
                clear_optional_if_present(
                    &mut patch.parent_thread_id,
                    existing
                        .as_ref()
                        .and_then(|row| row.parent_thread_id.as_ref()),
                );
                set_required_role(
                    &mut patch.agent_role,
                    existing.as_ref().map(|row| row.agent_role),
                    Some(AgentRole::Main),
                );
                set_optional(
                    &mut patch.root_session_id,
                    existing
                        .as_ref()
                        .and_then(|row| row.root_session_id.as_ref()),
                    Some(thread_id.to_owned()),
                );
            }
            (ParentChoice::Confirmed(parent), Some(AgentRole::Subagent))
                if matches!(root_state, RootState::Resolved | RootState::Unknown) =>
            {
                set_optional(
                    &mut patch.parent_thread_id,
                    existing
                        .as_ref()
                        .and_then(|row| row.parent_thread_id.as_ref()),
                    Some(parent),
                );
                set_required_role(
                    &mut patch.agent_role,
                    existing.as_ref().map(|row| row.agent_role),
                    Some(AgentRole::Subagent),
                );
                if matches!(root_state, RootState::Resolved) {
                    set_optional(
                        &mut patch.root_session_id,
                        existing
                            .as_ref()
                            .and_then(|row| row.root_session_id.as_ref()),
                        root,
                    );
                }
            }
            _ => {}
        }

        set_optional(
            &mut patch.title,
            existing.as_ref().and_then(|row| row.title.as_ref()),
            title,
        );
        set_optional(
            &mut patch.project_path,
            existing.as_ref().and_then(|row| row.project_path.as_ref()),
            project_path,
        );
        set_optional(
            &mut patch.project_name,
            existing.as_ref().and_then(|row| row.project_name.as_ref()),
            project_name,
        );
        set_required_project_kind(
            &mut patch.project_kind,
            existing.as_ref().map(|row| row.project_kind),
            project_kind,
        );
        set_optional(
            &mut patch.metadata_model,
            existing
                .as_ref()
                .and_then(|row| row.metadata_model.as_ref()),
            metadata_model,
        );
        set_optional(
            &mut patch.created_at_ms,
            existing.as_ref().and_then(|row| row.created_at_ms.as_ref()),
            created_at_ms,
        );
        set_optional(
            &mut patch.updated_at_ms,
            existing.as_ref().and_then(|row| row.updated_at_ms.as_ref()),
            updated_at_ms,
        );
        if let Some(archived) = archived
            && existing.as_ref().is_none_or(|row| row.archived != archived)
        {
            patch.archived = Patch::Set(archived);
        }
        set_optional(
            &mut patch.current_rollout_path,
            existing
                .as_ref()
                .and_then(|row| row.current_rollout_path.as_ref()),
            current_rollout_path,
        );

        let quality_changed = existing
            .as_ref()
            .is_none_or(|row| row.metadata_quality_status != quality);
        if !patch_has_field_change(&patch) && !quality_changed {
            return None;
        }
        if patch.validate().is_err() {
            return None;
        }
        Some(patch)
    }

    fn resolve_project_kind(
        &mut self,
        thread_id: &str,
        project_path: Option<&str>,
        existing: Option<&ExistingThread>,
    ) -> (ProjectKind, bool) {
        let snapshot = self.input.global_state_snapshot.clone();
        match snapshot.status {
            GlobalStateStatus::Complete => {
                let is_projectless = snapshot.is_projectless(thread_id);
                let has_assignment = snapshot.has_assignment(thread_id);
                if is_projectless && has_assignment {
                    self.diagnostic(
                        MetadataDiagnosticCode::ProjectAssignmentConflict,
                        MetadataDiagnosticSeverity::Conflict,
                        Some(thread_id),
                        None,
                        Some("project_kind"),
                        MetadataSourceKind::GlobalState,
                    );
                    (ProjectKind::Unknown, true)
                } else if is_projectless {
                    (ProjectKind::Projectless, false)
                } else if has_valid_project_path(project_path) {
                    (ProjectKind::Project, false)
                } else {
                    (ProjectKind::Unknown, false)
                }
            }
            GlobalStateStatus::NotPresent => {
                if has_valid_project_path(project_path) {
                    (ProjectKind::Project, false)
                } else {
                    (ProjectKind::Unknown, false)
                }
            }
            GlobalStateStatus::Malformed | GlobalStateStatus::Unreadable => (
                existing
                    .map(|row| row.project_kind)
                    .unwrap_or(ProjectKind::Unknown),
                false,
            ),
        }
    }

    fn diagnostic(
        &mut self,
        code: MetadataDiagnosticCode,
        severity: MetadataDiagnosticSeverity,
        thread_id: Option<&str>,
        source_file_id: Option<i64>,
        field: Option<&'static str>,
        source_kind: MetadataSourceKind,
    ) {
        self.diagnostics.push(MetadataDiagnostic {
            code,
            severity,
            thread_id: thread_id.map(ToOwned::to_owned),
            source_file_id,
            source_start_offset: None,
            field,
            source_kind,
        });
    }
}

fn resolve_thread_title(
    state_complete: bool,
    role: Option<AgentRole>,
    state: Option<&StateThreadFact>,
    session_name: Option<&crate::codex::session_index::SessionNameFact>,
    rollouts: &[RolloutThreadFact],
    existing_title: Option<&String>,
) -> Option<String> {
    if state_complete {
        if let Some(title) = state.and_then(|fact| fact.name.clone().or_else(|| fact.title.clone()))
        {
            return Some(title);
        }
        if let Some(title) = session_name.map(|fact| fact.thread_name.clone()) {
            return Some(title);
        }
    }

    if role != Some(AgentRole::Subagent) || existing_title.is_some() {
        return None;
    }

    if state_complete
        && let Some(title) = state
            .and_then(|fact| fact.agent_path.as_deref())
            .and_then(subagent_title_from_agent_path)
    {
        return Some(title);
    }

    rollouts.iter().find_map(|fact| {
        fact.agent_path
            .as_ref()
            .and_then(|candidate| subagent_title_from_agent_path(&candidate.value))
    })
}

fn subagent_title_from_agent_path(agent_path: &str) -> Option<String> {
    let normalized = normalize_agent_path(agent_path)?;
    let component = normalized.rsplit('/').next()?;
    let mut collapsed = String::new();
    for character in component.replace('_', " ").chars() {
        if character == ' ' {
            if !collapsed.ends_with(' ') {
                collapsed.push(' ');
            }
        } else {
            collapsed.push(character);
        }
    }
    let collapsed = collapsed.trim();
    let mut characters = collapsed.chars();
    let first = characters.next()?;
    let mut title = first.to_uppercase().collect::<String>();
    title.extend(characters);
    Some(title)
}

fn compare_state_fact(left: &StateThreadFact, right: &StateThreadFact) -> Ordering {
    right
        .updated_at_ms
        .cmp(&left.updated_at_ms)
        .then_with(|| state_fact_value_key(left).cmp(&state_fact_value_key(right)))
}

fn state_fact_value_key(fact: &StateThreadFact) -> String {
    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        fact.name,
        fact.title,
        fact.cwd,
        fact.rollout_path,
        fact.archived,
        fact.metadata_model,
        fact.created_at_ms,
        fact.agent_role_hint
    )
}

fn walk_root(
    start: &str,
    relationships: &BTreeMap<String, Relationship>,
    roles: &BTreeMap<String, Option<AgentRole>>,
    cycle_nodes: &BTreeSet<String>,
    trusted_ids: &BTreeSet<String>,
) -> (Option<String>, RootState) {
    let mut current = start;
    let mut seen = HashSet::new();
    for _ in 0..=MAX_PARENT_DEPTH {
        if cycle_nodes.contains(current) || !seen.insert(current.to_owned()) {
            return (None, RootState::Conflict);
        }
        match roles.get(current).copied().flatten() {
            Some(AgentRole::Main) => return (Some(current.to_owned()), RootState::Resolved),
            Some(AgentRole::Unknown) => {
                let conflict = relationships
                    .get(current)
                    .is_some_and(|relationship| relationship.conflict)
                    || cycle_nodes.contains(current);
                return (
                    None,
                    if conflict {
                        RootState::Conflict
                    } else {
                        RootState::Unknown
                    },
                );
            }
            None => return (None, RootState::Unknown),
            Some(AgentRole::Subagent) => {}
        }
        let Some(relationship) = relationships.get(current) else {
            return (None, RootState::MissingParent);
        };
        match &relationship.parent {
            ParentChoice::Confirmed(parent) if trusted_ids.contains(parent) => {
                current = parent;
            }
            ParentChoice::Confirmed(_) => return (None, RootState::MissingParent),
            ParentChoice::Unresolved => return (None, RootState::Conflict),
            ParentChoice::NoneConfirmed => return (None, RootState::Unknown),
        }
    }
    (None, RootState::Conflict)
}

fn select_rollout_cwd(facts: &[RolloutThreadFact]) -> (Option<String>, bool) {
    let mut candidates = facts
        .iter()
        .filter_map(|fact| {
            fact.cwd.as_ref().map(|candidate| {
                (
                    match candidate.provenance {
                        CwdProvenance::SessionMeta => 2,
                        CwdProvenance::TurnContext => 1,
                    },
                    candidate.record_offset,
                    fact.source_file_id,
                    candidate.value.clone(),
                )
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
            .then(left.3.cmp(&right.3))
    });
    let Some(selected) = candidates.first() else {
        return (None, false);
    };
    let conflict = candidates
        .iter()
        .filter(|candidate| candidate.0 == selected.0)
        .any(|candidate| candidate.3 != selected.3);
    (Some(selected.3.clone()), conflict)
}

fn select_rollout_model(facts: &[RolloutThreadFact]) -> (Option<String>, bool) {
    let mut candidates = facts
        .iter()
        .filter_map(|fact| {
            fact.latest_context_model.as_ref().map(|model| {
                (
                    fact.latest_context_turn_id.clone(),
                    latest_context_order_ms(
                        fact.latest_context_turn_id.as_deref(),
                        fact.latest_context_at_ms,
                    ),
                    fact.latest_context_record_offset,
                    fact.source_file_id,
                    model.clone(),
                )
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then(right.2.cmp(&left.2))
            .then(left.3.cmp(&right.3))
            .then(left.4.cmp(&right.4))
    });
    let Some(selected) = candidates.first() else {
        return (None, false);
    };
    let conflict = candidates
        .iter()
        .filter(|candidate| {
            candidate.0 == selected.0 && (candidate.0.is_some() || candidate.1 == selected.1)
        })
        .any(|candidate| candidate.4 != selected.4);
    (Some(selected.4.clone()), conflict)
}

fn select_physical_source(sources: &[SourceFileState]) -> Option<(SourceArea, String)> {
    let mut present = sources
        .iter()
        .filter(|source| source.file_status == FileStatus::Present)
        .map(|source| (source.source_area, source.current_path.clone()))
        .collect::<Vec<_>>();
    present.sort_by(|left, right| {
        source_area_priority(left.0)
            .cmp(&source_area_priority(right.0))
            .then(left.1.cmp(&right.1))
    });
    present.into_iter().next()
}

fn source_area_priority(area: SourceArea) -> u8 {
    match area {
        SourceArea::Sessions => 0,
        SourceArea::ArchivedSessions => 1,
    }
}

fn project_name_from_path(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn has_valid_project_path(path: Option<&str>) -> bool {
    path.is_some_and(|path| !path.is_empty() && Path::new(path).is_absolute())
}

fn set_optional<T: Clone + PartialEq>(
    patch: &mut Patch<T>,
    existing: Option<&T>,
    desired: Option<T>,
) {
    if let Some(desired) = desired
        && existing != Some(&desired)
    {
        *patch = Patch::Set(desired);
    }
}

fn clear_optional_if_present<T>(patch: &mut Patch<T>, existing: Option<&T>) {
    if existing.is_some() {
        *patch = Patch::Clear;
    }
}

fn set_required_role(
    patch: &mut Patch<AgentRole>,
    existing: Option<AgentRole>,
    desired: Option<AgentRole>,
) {
    if let Some(desired) = desired
        && existing != Some(desired)
    {
        *patch = Patch::Set(desired);
    }
}

fn set_required_project_kind(
    patch: &mut Patch<ProjectKind>,
    existing: Option<ProjectKind>,
    desired: ProjectKind,
) {
    if existing != Some(desired) {
        *patch = Patch::Set(desired);
    }
}

fn patch_has_field_change(patch: &ResolvedThreadPatch) -> bool {
    !patch.parent_thread_id.is_keep()
        || !patch.root_session_id.is_keep()
        || !patch.agent_role.is_keep()
        || !patch.title.is_keep()
        || !patch.project_name.is_keep()
        || !patch.project_path.is_keep()
        || !patch.project_kind.is_keep()
        || !patch.metadata_model.is_keep()
        || !patch.created_at_ms.is_keep()
        || !patch.updated_at_ms.is_keep()
        || !patch.archived.is_keep()
        || !patch.current_rollout_path.is_keep()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::codex::{
        rollout::{Candidate, OwnershipBoundary},
        session_index::{SessionNameFact, SessionSourceStatus},
        state_index::{SpawnEdgeFact, SpawnEdgeSource, StateSourceStatus},
    };
    use crate::domain::{FileStatus, SourceArea};

    fn fixture_path(name: &str) -> String {
        std::env::temp_dir()
            .join("usagi-codex-metadata")
            .join(name.trim_start_matches('/'))
            .to_string_lossy()
            .into_owned()
    }

    fn state(threads: Vec<StateThreadFact>, edges: Vec<(&str, &str)>) -> StateSnapshot {
        StateSnapshot {
            status: StateSourceStatus::Complete,
            threads,
            spawn_edges: edges
                .into_iter()
                .map(|(parent, child)| SpawnEdgeFact {
                    parent_thread_id: parent.to_owned(),
                    child_thread_id: child.to_owned(),
                    status: None,
                    source: SpawnEdgeSource::StateSpawnEdge,
                    observed_at_ms: None,
                })
                .collect(),
            spawn_edges_status: StateSourceStatus::Complete,
            diagnostics: Vec::new(),
        }
    }

    fn state_thread(id: &str) -> StateThreadFact {
        StateThreadFact {
            thread_id: id.to_owned(),
            rollout_path: None,
            created_at_ms: Some(10),
            updated_at_ms: Some(20),
            archived: None,
            title: None,
            name: None,
            cwd: None,
            metadata_model: None,
            agent_role_hint: None,
            agent_path: None,
        }
    }

    fn sessions(facts: Vec<SessionNameFact>) -> SessionNameSnapshot {
        let names = facts
            .iter()
            .cloned()
            .map(|fact| (fact.thread_id.clone(), fact))
            .collect();
        SessionNameSnapshot {
            names,
            facts,
            diagnostics: Vec::new(),
            status: SessionSourceStatus::Complete,
        }
    }

    fn global_state() -> GlobalStateSnapshot {
        GlobalStateSnapshot::unavailable(GlobalStateStatus::NotPresent, Vec::new())
    }

    fn rollout(source_id: i64, owner: &str) -> RolloutThreadFact {
        RolloutThreadFact {
            source_file_id: source_id,
            owning_thread_id: owner.to_owned(),
            cwd: None,
            created_at_ms: Some(11),
            latest_context_model: None,
            latest_context_turn_id: None,
            latest_context_at_ms: None,
            latest_context_record_offset: None,
            parent_thread_id_hint: None,
            agent_role_hint: None,
            agent_path: None,
            ownership_boundary: OwnershipBoundary {
                replay_start_offset: None,
                owning_records_start_offset: Some(0),
                confidence: OwnershipConfidence::Confirmed,
            },
            has_conflict: false,
            relationship_conflict: false,
        }
    }

    fn rollout_with_agent_path(source_id: i64, owner: &str, agent_path: &str) -> RolloutThreadFact {
        let mut fact = rollout(source_id, owner);
        fact.agent_path = Some(Candidate {
            value: agent_path.to_owned(),
            provenance: crate::codex::rollout::AgentPathProvenance::SessionMeta,
            record_offset: 1,
        });
        fact
    }

    fn source(source_id: i64, owner: &str, area: SourceArea, path: &str) -> SourceFileState {
        SourceFileState::new(
            source_id,
            Some(owner.to_owned()),
            path.to_owned(),
            area,
            source_id,
            source_id,
            1,
            10,
            10,
            FileStatus::Present,
            10,
        )
        .unwrap()
    }

    fn existing(id: &str) -> ExistingThread {
        ExistingThread {
            thread_id: id.to_owned(),
            parent_thread_id: None,
            root_session_id: None,
            agent_role: AgentRole::Unknown,
            title: None,
            project_name: None,
            project_path: None,
            project_kind: ProjectKind::Unknown,
            metadata_model: None,
            created_at_ms: None,
            updated_at_ms: None,
            archived: false,
            current_rollout_path: None,
            metadata_quality_status: MetadataQualityStatus::Partial,
        }
    }

    fn existing_main(id: &str) -> ExistingThread {
        ExistingThread {
            agent_role: AgentRole::Main,
            ..existing(id)
        }
    }

    fn patch<'a>(result: &'a ResolutionResult, id: &str) -> &'a ResolvedThreadPatch {
        result
            .patches
            .iter()
            .find(|patch| patch.thread_id == id)
            .unwrap()
    }

    #[test]
    fn fixed_source_priority_and_null_keep_are_deterministic() {
        let mut primary = state_thread("thread-a");
        primary.name = Some("State Name".to_owned());
        primary.title = Some("State Title".to_owned());
        primary.cwd = Some(fixture_path("state/project"));
        primary.metadata_model = Some("state-model".to_owned());
        let null_fact = state_thread("thread-b");

        let mut rollout_fact = rollout(1, "thread-a");
        rollout_fact.cwd = Some(Candidate {
            value: fixture_path("rollout/project"),
            provenance: CwdProvenance::SessionMeta,
            record_offset: 2,
        });
        rollout_fact.latest_context_model = Some("rollout-model".to_owned());
        rollout_fact.latest_context_at_ms = Some(30);
        rollout_fact.latest_context_record_offset = Some(3);

        let mut old_b = existing("thread-b");
        old_b.title = Some("Keep Existing".to_owned());
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(vec![primary, null_fact], Vec::new()),
            session_name_snapshot: sessions(vec![SessionNameFact {
                thread_id: "thread-a".to_owned(),
                thread_name: "Session Name".to_owned(),
                updated_at_ms: Some(40),
            }]),
            global_state_snapshot: global_state(),
            rollout_facts: vec![rollout_fact],
            source_file_observations: vec![source(
                1,
                "thread-a",
                SourceArea::Sessions,
                &fixture_path("sessions/rollout-a.jsonl"),
            )],
            existing_threads: vec![existing("thread-a"), old_b],
            resolved_at_ms: 100,
        });

        let resolved = patch(&result, "thread-a");
        assert_eq!(resolved.title, Patch::Set("State Name".to_owned()));
        assert_eq!(
            resolved.project_path,
            Patch::Set(fixture_path("rollout/project"))
        );
        assert_eq!(
            resolved.metadata_model,
            Patch::Set("state-model".to_owned())
        );
        assert!(
            result
                .patches
                .iter()
                .find(|patch| patch.thread_id == "thread-b")
                .is_none_or(|patch| patch.title == Patch::Keep)
        );
        assert!(
            result
                .patches
                .windows(2)
                .all(|pair| pair[0].thread_id < pair[1].thread_id)
        );
    }

    #[test]
    fn t_mu03_a02_agent_path_title_format_and_priority() {
        assert_eq!(
            subagent_title_from_agent_path("/root/gate_b_rereview"),
            Some("Gate b rereview".to_owned())
        );
        assert_eq!(
            subagent_title_from_agent_path("/root/group/a__b"),
            Some("A b".to_owned())
        );
        assert_eq!(
            subagent_title_from_agent_path(r"/root/a\b"),
            Some(r"A\b".to_owned())
        );

        let resolve = |child: StateThreadFact,
                       session_title: Option<&str>,
                       rollout_path: Option<&str>| {
            let child_rollout = rollout_path.map(|path| rollout_with_agent_path(1, "child", path));
            let result = ThreadMetadataResolver::resolve(ResolutionInput {
                state_snapshot: state(vec![state_thread("root"), child], vec![("root", "child")]),
                session_name_snapshot: sessions(session_title.map_or_else(Vec::new, |title| {
                    vec![SessionNameFact {
                        thread_id: "child".to_owned(),
                        thread_name: title.to_owned(),
                        updated_at_ms: Some(30),
                    }]
                })),
                global_state_snapshot: global_state(),
                rollout_facts: child_rollout.into_iter().collect(),
                source_file_observations: Vec::new(),
                existing_threads: vec![existing("root"), existing("child")],
                resolved_at_ms: 100,
            });
            patch(&result, "child").title.clone()
        };

        let mut child = state_thread("child");
        child.name = Some("State Name".to_owned());
        child.title = Some("State Title".to_owned());
        child.agent_path = Some("/root/state_path".to_owned());
        assert_eq!(
            resolve(
                child.clone(),
                Some("Session Name"),
                Some("/root/rollout_path")
            ),
            Patch::Set("State Name".to_owned())
        );

        child.name = None;
        assert_eq!(
            resolve(
                child.clone(),
                Some("Session Name"),
                Some("/root/rollout_path")
            ),
            Patch::Set("State Title".to_owned())
        );

        child.title = None;
        assert_eq!(
            resolve(
                child.clone(),
                Some("Session Name"),
                Some("/root/rollout_path")
            ),
            Patch::Set("Session Name".to_owned())
        );

        child.agent_path = Some("/root/state_path".to_owned());
        assert_eq!(
            resolve(child.clone(), None, Some("/root/rollout_path")),
            Patch::Set("State path".to_owned())
        );

        child.agent_path = None;
        assert_eq!(
            resolve(child, None, Some("/root/rollout_path")),
            Patch::Set("Rollout path".to_owned())
        );
    }

    #[test]
    fn t_mu03_a02_main_never_uses_agent_path_title() {
        let mut root = state_thread("main");
        root.agent_path = Some("/root/main_path".to_owned());
        let mut main_rollout = rollout_with_agent_path(1, "main", "/root/rollout_main_path");
        main_rollout.agent_role_hint = Some(Candidate {
            value: "main".to_owned(),
            provenance: AgentRoleProvenance::SessionMetaRole,
            record_offset: 1,
        });
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(vec![root], Vec::new()),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: vec![main_rollout],
            source_file_observations: Vec::new(),
            existing_threads: vec![existing_main("main")],
            resolved_at_ms: 100,
        });
        assert_eq!(patch(&result, "main").title, Patch::Keep);
    }

    #[test]
    fn t_mu03_a02_unavailable_state_preserves_existing_title_but_allows_subagent_fallback() {
        let mut titled = rollout_with_agent_path(1, "titled", "/root/rollout_titled");
        titled.parent_thread_id_hint = Some(Candidate {
            value: "parent".to_owned(),
            provenance: ParentHintProvenance::SubagentSource,
            record_offset: 1,
        });
        let mut untitled = rollout_with_agent_path(2, "untitled", "/root/gate_b_rereview");
        untitled.parent_thread_id_hint = Some(Candidate {
            value: "parent".to_owned(),
            provenance: ParentHintProvenance::SubagentSource,
            record_offset: 1,
        });
        let mut existing_titled = existing("titled");
        existing_titled.title = Some("Existing title".to_owned());
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: StateSnapshot::unavailable(Vec::new()),
            session_name_snapshot: sessions(vec![SessionNameFact {
                thread_id: "titled".to_owned(),
                thread_name: "Lower priority".to_owned(),
                updated_at_ms: Some(30),
            }]),
            global_state_snapshot: global_state(),
            rollout_facts: vec![titled, untitled],
            source_file_observations: Vec::new(),
            existing_threads: vec![existing_titled, existing("untitled")],
            resolved_at_ms: 100,
        });
        assert!(
            result
                .patches
                .iter()
                .all(|patch| patch.thread_id != "titled")
        );
        assert_eq!(
            patch(&result, "untitled").title,
            Patch::Set("Gate b rereview".to_owned())
        );
    }

    #[test]
    fn two_level_subagent_chain_resolves_one_root() {
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(
                vec![
                    state_thread("root"),
                    state_thread("child"),
                    state_thread("grandchild"),
                ],
                vec![("root", "child"), ("child", "grandchild")],
            ),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: Vec::new(),
            source_file_observations: Vec::new(),
            existing_threads: vec![existing("root"), existing("child"), existing("grandchild")],
            resolved_at_ms: 100,
        });

        assert_eq!(
            patch(&result, "root").agent_role,
            Patch::Set(AgentRole::Main)
        );
        assert_eq!(
            patch(&result, "child").root_session_id,
            Patch::Set("root".to_owned())
        );
        assert_eq!(
            patch(&result, "grandchild").parent_thread_id,
            Patch::Set("child".to_owned())
        );
        assert_eq!(
            patch(&result, "grandchild").root_session_id,
            Patch::Set("root".to_owned())
        );
    }

    #[test]
    fn missing_parent_multi_parent_cycle_and_unknown_are_safe() {
        let mut explicit_unknown = rollout(10, "unknown");
        explicit_unknown.agent_role_hint = Some(Candidate {
            value: "subagent".to_owned(),
            provenance: AgentRoleProvenance::SessionMetaRole,
            record_offset: 1,
        });
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(
                vec![
                    state_thread("missing-child"),
                    state_thread("multi"),
                    state_thread("cycle-a"),
                    state_thread("cycle-b"),
                ],
                vec![
                    ("absent", "missing-child"),
                    ("parent-a", "multi"),
                    ("parent-b", "multi"),
                    ("cycle-a", "cycle-b"),
                    ("cycle-b", "cycle-a"),
                ],
            ),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: vec![explicit_unknown],
            source_file_observations: vec![source(
                10,
                "unknown",
                SourceArea::Sessions,
                &fixture_path("sessions/unknown.jsonl"),
            )],
            existing_threads: vec![
                existing("missing-child"),
                existing_main("multi"),
                existing_main("cycle-a"),
                existing_main("cycle-b"),
                existing_main("unknown"),
            ],
            resolved_at_ms: 100,
        });

        assert_eq!(patch(&result, "missing-child").agent_role, Patch::Keep);
        for id in ["multi", "cycle-a", "cycle-b", "unknown"] {
            assert_eq!(patch(&result, id).agent_role, Patch::Keep);
            assert_eq!(patch(&result, id).parent_thread_id, Patch::Keep);
            assert_eq!(patch(&result, id).root_session_id, Patch::Keep);
        }
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == MetadataDiagnosticCode::ParentMissing)
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == MetadataDiagnosticCode::ParentConflict)
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == MetadataDiagnosticCode::ParentCycle)
        );
    }

    #[test]
    fn unavailable_relationships_require_explicit_main_evidence() {
        let mut explicit_main = rollout(1, "explicit-main");
        explicit_main.agent_role_hint = Some(Candidate {
            value: "main".to_owned(),
            provenance: AgentRoleProvenance::SessionMetaRole,
            record_offset: 1,
        });
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: StateSnapshot::unavailable(Vec::new()),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: vec![explicit_main],
            source_file_observations: Vec::new(),
            existing_threads: vec![existing("explicit-main"), existing("no-evidence")],
            resolved_at_ms: 100,
        });

        assert_eq!(
            patch(&result, "explicit-main").agent_role,
            Patch::Set(AgentRole::Main)
        );
        assert_eq!(
            patch(&result, "explicit-main").root_session_id,
            Patch::Set("explicit-main".to_owned())
        );
        assert!(result.patches.iter().all(|patch| {
            patch.thread_id != "no-evidence"
                || (patch.agent_role != Patch::Set(AgentRole::Main)
                    && !matches!(patch.root_session_id, Patch::Set(_)))
        }));
    }

    #[test]
    fn confirmed_rollout_parent_updates_direct_relationship_when_root_is_unknown() {
        let mut child = rollout(1, "child");
        child.parent_thread_id_hint = Some(Candidate {
            value: "parent".to_owned(),
            provenance: ParentHintProvenance::SubagentSource,
            record_offset: 1,
        });
        child.agent_role_hint = Some(Candidate {
            value: "subagent".to_owned(),
            provenance: AgentRoleProvenance::SubagentSource,
            record_offset: 1,
        });

        let mut state_snapshot = state(
            vec![state_thread("parent"), state_thread("child")],
            Vec::new(),
        );
        state_snapshot.spawn_edges_status = StateSourceStatus::Unavailable;

        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot,
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: vec![child],
            source_file_observations: Vec::new(),
            existing_threads: vec![existing("parent"), existing("child")],
            resolved_at_ms: 100,
        });

        let child = patch(&result, "child");
        assert_eq!(child.parent_thread_id, Patch::Set("parent".to_owned()));
        assert_eq!(child.agent_role, Patch::Set(AgentRole::Subagent));
        assert_eq!(child.root_session_id, Patch::Keep);
        assert_eq!(
            child.metadata_quality_status,
            MetadataQualityStatus::Partial
        );
        assert!(result.patches.iter().all(|patch| {
            patch.thread_id != "parent"
                || (patch.agent_role != Patch::Set(AgentRole::Main)
                    && !matches!(patch.root_session_id, Patch::Set(_)))
        }));
    }

    #[test]
    fn a_late_parent_recomputes_the_child_root() {
        let mut child_fact = rollout(1, "child");
        child_fact.parent_thread_id_hint = Some(Candidate {
            value: "parent".to_owned(),
            provenance: ParentHintProvenance::SubagentSource,
            record_offset: 1,
        });
        let before = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(vec![state_thread("child")], Vec::new()),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: vec![child_fact.clone()],
            source_file_observations: Vec::new(),
            existing_threads: vec![existing("child")],
            resolved_at_ms: 100,
        });
        assert_eq!(patch(&before, "child").parent_thread_id, Patch::Keep);
        assert_eq!(patch(&before, "child").agent_role, Patch::Keep);
        assert_eq!(patch(&before, "child").root_session_id, Patch::Keep);

        let after = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(
                vec![state_thread("parent"), state_thread("child")],
                Vec::new(),
            ),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: vec![child_fact],
            source_file_observations: Vec::new(),
            existing_threads: vec![existing("parent"), existing("child")],
            resolved_at_ms: 101,
        });
        assert_eq!(
            patch(&after, "child").root_session_id,
            Patch::Set("parent".to_owned())
        );
    }

    #[test]
    fn state_edge_wins_over_rollout_hint_and_role_conflict_is_unknown() {
        let mut child = rollout(1, "child");
        child.parent_thread_id_hint = Some(Candidate {
            value: "rollout-parent".to_owned(),
            provenance: ParentHintProvenance::SubagentSource,
            record_offset: 1,
        });
        child.agent_role_hint = Some(Candidate {
            value: "main".to_owned(),
            provenance: AgentRoleProvenance::SessionMetaRole,
            record_offset: 2,
        });
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(
                vec![state_thread("state-parent"), state_thread("child")],
                vec![("state-parent", "child")],
            ),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: vec![child],
            source_file_observations: Vec::new(),
            existing_threads: vec![existing("state-parent"), existing("child")],
            resolved_at_ms: 100,
        });

        let child = patch(&result, "child");
        assert_eq!(child.parent_thread_id, Patch::Keep);
        assert_eq!(child.root_session_id, Patch::Keep);
        assert_eq!(child.agent_role, Patch::Keep);
        assert_eq!(
            child.metadata_quality_status,
            MetadataQualityStatus::Conflict
        );
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.thread_id.as_deref() == Some("child")
                && diagnostic.code == MetadataDiagnosticCode::RoleConflict
        }));
    }

    #[test]
    fn direct_rollout_parent_resolves_without_a_state_spawn_edge() {
        let mut child = rollout(1, "child");
        child.parent_thread_id_hint = Some(Candidate {
            value: "root".to_owned(),
            provenance: ParentHintProvenance::SessionMetaParent,
            record_offset: 1,
        });
        child.agent_role_hint = Some(Candidate {
            value: "subagent".to_owned(),
            provenance: AgentRoleProvenance::SubagentSource,
            record_offset: 1,
        });
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(
                vec![state_thread("root"), state_thread("child")],
                Vec::new(),
            ),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: vec![child],
            source_file_observations: vec![source(
                1,
                "child",
                SourceArea::Sessions,
                &fixture_path("sessions/guardian.jsonl"),
            )],
            existing_threads: vec![existing("root"), existing("child")],
            resolved_at_ms: 100,
        });

        assert_eq!(
            patch(&result, "child").parent_thread_id,
            Patch::Set("root".to_owned())
        );
        assert_eq!(
            patch(&result, "child").root_session_id,
            Patch::Set("root".to_owned())
        );
        assert_eq!(
            patch(&result, "child").agent_role,
            Patch::Set(AgentRole::Subagent)
        );
        assert_eq!(
            patch(&result, "root").agent_role,
            Patch::Set(AgentRole::Main)
        );
        assert!(!result.diagnostics.iter().any(|diagnostic| {
            diagnostic.thread_id.as_deref() == Some("child")
                && diagnostic.code == MetadataDiagnosticCode::ParentConflict
        }));
    }

    #[test]
    fn state_edge_and_direct_parent_same_value_are_consistent() {
        let mut child = rollout(1, "child");
        child.parent_thread_id_hint = Some(Candidate {
            value: "root".to_owned(),
            provenance: ParentHintProvenance::SessionMetaParent,
            record_offset: 1,
        });
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(
                vec![state_thread("root"), state_thread("child")],
                vec![("root", "child")],
            ),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: vec![child],
            source_file_observations: Vec::new(),
            existing_threads: vec![existing("root"), existing("child")],
            resolved_at_ms: 100,
        });
        assert_eq!(
            patch(&result, "child").parent_thread_id,
            Patch::Set("root".to_owned())
        );
        assert!(!result.diagnostics.iter().any(|diagnostic| {
            diagnostic.thread_id.as_deref() == Some("child")
                && diagnostic.code == MetadataDiagnosticCode::ParentConflict
        }));
    }

    #[test]
    fn state_edge_wins_over_conflicting_direct_parent_but_marks_conflict() {
        let mut child = rollout(1, "child");
        child.parent_thread_id_hint = Some(Candidate {
            value: "rollout-parent".to_owned(),
            provenance: ParentHintProvenance::SessionMetaParent,
            record_offset: 1,
        });
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(
                vec![
                    state_thread("state-parent"),
                    state_thread("rollout-parent"),
                    state_thread("child"),
                ],
                vec![("state-parent", "child")],
            ),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: vec![child],
            source_file_observations: Vec::new(),
            existing_threads: vec![
                existing("state-parent"),
                existing("rollout-parent"),
                existing("child"),
            ],
            resolved_at_ms: 100,
        });
        assert_eq!(patch(&result, "child").parent_thread_id, Patch::Keep);
        assert_eq!(patch(&result, "child").agent_role, Patch::Keep);
        assert_eq!(patch(&result, "child").root_session_id, Patch::Keep);
        assert_eq!(
            patch(&result, "child").metadata_quality_status,
            MetadataQualityStatus::Conflict
        );
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.thread_id.as_deref() == Some("child")
                && diagnostic.code == MetadataDiagnosticCode::ParentConflict
        }));
    }

    #[test]
    fn metadata_only_conflict_keeps_existing_relationship_trio() {
        let mut child = rollout(1, "child");
        child.parent_thread_id_hint = Some(Candidate {
            value: "root".to_owned(),
            provenance: ParentHintProvenance::SessionMetaParent,
            record_offset: 1,
        });
        child.agent_role_hint = Some(Candidate {
            value: "subagent".to_owned(),
            provenance: AgentRoleProvenance::SubagentSource,
            record_offset: 1,
        });
        child.latest_context_model = Some("gpt-5.6-sol".to_owned());
        child.latest_context_turn_id = Some("00000000-0000-7000-8000-000000000010".to_owned());
        child.latest_context_at_ms = Some(10);
        child.latest_context_record_offset = Some(2);
        child.has_conflict = true;

        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(
                vec![state_thread("root"), state_thread("child")],
                vec![("root", "child")],
            ),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: vec![child],
            source_file_observations: Vec::new(),
            existing_threads: vec![existing_main("root"), {
                let mut existing = existing("child");
                existing.parent_thread_id = Some("root".to_owned());
                existing.root_session_id = Some("root".to_owned());
                existing.agent_role = AgentRole::Subagent;
                existing
            }],
            resolved_at_ms: 100,
        });

        let patch = patch(&result, "child");
        assert_eq!(patch.parent_thread_id, Patch::Keep);
        assert_eq!(patch.agent_role, Patch::Keep);
        assert_eq!(patch.root_session_id, Patch::Keep);
        assert_eq!(
            patch.metadata_quality_status,
            MetadataQualityStatus::Conflict
        );
    }

    #[test]
    fn a_self_loop_is_conflict_and_never_produces_a_root() {
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(vec![state_thread("self")], vec![("self", "self")]),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: Vec::new(),
            source_file_observations: Vec::new(),
            existing_threads: vec![existing_main("self")],
            resolved_at_ms: 100,
        });

        let self_patch = patch(&result, "self");
        assert_eq!(self_patch.agent_role, Patch::Keep);
        assert_eq!(self_patch.parent_thread_id, Patch::Keep);
        assert_eq!(self_patch.root_session_id, Patch::Keep);
        assert_eq!(
            self_patch.metadata_quality_status,
            MetadataQualityStatus::Conflict
        );
    }

    #[test]
    fn state_title_unavailable_keep_and_unchanged_output_are_deterministic() {
        let mut titled = state_thread("titled");
        titled.title = Some("State Title".to_owned());
        let title_result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(vec![titled], Vec::new()),
            session_name_snapshot: sessions(vec![SessionNameFact {
                thread_id: "titled".to_owned(),
                thread_name: "Session Title".to_owned(),
                updated_at_ms: Some(30),
            }]),
            global_state_snapshot: global_state(),
            rollout_facts: Vec::new(),
            source_file_observations: Vec::new(),
            existing_threads: vec![existing("titled")],
            resolved_at_ms: 100,
        });
        assert_eq!(
            patch(&title_result, "titled").title,
            Patch::Set("State Title".to_owned())
        );

        let mut stable = existing("stable");
        stable.title = Some("Stable Title".to_owned());
        let unavailable = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: StateSnapshot::unavailable(Vec::new()),
            session_name_snapshot: sessions(vec![SessionNameFact {
                thread_id: "stable".to_owned(),
                thread_name: "Lower Priority".to_owned(),
                updated_at_ms: Some(30),
            }]),
            global_state_snapshot: global_state(),
            rollout_facts: Vec::new(),
            source_file_observations: Vec::new(),
            existing_threads: vec![stable],
            resolved_at_ms: 101,
        });
        assert!(unavailable.patches.iter().all(|patch| {
            patch.thread_id != "stable"
                || matches!(patch.title, Patch::Keep) && !matches!(patch.title, Patch::Clear)
        }));

        let mut unchanged = existing_main("unchanged");
        unchanged.root_session_id = Some("unchanged".to_owned());
        unchanged.created_at_ms = Some(10);
        unchanged.updated_at_ms = Some(20);
        let unchanged_result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(vec![state_thread("unchanged")], Vec::new()),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: Vec::new(),
            source_file_observations: Vec::new(),
            existing_threads: vec![unchanged],
            resolved_at_ms: 102,
        });
        assert!(unchanged_result.patches.is_empty());
    }

    #[test]
    fn newer_and_provenance_ranked_facts_are_stable_conflicting_and_private() {
        const SENTINEL: &str = "PROMPT_TOOL_BODY_PRIVATE_SENTINEL";
        let mut older = state_thread("merged");
        older.updated_at_ms = Some(10);
        older.title = Some("Old State Title".to_owned());
        let mut newer = state_thread("merged");
        newer.updated_at_ms = Some(50);
        newer.title = Some("New State Title".to_owned());

        let mut selected = rollout(1, "merged");
        selected.cwd = Some(Candidate {
            value: fixture_path("chosen/project"),
            provenance: CwdProvenance::SessionMeta,
            record_offset: 2,
        });
        selected.parent_thread_id_hint = Some(Candidate {
            value: "direct-parent".to_owned(),
            provenance: ParentHintProvenance::SubagentSource,
            record_offset: 3,
        });
        selected.agent_role_hint = Some(Candidate {
            value: "subagent".to_owned(),
            provenance: AgentRoleProvenance::SubagentSource,
            record_offset: 4,
        });
        let safe = selected
            .to_safe_fact(
                1,
                1,
                20,
                100,
                &crate::codex::rollout::FinalContinuation::OwningLive {
                    owning_thread_id: "merged".to_owned(),
                },
            )
            .unwrap();
        assert!(!format!("{safe:?}").contains(SENTINEL));
        let selected = RolloutThreadFact::from_safe_fact(&safe).unwrap();
        assert_eq!(
            selected.cwd.as_ref().unwrap().provenance,
            CwdProvenance::SessionMeta
        );
        assert_eq!(selected.cwd.as_ref().unwrap().record_offset, 2);
        assert_eq!(
            selected.parent_thread_id_hint.as_ref().unwrap().provenance,
            ParentHintProvenance::SubagentSource
        );
        assert_eq!(
            selected.agent_role_hint.as_ref().unwrap().provenance,
            AgentRoleProvenance::SubagentSource
        );

        let mut same_priority = rollout(2, "merged");
        same_priority.cwd = Some(Candidate {
            value: fixture_path("other/project"),
            provenance: CwdProvenance::SessionMeta,
            record_offset: 8,
        });
        same_priority.parent_thread_id_hint = Some(Candidate {
            value: "fallback-parent".to_owned(),
            provenance: ParentHintProvenance::ForkedFromId,
            record_offset: 1,
        });
        same_priority.agent_role_hint = Some(Candidate {
            value: "main".to_owned(),
            provenance: AgentRoleProvenance::SessionMetaRole,
            record_offset: 1,
        });
        let mut lower_priority = rollout(3, "merged");
        lower_priority.cwd = Some(Candidate {
            value: format!("/{SENTINEL}"),
            provenance: CwdProvenance::TurnContext,
            record_offset: 1,
        });

        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(vec![older, newer], Vec::new()),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: global_state(),
            rollout_facts: vec![lower_priority, same_priority, selected],
            source_file_observations: vec![
                source(
                    1,
                    "merged",
                    SourceArea::Sessions,
                    &fixture_path("sessions/one.jsonl"),
                ),
                source(
                    2,
                    "merged",
                    SourceArea::Sessions,
                    &fixture_path("sessions/two.jsonl"),
                ),
                source(
                    3,
                    "merged",
                    SourceArea::Sessions,
                    &fixture_path("sessions/three.jsonl"),
                ),
            ],
            existing_threads: vec![existing("merged")],
            resolved_at_ms: 101,
        });

        assert_eq!(result.patches.len(), 1);
        let merged = patch(&result, "merged");
        assert_eq!(merged.title, Patch::Set("New State Title".to_owned()));
        assert_eq!(
            merged.project_path,
            Patch::Set(fixture_path("chosen/project"))
        );
        assert_eq!(merged.parent_thread_id, Patch::Keep);
        assert_eq!(merged.agent_role, Patch::Keep);
        assert_eq!(merged.root_session_id, Patch::Keep);
        assert_eq!(
            merged.metadata_quality_status,
            MetadataQualityStatus::Conflict
        );
        assert!(result.diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.code,
                MetadataDiagnosticCode::FieldConflict | MetadataDiagnosticCode::RoleConflict
            )
        }));
        assert!(!format!("{result:?}").contains(SENTINEL));
    }

    #[test]
    fn t_s03_001_project_kind_resolution_matrix_uses_only_global_state_identity() {
        let mut project = state_thread("project");
        project.cwd = Some(fixture_path("workspace/project"));
        let mut projectless = state_thread("projectless");
        projectless.cwd = Some(fixture_path("generated/workspace/projectless"));
        let mut projectless_only = state_thread("projectless-only");
        projectless_only.cwd = Some(fixture_path("generated/workspace/projectless-only"));
        let mut heuristic = state_thread("heuristic");
        heuristic.cwd = Some(fixture_path(
            "Users/example/Documents/Codex/generated-workspace",
        ));
        let mut root = state_thread("root");
        root.cwd = Some(fixture_path("workspace/root"));
        let mut child = state_thread("child");
        child.cwd = Some(fixture_path("generated/workspace/child"));
        let mut existing_conflict = existing("projectless");
        existing_conflict.project_kind = ProjectKind::Project;
        let mut existing_no_path = existing("no-path");
        existing_no_path.project_kind = ProjectKind::Project;
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(
                vec![
                    project,
                    projectless,
                    projectless_only,
                    state_thread("no-path"),
                    heuristic,
                    root,
                    child,
                ],
                vec![("root", "child")],
            ),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: GlobalStateSnapshot {
                status: GlobalStateStatus::Complete,
                projectless_thread_ids: vec![
                    "projectless".to_owned(),
                    "projectless-only".to_owned(),
                ],
                thread_project_assignments: BTreeSet::from(["projectless".to_owned()]),
                diagnostics: Vec::new(),
            },
            rollout_facts: Vec::new(),
            source_file_observations: Vec::new(),
            existing_threads: vec![
                existing("project"),
                existing_conflict,
                existing("projectless-only"),
                existing_no_path,
                existing("heuristic"),
                existing("root"),
                existing("child"),
            ],
            resolved_at_ms: 100,
        });

        assert_eq!(
            patch(&result, "project").project_kind,
            Patch::Set(ProjectKind::Project)
        );
        assert_eq!(
            patch(&result, "projectless").project_kind,
            Patch::Set(ProjectKind::Unknown)
        );
        assert_eq!(
            patch(&result, "projectless").project_path,
            Patch::Set(fixture_path("generated/workspace/projectless"))
        );
        assert_eq!(
            patch(&result, "projectless-only").project_kind,
            Patch::Set(ProjectKind::Projectless)
        );
        assert_eq!(
            patch(&result, "projectless-only").project_path,
            Patch::Set(fixture_path("generated/workspace/projectless-only"))
        );
        assert_eq!(
            patch(&result, "projectless-only").project_name,
            Patch::Set("projectless-only".to_owned())
        );
        assert_eq!(
            patch(&result, "no-path").project_kind,
            Patch::Set(ProjectKind::Unknown)
        );
        assert_eq!(
            patch(&result, "heuristic").project_kind,
            Patch::Set(ProjectKind::Project)
        );
        assert_eq!(
            patch(&result, "root").project_kind,
            Patch::Set(ProjectKind::Project)
        );
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.thread_id.as_deref() == Some("projectless")
                && diagnostic.code == MetadataDiagnosticCode::ProjectAssignmentConflict
                && diagnostic.severity == MetadataDiagnosticSeverity::Conflict
                && diagnostic.source_kind == MetadataSourceKind::GlobalState
        }));
        assert_eq!(
            patch(&result, "child").project_kind,
            Patch::Set(ProjectKind::Project)
        );
    }

    #[test]
    fn t_s03_002_global_state_unavailable_preserves_existing_and_recovers_idempotently() {
        let mut existing_projectless = existing("existing-projectless");
        existing_projectless.project_kind = ProjectKind::Projectless;
        existing_projectless.project_path = Some(fixture_path("generated/existing"));
        existing_projectless.project_name = Some("existing".to_owned());
        let mut existing_project = existing("existing-project");
        existing_project.project_kind = ProjectKind::Project;
        existing_project.project_path = Some(fixture_path("workspace/existing"));

        let malformed = GlobalStateSnapshot::unavailable(GlobalStateStatus::Malformed, Vec::new());
        let unavailable = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(
                vec![
                    {
                        let mut fact = state_thread("existing-projectless");
                        fact.cwd = Some(fixture_path("generated/existing"));
                        fact
                    },
                    {
                        let mut fact = state_thread("existing-project");
                        fact.cwd = Some(fixture_path("workspace/existing"));
                        fact
                    },
                    {
                        let mut fact = state_thread("new-thread");
                        fact.cwd = Some(fixture_path("workspace/new"));
                        fact
                    },
                ],
                Vec::new(),
            ),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: malformed,
            rollout_facts: Vec::new(),
            source_file_observations: Vec::new(),
            existing_threads: vec![existing_projectless.clone(), existing_project],
            resolved_at_ms: 100,
        });
        assert_eq!(
            patch(&unavailable, "existing-projectless").project_kind,
            Patch::Keep
        );
        assert_eq!(
            patch(&unavailable, "existing-projectless").project_path,
            Patch::Keep
        );
        assert_eq!(
            patch(&unavailable, "new-thread").project_kind,
            Patch::Set(ProjectKind::Unknown)
        );

        let complete = GlobalStateSnapshot {
            status: GlobalStateStatus::Complete,
            projectless_thread_ids: vec!["existing-project".to_owned()],
            thread_project_assignments: BTreeSet::new(),
            diagnostics: Vec::new(),
        };
        let recovered_input = ResolutionInput {
            state_snapshot: state(
                vec![
                    {
                        let mut fact = state_thread("existing-projectless");
                        fact.cwd = Some(fixture_path("generated/existing"));
                        fact
                    },
                    {
                        let mut fact = state_thread("existing-project");
                        fact.cwd = Some(fixture_path("workspace/existing"));
                        fact
                    },
                ],
                Vec::new(),
            ),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: complete.clone(),
            rollout_facts: Vec::new(),
            source_file_observations: Vec::new(),
            existing_threads: vec![existing_projectless, existing("existing-project")],
            resolved_at_ms: 101,
        };
        let recovered = ThreadMetadataResolver::resolve(recovered_input.clone());
        assert_eq!(
            patch(&recovered, "existing-project").project_kind,
            Patch::Set(ProjectKind::Projectless)
        );
        assert_eq!(
            patch(&recovered, "existing-projectless").project_kind,
            Patch::Set(ProjectKind::Project)
        );
        assert_eq!(recovered, ThreadMetadataResolver::resolve(recovered_input));

        let not_present = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: state(
                vec![
                    {
                        let mut fact = state_thread("path-without-global-state");
                        fact.cwd = Some(fixture_path("workspace/path"));
                        fact
                    },
                    state_thread("no-path-without-global-state"),
                ],
                Vec::new(),
            ),
            session_name_snapshot: sessions(Vec::new()),
            global_state_snapshot: GlobalStateSnapshot::unavailable(
                GlobalStateStatus::NotPresent,
                Vec::new(),
            ),
            rollout_facts: Vec::new(),
            source_file_observations: Vec::new(),
            existing_threads: vec![existing("path-without-global-state"), {
                let mut thread = existing("no-path-without-global-state");
                thread.project_kind = ProjectKind::Project;
                thread
            }],
            resolved_at_ms: 100,
        });
        assert_eq!(
            patch(&not_present, "path-without-global-state").project_kind,
            Patch::Set(ProjectKind::Project)
        );
        assert_eq!(
            patch(&not_present, "no-path-without-global-state").project_kind,
            Patch::Set(ProjectKind::Unknown)
        );
    }
}
