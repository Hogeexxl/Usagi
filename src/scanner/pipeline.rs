//! Metadata scan planning and Thread-group assembly.
//!
//! This module intentionally stops at the parser/reader seam. It never opens
//! rollout bytes. Discovery and the storage ledger provide observations and
//! matching safe facts; a chunk reader/parser supplies `ParsedSource` values;
//! this module turns those inputs into deterministic plans and atomic metadata
//! commit groups.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    path::PathBuf,
};

use crate::{
    codex::{
        ExistingThread, FinalContinuation, GlobalStateSnapshot, ResolutionInput, ResolutionResult,
        ResumeState, RolloutThreadFact, SessionNameSnapshot, StateSnapshot, ThreadMetadataResolver,
    },
    domain::{
        CheckpointProcessingStatus, ConsumerKind, MetadataCheckpointAdvance, MetadataCommitBatch,
        MetadataScanState, MetadataScanStateEntry, MetadataSourceCommit, MetadataThreadCommit,
        SafeFactState, SourceFileState, SourceObservation, SourceObservationBatch, SourceOutcome,
        SourceRegionStatus,
    },
    storage::{Ledger, StorageError, source::UsageCarryObservationProof},
};

use super::discovery::{DiscoveredFile, DiscoverySnapshot};

/// Why a source cannot safely reuse its current metadata checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Ord, PartialOrd)]
#[expect(
    dead_code,
    reason = "preserve public rebuild reason codes used by downstream callers"
)]
pub enum RebuildReason {
    NewSource,
    PendingCheckpoint,
    GenerationChanged,
    ParserVersionChanged,
    CheckpointOutOfRange,
    GuardMissing,
    GuardMismatch,
    SafeFactMissing,
    SafeFactStale,
    BindingConflict,
    ContinuationUnstable,
    CheckpointErrorUnverified,
}

impl RebuildReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NewSource => "NEW_SOURCE",
            Self::PendingCheckpoint => "CHECKPOINT_PENDING",
            Self::GenerationChanged => "GENERATION_CHANGED",
            Self::ParserVersionChanged => "PARSER_VERSION_CHANGED",
            Self::CheckpointOutOfRange => "CHECKPOINT_OUT_OF_RANGE",
            Self::GuardMissing => "CHECKPOINT_GUARD_MISSING",
            Self::GuardMismatch => "CHECKPOINT_GUARD_MISMATCH",
            Self::SafeFactMissing => "SAFE_FACT_MISSING",
            Self::SafeFactStale => "SAFE_FACT_STALE",
            Self::BindingConflict => "SOURCE_IDENTITY_CONFLICT",
            Self::ContinuationUnstable => "METADATA_CONTINUATION_UNSTABLE",
            Self::CheckpointErrorUnverified => "CHECKPOINT_ERROR_UNVERIFIED",
        }
    }
}

/// One metadata action for one discovered source.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "preserve the established public plan ownership shape"
)]
pub enum FilePlan {
    Skip {
        source_file_id: i64,
        observed_size: i64,
        fact: crate::domain::RolloutMetadataFact,
    },
    ReadFrom {
        source_file_id: i64,
        start_offset: u64,
        observed_size: i64,
        resume_state: ResumeState,
    },
    Rebuild {
        source_file_id: i64,
        observed_size: i64,
        reason: RebuildReason,
    },
    Reject {
        path: PathBuf,
        error_code: &'static str,
    },
}

impl FilePlan {
    pub fn source_file_id(&self) -> Option<i64> {
        match self {
            Self::Skip { source_file_id, .. }
            | Self::ReadFrom { source_file_id, .. }
            | Self::Rebuild { source_file_id, .. } => Some(*source_file_id),
            Self::Reject { .. } => None,
        }
    }
}

/// Plan-time diagnostic. It contains only source identifiers and fixed codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineDiagnostic {
    pub code: &'static str,
    pub source_file_id: Option<i64>,
    pub path: Option<PathBuf>,
    pub thread_id: Option<String>,
}

/// The plans for one discovery/observation/state view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelinePlan {
    pub plans: Vec<FilePlan>,
    pub diagnostics: Vec<PipelineDiagnostic>,
    pub sessions: SourceRegionStatus,
    pub archived_sessions: SourceRegionStatus,
}

impl PipelinePlan {
    pub fn plan_for(&self, source_file_id: i64) -> Option<&FilePlan> {
        self.plans
            .iter()
            .find(|plan| plan.source_file_id() == Some(source_file_id))
    }
}

/// A parser/reader result supplied to the pipeline. The reader owns byte
/// framing and guard calculation; this type carries only safe parser output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSource {
    pub source_file_id: i64,
    pub fact: Option<RolloutThreadFact>,
    pub final_continuation: FinalContinuation,
    pub last_processed_offset: u64,
    pub guard_hash: Option<Vec<u8>>,
    pub needs_rebuild: bool,
    pub bytes_read: u64,
    pub guard_bytes_read: u64,
    pub peak_buffered_body_bytes: u64,
    pub complete_line_count: u64,
    pub oversized_complete_line_count: u64,
    pub malformed_record_count: u64,
    pub diagnostic_count: u64,
    pub has_half_line: bool,
}

impl ParsedSource {
    pub fn stable(&self) -> bool {
        !self.needs_rebuild
            && matches!(
                self.final_continuation,
                FinalContinuation::ReplayedAncestor { .. } | FinalContinuation::OwningLive { .. }
            )
            && self.fact.is_some()
    }
}

/// Completeness of one owning Thread group. Incomplete groups are never passed
/// to `Ledger::commit_metadata`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadGroupCompleteness {
    Complete,
    Incomplete { reasons: Vec<&'static str> },
}

impl ThreadGroupCompleteness {
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadGroupPlan {
    pub thread_id: String,
    pub source_file_ids: Vec<i64>,
    pub completeness: ThreadGroupCompleteness,
}

/// Input to one full resolver pass. Snapshots are supplied by the caller so
/// state/session are read exactly once per scan and rollout parsing remains a
/// separate explicit seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineResolutionInput {
    pub state_snapshot: StateSnapshot,
    pub session_name_snapshot: SessionNameSnapshot,
    pub global_state_snapshot: GlobalStateSnapshot,
    pub scan_state: MetadataScanState,
    pub plans: PipelinePlan,
    pub parsed_sources: Vec<ParsedSource>,
    pub existing_threads: Vec<ExistingThread>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineResolution {
    pub groups: Vec<ThreadGroupPlan>,
    pub resolution: ResolutionResult,
    pub commit_batch: Option<MetadataCommitBatch>,
    pub diagnostics: Vec<PipelineDiagnostic>,
}

#[derive(Debug)]
pub enum PipelineError {
    Invalid(String),
    Storage(StorageError),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => {
                write!(formatter, "invalid metadata pipeline input: {message}")
            }
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PipelineError {}

impl From<StorageError> for PipelineError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

/// Metadata pipeline configuration and pure planning/assembly operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadataPipeline {
    pub metadata_parser_version: i64,
    pub resolved_at_ms: i64,
}

impl MetadataPipeline {
    pub fn new(metadata_parser_version: i64, resolved_at_ms: i64) -> Result<Self, PipelineError> {
        if metadata_parser_version < 0 || resolved_at_ms < 0 {
            return Err(PipelineError::Invalid(
                "parser version and resolved time must be non-negative".to_owned(),
            ));
        }
        Ok(Self {
            metadata_parser_version,
            resolved_at_ms,
        })
    }

    /// Convert the discovery files to the Spec 01 observation batch. No source
    /// row is inferred here; the ledger assigns IDs/generation in one write.
    pub fn observation_batch(
        snapshot: &DiscoverySnapshot,
        observed_at_ms: i64,
    ) -> Result<SourceObservationBatch, PipelineError> {
        let observations = snapshot
            .files
            .iter()
            .map(|file| {
                SourceObservation::new(
                    file.path.to_str().ok_or_else(|| {
                        PipelineError::Invalid("discovery path is not valid UTF-8".to_owned())
                    })?,
                    file.source_area,
                    file.device_id,
                    file.inode,
                    file.size,
                    file.mtime_ns,
                    observed_at_ms,
                )
                .map_err(|error| PipelineError::Invalid(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        SourceObservationBatch::new(
            observations,
            snapshot.sessions.clone(),
            snapshot.archived_sessions.clone(),
        )
        .map_err(|error| PipelineError::Invalid(error.to_string()))
    }

    /// Record this snapshot and immediately load the same source/checkpoint/
    /// safe-fact view for planning. This is the only storage call in the
    /// planning stage; no rollout reader is opened here.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "retain the established public planning seam used by downstream callers"
        )
    )]
    pub fn record_and_load(
        &self,
        ledger: &Ledger,
        snapshot: &DiscoverySnapshot,
    ) -> Result<(SourceOutcome, MetadataScanState), PipelineError> {
        self.record_and_load_with_usage_carry_proofs(ledger, snapshot, &[])
    }

    pub(crate) fn record_and_load_with_usage_carry_proofs(
        &self,
        ledger: &Ledger,
        snapshot: &DiscoverySnapshot,
        usage_carry_proofs: &[UsageCarryObservationProof],
    ) -> Result<(SourceOutcome, MetadataScanState), PipelineError> {
        let batch = Self::observation_batch(snapshot, self.resolved_at_ms)?;
        let outcome =
            ledger.record_source_observations_with_usage_carry_proofs(batch, usage_carry_proofs)?;
        let ids = outcome
            .results
            .iter()
            .map(|result| result.source_file_id)
            .collect::<Vec<_>>();
        let state = ledger.load_metadata_scan_state(ids)?;
        Ok((outcome, state))
    }

    /// Generate Skip/ReadFrom/Rebuild decisions from one observation result and
    /// one matching ledger state. The source IDs in `outcome.results` must be
    /// in the same order as `snapshot.files`, as guaranteed by storage's
    /// observation contract.
    pub fn plan_files(
        &self,
        snapshot: &DiscoverySnapshot,
        outcome: &SourceOutcome,
        scan_state: &MetadataScanState,
    ) -> PipelinePlan {
        let mut plans = Vec::with_capacity(snapshot.files.len());
        let mut diagnostics = Vec::new();
        if snapshot.files.len() != outcome.results.len() {
            diagnostics.push(PipelineDiagnostic {
                code: "SOURCE_OBSERVATION_CARDINALITY_MISMATCH",
                source_file_id: None,
                path: None,
                thread_id: None,
            });
            for file in &snapshot.files {
                plans.push(FilePlan::Reject {
                    path: file.path.clone(),
                    error_code: "SOURCE_OBSERVATION_CARDINALITY_MISMATCH",
                });
            }
            return PipelinePlan {
                plans,
                diagnostics,
                sessions: snapshot.sessions.clone(),
                archived_sessions: snapshot.archived_sessions.clone(),
            };
        }

        for (file, result) in snapshot.files.iter().zip(&outcome.results) {
            let Some(entry) = scan_state.get(result.source_file_id) else {
                plans.push(FilePlan::Reject {
                    path: file.path.clone(),
                    error_code: "SOURCE_STATE_UNAVAILABLE",
                });
                diagnostics.push(PipelineDiagnostic {
                    code: "SOURCE_STATE_UNAVAILABLE",
                    source_file_id: Some(result.source_file_id),
                    path: Some(file.path.clone()),
                    thread_id: None,
                });
                continue;
            };
            plans.push(self.plan_one(file, result, entry, &mut diagnostics));
        }
        PipelinePlan {
            plans,
            diagnostics,
            sessions: snapshot.sessions.clone(),
            archived_sessions: snapshot.archived_sessions.clone(),
        }
    }

    fn plan_one(
        &self,
        file: &DiscoveredFile,
        outcome: &crate::domain::SourceObservationResult,
        entry: &MetadataScanStateEntry,
        diagnostics: &mut Vec<PipelineDiagnostic>,
    ) -> FilePlan {
        let source_id = outcome.source_file_id;
        let observed_size = entry.source.observed_size;
        if outcome.generation_changed()
            || outcome.rebuild_consumers.contains(&ConsumerKind::Metadata)
        {
            return FilePlan::Rebuild {
                source_file_id: source_id,
                observed_size,
                reason: RebuildReason::GenerationChanged,
            };
        }
        let Some(checkpoint) = entry.metadata_checkpoint.as_ref() else {
            return FilePlan::ReadFrom {
                source_file_id: source_id,
                start_offset: 0,
                observed_size,
                resume_state: ResumeState::AwaitOwningMeta,
            };
        };
        if checkpoint.parser_version != self.metadata_parser_version {
            return FilePlan::Rebuild {
                source_file_id: source_id,
                observed_size,
                reason: RebuildReason::ParserVersionChanged,
            };
        }
        let Ok(offset) = u64::try_from(checkpoint.committed_offset) else {
            return FilePlan::Rebuild {
                source_file_id: source_id,
                observed_size,
                reason: RebuildReason::CheckpointOutOfRange,
            };
        };
        if checkpoint.committed_offset > entry.source.observed_size {
            return FilePlan::Rebuild {
                source_file_id: source_id,
                observed_size,
                reason: RebuildReason::CheckpointOutOfRange,
            };
        }
        if (offset == 0) != checkpoint.guard_hash.is_none() {
            return FilePlan::Rebuild {
                source_file_id: source_id,
                observed_size,
                reason: RebuildReason::GuardMissing,
            };
        }
        let stable_fact = matching_stable_fact(entry);
        let resume_state = stable_fact.and_then(|fact| {
            let thread_id = entry
                .source
                .thread_id
                .as_ref()
                .filter(|thread_id| *thread_id == &fact.owning_thread_id)?;
            match fact.continuation_state {
                crate::domain::ContinuationState::ReplayedAncestor => {
                    Some(ResumeState::ReplayedAncestor {
                        owning_thread_id: thread_id.clone(),
                    })
                }
                crate::domain::ContinuationState::OwningLive => Some(ResumeState::OwningLive {
                    owning_thread_id: thread_id.clone(),
                }),
                crate::domain::ContinuationState::Unstable => None,
            }
        });
        match checkpoint.processing_status {
            CheckpointProcessingStatus::Pending if offset == 0 => FilePlan::ReadFrom {
                source_file_id: source_id,
                start_offset: 0,
                observed_size,
                resume_state: ResumeState::AwaitOwningMeta,
            },
            CheckpointProcessingStatus::Pending => FilePlan::Rebuild {
                source_file_id: source_id,
                observed_size,
                reason: RebuildReason::PendingCheckpoint,
            },
            CheckpointProcessingStatus::RebuildRequired => FilePlan::Rebuild {
                source_file_id: source_id,
                observed_size,
                reason: RebuildReason::GenerationChanged,
            },
            CheckpointProcessingStatus::Error => {
                if offset > 0 {
                    if let Some(resume_state) = resume_state {
                        FilePlan::ReadFrom {
                            source_file_id: source_id,
                            start_offset: offset,
                            observed_size,
                            resume_state,
                        }
                    } else {
                        FilePlan::Rebuild {
                            source_file_id: source_id,
                            observed_size,
                            reason: RebuildReason::CheckpointErrorUnverified,
                        }
                    }
                } else {
                    FilePlan::Rebuild {
                        source_file_id: source_id,
                        observed_size,
                        reason: RebuildReason::CheckpointErrorUnverified,
                    }
                }
            }
            CheckpointProcessingStatus::Ready => {
                if offset == 0 {
                    return FilePlan::ReadFrom {
                        source_file_id: source_id,
                        start_offset: 0,
                        observed_size,
                        resume_state: ResumeState::AwaitOwningMeta,
                    };
                }
                let Some(resume_state) = resume_state else {
                    let reason = match &entry.safe_fact {
                        SafeFactState::None => RebuildReason::SafeFactMissing,
                        SafeFactState::Stale(_) => RebuildReason::SafeFactStale,
                        SafeFactState::Matching(_) => RebuildReason::ContinuationUnstable,
                    };
                    diagnostics.push(PipelineDiagnostic {
                        code: reason.as_str(),
                        source_file_id: Some(source_id),
                        path: Some(file.path.clone()),
                        thread_id: entry.source.thread_id.clone(),
                    });
                    return FilePlan::Rebuild {
                        source_file_id: source_id,
                        observed_size,
                        reason,
                    };
                };
                if checkpoint.committed_offset == entry.source.observed_size {
                    let SafeFactState::Matching(fact) = &entry.safe_fact else {
                        unreachable!("stable resume state requires matching safe fact")
                    };
                    FilePlan::Skip {
                        source_file_id: source_id,
                        observed_size,
                        fact: (*fact).clone(),
                    }
                } else {
                    FilePlan::ReadFrom {
                        source_file_id: source_id,
                        start_offset: offset,
                        observed_size,
                        resume_state,
                    }
                }
            }
        }
    }

    /// Resolve all source facts once, then build only complete Thread groups as
    /// storage commit groups. Incomplete groups remain visible in the result
    /// but are absent from `commit_batch`.
    pub fn resolve(
        &self,
        input: PipelineResolutionInput,
    ) -> Result<PipelineResolution, PipelineError> {
        let sessions_complete = input.plans.sessions.is_complete();
        let archived_sessions_complete = input.plans.archived_sessions.is_complete();
        let mut diagnostics = input.plans.diagnostics.clone();
        let mut source_entries = BTreeMap::new();
        for entry in &input.scan_state.entries {
            source_entries.insert(entry.source.source_file_id, entry.clone());
        }
        let parsed_by_source = input
            .parsed_sources
            .iter()
            .map(|parsed| (parsed.source_file_id, parsed))
            .collect::<HashMap<_, _>>();

        let mut rollout_facts = Vec::new();
        let mut owners = BTreeMap::<i64, String>::new();
        let mut source_complete = BTreeMap::<i64, bool>::new();
        let mut source_reasons = BTreeMap::<i64, BTreeSet<&'static str>>::new();

        for plan in &input.plans.plans {
            let Some(source_id) = plan.source_file_id() else {
                continue;
            };
            let Some(entry) = source_entries.get(&source_id) else {
                source_complete.insert(source_id, false);
                source_reasons
                    .entry(source_id)
                    .or_default()
                    .insert("SOURCE_STATE_UNAVAILABLE");
                continue;
            };
            let (fact, stable) = match plan {
                FilePlan::Skip { fact, .. } => match RolloutThreadFact::from_safe_fact(fact) {
                    Ok(fact) => (Some(fact), true),
                    Err(_) => (None, false),
                },
                FilePlan::ReadFrom { .. } | FilePlan::Rebuild { .. } => {
                    match parsed_by_source.get(&source_id) {
                        Some(parsed) => (parsed.fact.clone(), parsed.stable()),
                        None => (None, false),
                    }
                }
                FilePlan::Reject { .. } => (None, false),
            };
            let Some(fact) = fact else {
                source_complete.insert(source_id, false);
                source_reasons
                    .entry(source_id)
                    .or_default()
                    .insert("METADATA_FACT_UNAVAILABLE");
                continue;
            };
            if !stable {
                source_complete.insert(source_id, false);
                source_reasons
                    .entry(source_id)
                    .or_default()
                    .insert("METADATA_CONTINUATION_UNSTABLE");
            } else {
                source_complete.insert(source_id, true);
            }
            if let Some(existing) = entry.source.thread_id.as_ref()
                && existing != &fact.owning_thread_id
            {
                source_complete.insert(source_id, false);
                source_reasons
                    .entry(source_id)
                    .or_default()
                    .insert("SOURCE_IDENTITY_CONFLICT");
            }
            owners.insert(source_id, fact.owning_thread_id.clone());
            rollout_facts.push(fact);
        }

        let source_view = input
            .scan_state
            .entries
            .iter()
            .map(|entry| entry.source.clone())
            .collect::<Vec<_>>();
        let resolution = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: input.state_snapshot,
            session_name_snapshot: input.session_name_snapshot,
            global_state_snapshot: input.global_state_snapshot,
            rollout_facts: rollout_facts.clone(),
            source_file_observations: source_view.clone(),
            existing_threads: input.existing_threads,
            resolved_at_ms: self.resolved_at_ms,
        });

        let thread_ids = collect_thread_ids(&resolution, &rollout_facts, &source_view);
        let mut groups = Vec::new();
        for thread_id in thread_ids {
            let source_file_ids = source_view
                .iter()
                .filter(|source| source.file_status == crate::domain::FileStatus::Present)
                .filter(|source| {
                    source
                        .thread_id
                        .as_deref()
                        .or_else(|| owners.get(&source.source_file_id).map(String::as_str))
                        == Some(thread_id.as_str())
                })
                .map(|source| source.source_file_id)
                .collect::<Vec<_>>();
            let mut reasons = BTreeSet::new();
            if !sessions_complete {
                reasons.insert("SESSIONS_SOURCE_INCOMPLETE");
            }
            if !archived_sessions_complete {
                reasons.insert("ARCHIVED_SESSIONS_SOURCE_INCOMPLETE");
            }
            for source_id in &source_file_ids {
                if !source_complete.get(source_id).copied().unwrap_or(false) {
                    reasons.extend(source_reasons.get(source_id).into_iter().flatten().copied());
                }
                if !owners.contains_key(source_id) {
                    reasons.insert("OWNING_THREAD_UNCONFIRMED");
                }
            }
            let completeness = if reasons.is_empty() {
                ThreadGroupCompleteness::Complete
            } else {
                ThreadGroupCompleteness::Incomplete {
                    reasons: reasons.into_iter().collect(),
                }
            };
            groups.push(ThreadGroupPlan {
                thread_id,
                source_file_ids,
                completeness,
            });
        }
        groups.sort_by(|left, right| left.thread_id.cmp(&right.thread_id));

        let mut commits = Vec::new();
        for group in &groups {
            if !group.completeness.is_complete() {
                continue;
            }
            let patch = resolution
                .patches
                .iter()
                .find(|patch| patch.thread_id == group.thread_id)
                .cloned();
            let mut source_commits = Vec::new();
            for source_id in &group.source_file_ids {
                let Some(plan) = input.plans.plan_for(*source_id) else {
                    continue;
                };
                let Some(entry) = source_entries.get(source_id) else {
                    continue;
                };
                let (safe_fact, checkpoint) = match plan {
                    FilePlan::Skip { fact, .. } => {
                        let checkpoint = entry.metadata_checkpoint.as_ref().ok_or_else(|| {
                            PipelineError::Invalid(
                                "Skip source is missing metadata checkpoint".to_owned(),
                            )
                        })?;
                        let mut advance = MetadataCheckpointAdvance::new(
                            checkpoint.parser_version,
                            checkpoint.committed_offset,
                            checkpoint.guard_hash.clone(),
                            checkpoint.processing_status,
                        )
                        .map_err(|error| PipelineError::Invalid(error.to_string()))?;
                        advance.last_successful_scan_at_ms = checkpoint.last_successful_scan_at_ms;
                        advance.last_error_code = checkpoint.last_error_code.clone();
                        (fact.clone(), advance)
                    }
                    FilePlan::ReadFrom { .. } | FilePlan::Rebuild { .. } => {
                        let Some(parsed) = parsed_by_source.get(source_id) else {
                            continue;
                        };
                        if !parsed.stable() {
                            continue;
                        }
                        let fact = parsed.fact.as_ref().expect("stable parsed fact");
                        let safe_fact = fact
                            .to_safe_fact(
                                entry.source.file_generation,
                                self.metadata_parser_version,
                                parsed.last_processed_offset,
                                self.resolved_at_ms,
                                &parsed.final_continuation,
                            )
                            .map_err(|error| PipelineError::Invalid(error.to_string()))?;
                        let offset = i64::try_from(parsed.last_processed_offset).map_err(|_| {
                            PipelineError::Invalid("metadata offset does not fit in i64".to_owned())
                        })?;
                        let mut checkpoint = MetadataCheckpointAdvance::new(
                            self.metadata_parser_version,
                            offset,
                            parsed.guard_hash.clone(),
                            CheckpointProcessingStatus::Ready,
                        )
                        .map_err(|error| PipelineError::Invalid(error.to_string()))?;
                        checkpoint.last_successful_scan_at_ms = Some(self.resolved_at_ms);
                        (safe_fact, checkpoint)
                    }
                    FilePlan::Reject { .. } => continue,
                };
                let expected_previous_thread_id = entry.source.thread_id.clone();
                source_commits.push(
                    MetadataSourceCommit::new(
                        *source_id,
                        entry.source.file_generation,
                        expected_previous_thread_id,
                        safe_fact.owning_thread_id.clone(),
                        safe_fact,
                        checkpoint,
                    )
                    .map_err(|error| PipelineError::Invalid(error.to_string()))?,
                );
            }
            if patch.is_none() && source_commits.is_empty() {
                continue;
            }
            commits.push(
                MetadataThreadCommit::new(group.thread_id.clone(), patch, source_commits)
                    .map_err(|error| PipelineError::Invalid(error.to_string()))?,
            );
        }
        let commit_batch = if commits.is_empty() {
            None
        } else {
            Some(
                MetadataCommitBatch::new(commits)
                    .map_err(|error| PipelineError::Invalid(error.to_string()))?,
            )
        };
        diagnostics.extend(resolution.diagnostics.iter().filter_map(|diagnostic| {
            diagnostic
                .thread_id
                .as_ref()
                .map(|thread_id| PipelineDiagnostic {
                    code: "RESOLUTION_DIAGNOSTIC",
                    source_file_id: diagnostic.source_file_id,
                    path: None,
                    thread_id: Some(thread_id.clone()),
                })
        }));
        Ok(PipelineResolution {
            groups,
            resolution,
            commit_batch,
            diagnostics,
        })
    }

    /// Commit only complete groups. `None` means there was no complete group;
    /// in particular, this path performs no storage write for an incomplete
    /// Thread group.
    pub fn commit(
        &self,
        ledger: &Ledger,
        resolution: &PipelineResolution,
    ) -> Result<Option<crate::domain::CommitOutcome>, PipelineError> {
        let Some(batch) = resolution.commit_batch.clone() else {
            return Ok(None);
        };
        let mut committed_group_count = 0_usize;
        let mut data_revision = None;
        let mut data_changed = false;
        let mut first_error = None;
        for group in batch.groups {
            let group_batch = MetadataCommitBatch::new(vec![group])
                .map_err(|error| PipelineError::Invalid(error.to_string()))?;
            match ledger.commit_metadata(group_batch) {
                Ok(outcome) => {
                    committed_group_count =
                        committed_group_count.saturating_add(outcome.committed_group_count);
                    data_revision = Some(outcome.data_revision);
                    data_changed |= outcome.data_changed;
                }
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        if let Some(error) = first_error {
            return Err(PipelineError::Storage(error));
        }
        let data_revision = data_revision.ok_or_else(|| {
            PipelineError::Invalid("metadata commit contained no successful groups".to_owned())
        })?;
        let outcome =
            crate::domain::CommitOutcome::new(committed_group_count, data_revision, data_changed)
                .map_err(|error| PipelineError::Invalid(error.to_string()))?;
        Ok(Some(outcome))
    }
}

fn matching_stable_fact(
    entry: &MetadataScanStateEntry,
) -> Option<&crate::domain::RolloutMetadataFact> {
    let SafeFactState::Matching(fact) = &entry.safe_fact else {
        return None;
    };
    if !matches!(
        fact.continuation_state,
        crate::domain::ContinuationState::ReplayedAncestor
            | crate::domain::ContinuationState::OwningLive
    ) {
        return None;
    }
    let thread_id = entry.source.thread_id.as_deref()?;
    (fact.owning_thread_id == thread_id).then_some(fact)
}

fn collect_thread_ids(
    resolution: &ResolutionResult,
    rollout_facts: &[RolloutThreadFact],
    sources: &[SourceFileState],
) -> BTreeSet<String> {
    let mut ids: BTreeSet<String> = resolution.affected_thread_ids.iter().cloned().collect();
    ids.extend(
        rollout_facts
            .iter()
            .map(|fact| fact.owning_thread_id.clone()),
    );
    ids.extend(sources.iter().filter_map(|source| source.thread_id.clone()));
    ids
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use rusqlite::Connection;

    use super::*;
    use crate::{
        domain::{
            BuildDisposition, ContinuationState, FactQualityStatus, FileStatus,
            MetadataCheckpointState, OwnershipConfidence as DomainOwnershipConfidence,
            RolloutMetadataFact, SafeFactMismatchReason, SourceObservationResult,
        },
        storage::LedgerOptions,
    };

    fn fixture_path(name: &str) -> String {
        std::env::temp_dir()
            .join("usagi-scanner-pipeline")
            .join(name.trim_start_matches('/'))
            .to_string_lossy()
            .into_owned()
    }

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempLedger {
        directory: PathBuf,
        ledger: Ledger,
    }

    impl TempLedger {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "usagi-pipeline-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&directory).unwrap();
            let home = directory.join("codex");
            fs::create_dir(&home).unwrap();
            let ledger = Ledger::open(LedgerOptions::new(directory.join("mu.db"), home)).unwrap();
            Self { directory, ledger }
        }
    }

    impl Drop for TempLedger {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn discovered(path: &str, device_id: i64, inode: i64, size: i64, mtime: i64) -> DiscoveredFile {
        DiscoveredFile {
            path: PathBuf::from(path),
            source_area: if path.contains("archived") {
                crate::domain::SourceArea::ArchivedSessions
            } else {
                crate::domain::SourceArea::Sessions
            },
            device_id,
            inode,
            size,
            mtime_ns: mtime,
            filename_thread_id_candidate: None,
        }
    }

    fn snapshot(files: Vec<DiscoveredFile>) -> DiscoverySnapshot {
        DiscoverySnapshot {
            started_at_ms: 10,
            sessions: SourceRegionStatus::Complete,
            archived_sessions: SourceRegionStatus::Complete,
            files,
            diagnostics: Vec::new(),
        }
    }

    fn source(source_id: i64, size: i64, binding: Option<&str>) -> SourceFileState {
        SourceFileState::new(
            source_id,
            binding.map(str::to_owned),
            fixture_path(&format!("sessions/rollout-{source_id}.jsonl")),
            crate::domain::SourceArea::Sessions,
            1,
            source_id,
            1,
            size,
            1,
            FileStatus::Present,
            10,
        )
        .unwrap()
    }

    fn checkpoint(
        source_id: i64,
        parser_version: i64,
        offset: i64,
        guard: Option<Vec<u8>>,
        status: CheckpointProcessingStatus,
    ) -> MetadataCheckpointState {
        MetadataCheckpointState {
            source_file_id: source_id,
            parser_version,
            committed_offset: offset,
            guard_hash: guard,
            processing_status: status,
            last_successful_scan_at_ms: Some(10),
            last_error_code: (status == CheckpointProcessingStatus::Error)
                .then(|| "RETRYABLE_ERROR".to_owned()),
        }
    }

    fn safe_fact(source_id: i64, offset: i64, owner: &str) -> RolloutMetadataFact {
        RolloutMetadataFact {
            source_file_id: source_id,
            file_generation: 1,
            metadata_parser_version: 1,
            resolved_through_offset: offset,
            owning_thread_id: owner.to_owned(),
            continuation_state: ContinuationState::OwningLive,
            cwd: None,
            cwd_provenance: None,
            cwd_record_offset: None,
            created_at_ms: None,
            latest_context_model: None,
            latest_context_turn_id: None,
            latest_context_at_ms: None,
            parent_thread_id_hint: None,
            parent_hint_provenance: None,
            parent_hint_record_offset: None,
            agent_role_hint: None,
            agent_role_provenance: None,
            agent_role_record_offset: None,
            agent_path: None,
            agent_path_provenance: None,
            agent_path_record_offset: None,
            replay_start_offset: None,
            owning_records_start_offset: Some(0),
            ownership_confidence: DomainOwnershipConfidence::Confirmed,
            fact_quality_status: FactQualityStatus::Complete,
            relationship_conflict: false,
            updated_at_ms: 10,
        }
    }

    fn outcome(source_id: i64) -> SourceOutcome {
        SourceOutcome::new(vec![
            SourceObservationResult::new(
                source_id,
                1,
                false,
                false,
                false,
                Vec::new(),
                BuildDisposition::Unchanged,
            )
            .unwrap(),
        ])
        .unwrap()
    }

    fn plan_one(entry: MetadataScanStateEntry) -> FilePlan {
        let pipeline = MetadataPipeline::new(1, 10).unwrap();
        let snapshot = snapshot(vec![discovered(
            &fixture_path("sessions/rollout-1.jsonl"),
            1,
            1,
            entry.source.observed_size,
            1,
        )]);
        pipeline
            .plan_files(
                &snapshot,
                &outcome(entry.source.source_file_id),
                &MetadataScanState {
                    entries: vec![entry],
                },
            )
            .plans
            .into_iter()
            .next()
            .unwrap()
    }

    #[test]
    fn identity_lifecycle_matrix_preserves_moves_and_rebuilds_content_generations() {
        let fixture = TempLedger::new();
        let pipeline = MetadataPipeline::new(1, 10).unwrap();
        let first_snapshot = snapshot(vec![discovered(
            &fixture_path("sessions/rollout-a.jsonl"),
            1,
            2,
            10,
            1,
        )]);
        let (first, _) = pipeline
            .record_and_load(&fixture.ledger, &first_snapshot)
            .unwrap();
        let source_id = first.results[0].source_file_id;
        let connection = Connection::open(fixture.ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE source_files SET thread_id = 'thread-a' WHERE source_file_id = ?1",
                [source_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE source_checkpoints SET parser_version = 1, committed_offset = 5,
                 guard_hash = X'01', processing_status = 'ready'
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [source_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_checkpoints (
                    source_file_id, consumer_kind, parser_version, committed_offset,
                    guard_hash, processing_status
                 ) VALUES (?1, 'usage', 1, 4, X'02', 'ready')",
                [source_id],
            )
            .unwrap();
        drop(connection);

        let renamed = snapshot(vec![discovered(
            &fixture_path("archived_sessions/rollout-a.jsonl"),
            1,
            2,
            10,
            1,
        )]);
        let (renamed_outcome, renamed_state) =
            pipeline.record_and_load(&fixture.ledger, &renamed).unwrap();
        assert_eq!(renamed_outcome.results[0].source_file_id, source_id);
        assert!(renamed_outcome.results[0].moved);
        assert_eq!(renamed_outcome.results[0].file_generation, 1);
        assert_eq!(
            renamed_state.entries[0].source.thread_id.as_deref(),
            Some("thread-a")
        );
        assert_eq!(
            renamed_state.entries[0]
                .metadata_checkpoint
                .as_ref()
                .unwrap()
                .committed_offset,
            5
        );

        let version_plan = MetadataPipeline::new(2, 10).unwrap().plan_files(
            &renamed,
            &renamed_outcome,
            &renamed_state,
        );
        assert!(matches!(
            version_plan.plans[0],
            FilePlan::Rebuild {
                reason: RebuildReason::ParserVersionChanged,
                ..
            }
        ));
        let connection = Connection::open(fixture.ledger.database_path()).unwrap();
        let usage_before_rewrite: (String, i64) = connection
            .query_row(
                "SELECT processing_status, committed_offset FROM source_checkpoints
                 WHERE source_file_id = ?1 AND consumer_kind = 'usage'",
                [source_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(usage_before_rewrite, ("ready".to_owned(), 4));
        drop(connection);

        let copied = snapshot(vec![
            discovered(
                &fixture_path("archived_sessions/rollout-a.jsonl"),
                1,
                2,
                10,
                1,
            ),
            discovered(&fixture_path("sessions/rollout-copy.jsonl"), 1, 3, 10, 1),
        ]);
        let (copy_outcome, _) = pipeline.record_and_load(&fixture.ledger, &copied).unwrap();
        assert_eq!(copy_outcome.results[0].source_file_id, source_id);
        assert!(copy_outcome.results[1].created);
        assert_ne!(copy_outcome.results[1].source_file_id, source_id);

        let replaced = snapshot(vec![
            discovered(
                &fixture_path("archived_sessions/rollout-a.jsonl"),
                9,
                9,
                10,
                2,
            ),
            discovered(&fixture_path("sessions/rollout-copy.jsonl"), 1, 3, 10, 1),
        ]);
        let (replacement, replacement_state) = pipeline
            .record_and_load(&fixture.ledger, &replaced)
            .unwrap();
        assert_eq!(replacement.results[0].source_file_id, source_id);
        assert!(replacement.results[0].replaced);
        assert_eq!(replacement.results[0].file_generation, 2);
        assert_eq!(replacement_state.entries[0].source.thread_id, None);
        assert_eq!(
            replacement_state.entries[0]
                .metadata_checkpoint
                .as_ref()
                .unwrap()
                .processing_status,
            CheckpointProcessingStatus::RebuildRequired
        );
        let connection = Connection::open(fixture.ledger.database_path()).unwrap();
        let usage_status: String = connection
            .query_row(
                "SELECT processing_status FROM source_checkpoints
                 WHERE source_file_id = ?1 AND consumer_kind = 'usage'",
                [source_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(usage_status, "rebuild_required");
        drop(connection);

        let truncated = snapshot(vec![
            discovered(
                &fixture_path("archived_sessions/rollout-a.jsonl"),
                9,
                9,
                5,
                3,
            ),
            discovered(&fixture_path("sessions/rollout-copy.jsonl"), 1, 3, 10, 1),
        ]);
        let (truncation, _) = pipeline
            .record_and_load(&fixture.ledger, &truncated)
            .unwrap();
        assert_eq!(truncation.results[0].file_generation, 3);
        assert!(truncation.results[0].replaced);

        let rewritten = snapshot(vec![
            discovered(
                &fixture_path("archived_sessions/rollout-a.jsonl"),
                9,
                9,
                5,
                4,
            ),
            discovered(&fixture_path("sessions/rollout-copy.jsonl"), 1, 3, 10, 1),
        ]);
        let (rewrite, _) = pipeline
            .record_and_load(&fixture.ledger, &rewritten)
            .unwrap();
        assert_eq!(rewrite.results[0].file_generation, 4);
        assert!(rewrite.results[0].replaced);

        pipeline
            .record_and_load(&fixture.ledger, &snapshot(Vec::new()))
            .unwrap();
        let missing = fixture
            .ledger
            .load_metadata_scan_state([source_id])
            .unwrap();
        assert_eq!(missing.entries[0].source.file_status, FileStatus::Missing);

        let restored = snapshot(vec![discovered(
            &fixture_path("sessions/rollout-restored.jsonl"),
            9,
            9,
            5,
            4,
        )]);
        let (restored_outcome, restored_state) = pipeline
            .record_and_load(&fixture.ledger, &restored)
            .unwrap();
        assert_eq!(restored_outcome.results[0].source_file_id, source_id);
        assert_eq!(restored_outcome.results[0].file_generation, 4);
        assert_eq!(
            restored_state.entries[0].source.file_status,
            FileStatus::Present
        );

        let unavailable = DiscoverySnapshot {
            sessions: SourceRegionStatus::Unavailable("PERMISSION_DENIED".to_owned()),
            ..snapshot(Vec::new())
        };
        pipeline
            .record_and_load(&fixture.ledger, &unavailable)
            .unwrap();
        let preserved = fixture
            .ledger
            .load_metadata_scan_state([source_id])
            .unwrap();
        assert_eq!(preserved.entries[0].source.file_status, FileStatus::Present);
    }

    #[test]
    fn crash_windows_read_old_checkpoint_until_commit_then_skip() {
        let source = source(1, 20, Some("thread"));
        let before_commit = MetadataScanStateEntry {
            source: source.clone(),
            metadata_checkpoint: Some(checkpoint(
                1,
                1,
                10,
                Some(vec![1]),
                CheckpointProcessingStatus::Ready,
            )),
            safe_fact: SafeFactState::Matching(safe_fact(1, 10, "thread")),
        };
        for _ in 0..2 {
            assert!(matches!(
                plan_one(before_commit.clone()),
                FilePlan::ReadFrom {
                    start_offset: 10,
                    observed_size: 20,
                    ..
                }
            ));
        }

        let after_commit = MetadataScanStateEntry {
            source,
            metadata_checkpoint: Some(checkpoint(
                1,
                1,
                20,
                Some(vec![2]),
                CheckpointProcessingStatus::Ready,
            )),
            safe_fact: SafeFactState::Matching(safe_fact(1, 20, "thread")),
        };
        assert!(matches!(plan_one(after_commit), FilePlan::Skip { .. }));
    }

    #[test]
    fn planning_matrix_rebuilds_out_of_range_versions_pending_and_unstable_state() {
        let cases = [
            (
                MetadataScanStateEntry {
                    source: source(1, 10, Some("thread")),
                    metadata_checkpoint: Some(checkpoint(
                        1,
                        1,
                        11,
                        Some(vec![1]),
                        CheckpointProcessingStatus::Ready,
                    )),
                    safe_fact: SafeFactState::None,
                },
                RebuildReason::CheckpointOutOfRange,
            ),
            (
                MetadataScanStateEntry {
                    source: source(1, 10, Some("thread")),
                    metadata_checkpoint: Some(checkpoint(
                        1,
                        2,
                        5,
                        Some(vec![1]),
                        CheckpointProcessingStatus::Ready,
                    )),
                    safe_fact: SafeFactState::None,
                },
                RebuildReason::ParserVersionChanged,
            ),
            (
                MetadataScanStateEntry {
                    source: source(1, 10, Some("thread")),
                    metadata_checkpoint: Some(checkpoint(
                        1,
                        1,
                        5,
                        Some(vec![1]),
                        CheckpointProcessingStatus::Pending,
                    )),
                    safe_fact: SafeFactState::None,
                },
                RebuildReason::PendingCheckpoint,
            ),
            (
                MetadataScanStateEntry {
                    source: source(1, 10, Some("thread")),
                    metadata_checkpoint: Some(checkpoint(
                        1,
                        1,
                        5,
                        Some(vec![1]),
                        CheckpointProcessingStatus::Ready,
                    )),
                    safe_fact: SafeFactState::Stale(SafeFactMismatchReason::OffsetMismatch),
                },
                RebuildReason::SafeFactStale,
            ),
        ];
        for (entry, expected) in cases {
            assert!(matches!(
                plan_one(entry),
                FilePlan::Rebuild { reason, .. } if reason == expected
            ));
        }

        let pending_zero = MetadataScanStateEntry {
            source: source(1, 10, None),
            metadata_checkpoint: Some(checkpoint(
                1,
                1,
                0,
                None,
                CheckpointProcessingStatus::Pending,
            )),
            safe_fact: SafeFactState::None,
        };
        assert!(matches!(
            plan_one(pending_zero),
            FilePlan::ReadFrom {
                start_offset: 0,
                resume_state: ResumeState::AwaitOwningMeta,
                ..
            }
        ));
    }

    #[test]
    fn error_and_nonzero_resume_require_guard_binding_and_stable_owning_fact() {
        let verified_error = MetadataScanStateEntry {
            source: source(1, 20, Some("thread")),
            metadata_checkpoint: Some(checkpoint(
                1,
                1,
                5,
                Some(vec![1]),
                CheckpointProcessingStatus::Error,
            )),
            safe_fact: SafeFactState::Matching(safe_fact(1, 5, "thread")),
        };
        assert!(matches!(
            plan_one(verified_error),
            FilePlan::ReadFrom {
                start_offset: 5,
                resume_state: ResumeState::OwningLive { .. },
                ..
            }
        ));

        let error_zero = MetadataScanStateEntry {
            source: source(1, 20, Some("thread")),
            metadata_checkpoint: Some(checkpoint(1, 1, 0, None, CheckpointProcessingStatus::Error)),
            safe_fact: SafeFactState::None,
        };
        assert!(matches!(
            plan_one(error_zero),
            FilePlan::Rebuild {
                reason: RebuildReason::CheckpointErrorUnverified,
                ..
            }
        ));

        let missing_binding = MetadataScanStateEntry {
            source: source(1, 20, None),
            metadata_checkpoint: Some(checkpoint(
                1,
                1,
                5,
                Some(vec![1]),
                CheckpointProcessingStatus::Ready,
            )),
            safe_fact: SafeFactState::Matching(safe_fact(1, 5, "thread")),
        };
        assert!(matches!(
            plan_one(missing_binding),
            FilePlan::Rebuild {
                reason: RebuildReason::ContinuationUnstable,
                ..
            }
        ));

        let missing_guard = MetadataScanStateEntry {
            source: source(1, 20, Some("thread")),
            metadata_checkpoint: Some(checkpoint(1, 1, 5, None, CheckpointProcessingStatus::Ready)),
            safe_fact: SafeFactState::Matching(safe_fact(1, 5, "thread")),
        };
        assert!(matches!(
            plan_one(missing_guard),
            FilePlan::Rebuild {
                reason: RebuildReason::GuardMissing,
                ..
            }
        ));

        let mut unstable = safe_fact(1, 5, "thread");
        unstable.continuation_state = ContinuationState::Unstable;
        unstable.ownership_confidence = DomainOwnershipConfidence::Unresolved;
        let unstable = MetadataScanStateEntry {
            source: source(1, 20, Some("thread")),
            metadata_checkpoint: Some(checkpoint(
                1,
                1,
                5,
                Some(vec![1]),
                CheckpointProcessingStatus::Ready,
            )),
            safe_fact: SafeFactState::Matching(unstable),
        };
        assert!(matches!(
            plan_one(unstable),
            FilePlan::Rebuild {
                reason: RebuildReason::ContinuationUnstable,
                ..
            }
        ));
    }
}
