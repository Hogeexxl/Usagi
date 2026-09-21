//! Codex metadata and usage ingestion orchestration.
//!
//! This module owns the Codex-specific scan algorithm. It receives a
//! source-bound `CodexStorage` capability from the adapter and delegates every
//! durable operation through the frozen storage surface.

use std::{
    collections::BTreeMap,
    fs::File,
    io,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    codex::domain::{
        CheckpointRebuildCommand, ConsumerKind, MetadataScanState, MetadataScanStateEntry,
        SafeFactState,
    },
    codex::storage::CodexStorage,
    codex::{
        GlobalStateReader, ResumeState, RolloutMetadataParser, RolloutParseContext,
        RolloutThreadFact, SessionIndexReader, SessionNameSnapshot, SourceAvailability,
        StateIndexReader, StateSnapshot,
    },
};

mod chunk_reader;
mod discovery;
mod metadata_pipeline;
mod report;
mod usage_commit;
mod usage_consumer;
mod usage_pipeline;
pub(crate) mod usage_processor;

pub(crate) use chunk_reader::{
    ChunkReadError, ChunkReadPlan, FramedItem, GuardHash, PhysicalIdentity,
};
pub(crate) use discovery::{DiscoveredFile, Discovery, DiscoverySnapshot};
pub(crate) use metadata_pipeline::{FilePlan, MetadataPipeline, ParsedSource, PipelinePlan};
pub(crate) use report::ScanReport;

use crate::codex::rollout::{CompleteRolloutLine, OwningThreadCandidate, OwningThreadCandidates};
use chunk_reader::read_chunk;
use metadata_pipeline::PipelineResolutionInput;
use report::ScanReport as IngestionReport;
use usage_consumer::run_usage_round;

pub(crate) struct CodexIngestion;

impl CodexIngestion {
    pub(crate) fn run(
        config: &crate::codex::CodexConfig,
        storage: &CodexStorage<'_>,
        cancellation: &AtomicBool,
    ) -> Result<(), &'static str> {
        let round_at_ms = now_ms();
        let mut report = IngestionReport::new(round_at_ms);
        let mut hard_error = None;

        let state_snapshot = read_state_snapshot(config.metadata().state_index_path());
        if !state_snapshot.status.is_complete() || !state_snapshot.spawn_edges_status.is_complete()
        {
            record_hard_error(&mut report, &mut hard_error, "STATE_SOURCE_UNAVAILABLE");
        }
        if cancelled(cancellation) {
            report.finish(now_ms());
            return Ok(());
        }
        let session_name_snapshot = read_session_snapshot(config.metadata().session_index_path());
        if session_name_snapshot.status == SourceAvailability::Unavailable {
            record_hard_error(&mut report, &mut hard_error, "SESSION_INDEX_UNAVAILABLE");
        }
        let global_state_snapshot =
            GlobalStateReader::read_snapshot(config.metadata().global_state_path());
        if cancelled(cancellation) {
            report.finish(now_ms());
            return Ok(());
        }

        let discovery = Discovery::discover_at(config.home(), round_at_ms);
        report.observe_discovery(discovery.files.len());
        for diagnostic in &discovery.diagnostics {
            if diagnostic.code != "DUPLICATE_PHYSICAL_ALIAS" {
                record_hard_error(&mut report, &mut hard_error, diagnostic.code);
            }
        }
        if !discovery.sessions.is_complete() || !discovery.archived_sessions.is_complete() {
            record_hard_error(&mut report, &mut hard_error, "SOURCE_AREA_UNAVAILABLE");
        }

        let pipeline =
            MetadataPipeline::new(crate::codex::rollout::METADATA_PARSER_VERSION, round_at_ms)
                .map_err(|_| "METADATA_PIPELINE_INVALID")?;
        let usage_carry_proofs = usage_consumer::collect_usage_carry_observation_proofs(
            storage,
            &discovery,
            &mut report,
        )?;
        let (outcome, scan_state) = pipeline
            .record_and_load_with_usage_carry_proofs(storage, &discovery, &usage_carry_proofs)
            .map_err(|_| "SOURCE_OBSERVATION_FAILED")?;
        if cancelled(cancellation) {
            report.finish(now_ms());
            return Ok(());
        }
        for result in &outcome.results {
            report.observe_source(result);
        }
        let plan = pipeline.plan_files(&discovery, &outcome, &scan_state);
        report.observe_plan(&plan);
        for file_plan in &plan.plans {
            if let FilePlan::Reject { error_code, .. } = file_plan {
                record_hard_error(&mut report, &mut hard_error, error_code);
            }
        }
        let parsed_sources = parse_sources(
            &discovery,
            &outcome,
            &scan_state,
            &plan,
            &state_snapshot,
            cancellation,
            &mut report,
            &mut hard_error,
        );
        persist_metadata_rebuilds(storage, &parsed_sources)?;
        if cancelled(cancellation) {
            report.finish(now_ms());
            return Ok(());
        }

        let existing_threads = storage
            .load_existing_threads()
            .map_err(|_| "METADATA_STATE_LOAD_FAILED")?
            .into_iter()
            .map(Into::into)
            .collect();
        let usage_state_snapshot = state_snapshot.clone();
        let resolution = pipeline
            .resolve(PipelineResolutionInput {
                state_snapshot,
                session_name_snapshot,
                global_state_snapshot,
                scan_state,
                plans: plan,
                parsed_sources,
                existing_threads,
            })
            .map_err(|_| "METADATA_RESOLUTION_FAILED")?;
        pipeline
            .commit(storage, &resolution)
            .map_err(|_| "METADATA_COMMIT_FAILED")?;
        if let Err(error_code) = run_usage_round(
            storage,
            &discovery,
            &outcome,
            &usage_state_snapshot,
            cancellation,
            &mut report,
        ) {
            record_hard_error(&mut report, &mut hard_error, error_code);
        }
        report.finish(now_ms());
        hard_error.map_or(Ok(()), Err)
    }
}

fn parse_sources(
    discovery: &DiscoverySnapshot,
    outcome: &crate::codex::domain::SourceOutcome,
    scan_state: &MetadataScanState,
    plan: &PipelinePlan,
    state_snapshot: &StateSnapshot,
    cancellation: &AtomicBool,
    report: &mut ScanReport,
    hard_error: &mut Option<&'static str>,
) -> Vec<ParsedSource> {
    let mut parsed = Vec::new();
    for (index, file) in discovery.files.iter().enumerate() {
        if cancelled(cancellation) {
            break;
        }
        let Some(result) = outcome.results.get(index) else {
            break;
        };
        let Some(file_plan) = plan.plan_for(result.source_file_id) else {
            continue;
        };
        if matches!(file_plan, FilePlan::Skip { .. } | FilePlan::Reject { .. }) {
            continue;
        }
        let Some(entry) = scan_state.get(result.source_file_id) else {
            continue;
        };
        report.observe_body_open_attempt();
        match parse_one(file, entry, file_plan, state_snapshot, cancellation) {
            Ok(value) => {
                report.observe_parse(&value);
                if value.needs_rebuild || !value.stable() {
                    record_hard_error(report, hard_error, "METADATA_CONTINUATION_UNSTABLE");
                }
                parsed.push(value);
            }
            Err(error_code) => {
                report.failed_source();
                record_hard_error(report, hard_error, error_code);
            }
        }
    }
    parsed
}

fn persist_metadata_rebuilds(
    storage: &CodexStorage<'_>,
    parsed_sources: &[ParsedSource],
) -> Result<(), &'static str> {
    let mut source_file_ids = parsed_sources
        .iter()
        .filter(|source| source.needs_rebuild)
        .map(|source| source.source_file_id)
        .collect::<Vec<_>>();
    if source_file_ids.is_empty() {
        return Ok(());
    }
    source_file_ids.sort_unstable();
    source_file_ids.dedup();
    let command = CheckpointRebuildCommand::new(ConsumerKind::Metadata, source_file_ids)
        .map_err(|_| "STORAGE_COMMIT_FAILED")?;
    storage
        .require_checkpoint_rebuild(command)
        .map_err(|_| "STORAGE_COMMIT_FAILED")?;
    Ok(())
}

fn parse_one(
    file: &DiscoveredFile,
    entry: &MetadataScanStateEntry,
    file_plan: &FilePlan,
    state_snapshot: &StateSnapshot,
    cancellation: &AtomicBool,
) -> Result<ParsedSource, &'static str> {
    let (source_file_id, start_offset, observed_size, resume_state) = match file_plan {
        FilePlan::ReadFrom {
            source_file_id,
            start_offset,
            observed_size,
            resume_state,
        } => (
            *source_file_id,
            *start_offset,
            *observed_size,
            resume_state.clone(),
        ),
        FilePlan::Rebuild {
            source_file_id,
            observed_size,
            ..
        } => (
            *source_file_id,
            0,
            *observed_size,
            ResumeState::AwaitOwningMeta,
        ),
        FilePlan::Skip { .. } | FilePlan::Reject { .. } => return Err("METADATA_PLAN_NOT_READ"),
    };
    let observed_size = u64::try_from(observed_size).map_err(|_| "SOURCE_SIZE_INVALID")?;
    let identity = PhysicalIdentity {
        device_id: u64::try_from(file.device_id).map_err(|_| "SOURCE_IDENTITY_INVALID")?,
        inode: u64::try_from(file.inode).map_err(|_| "SOURCE_IDENTITY_INVALID")?,
    };
    let expected_guard = expected_guard(entry, start_offset)?;
    let chunk_plan = ChunkReadPlan {
        path: file.path.clone(),
        identity,
        start_offset,
        observed_size,
        expected_guard,
    };
    let candidates = owning_candidates(file, state_snapshot);
    let existing_fact = match (&resume_state, &entry.safe_fact) {
        (
            ResumeState::OwningLive { .. } | ResumeState::ReplayedAncestor { .. },
            SafeFactState::Matching(fact),
        ) => RolloutThreadFact::from_safe_fact(fact).ok(),
        _ => None,
    };
    let mut parser = RolloutMetadataParser::start_chunk(RolloutParseContext {
        source_file_id,
        chunk_start_offset: start_offset,
        candidates,
        resume_state,
        existing_fact,
    });
    let read_result = read_chunk(&chunk_plan, |item| match item {
        FramedItem::Line(line) => {
            let start = line.start_offset();
            if let Some(line) = CompleteRolloutLine::new(start, line.into_bytes_with_newline()) {
                parser.push(line);
            }
        }
        FramedItem::OversizedCompleteLine(diagnostic) => {
            parser.push_opaque_classified(diagnostic.start_offset, diagnostic.end_offset);
        }
    })
    .map_err(read_error_code)?;
    if cancelled(cancellation) {
        return Err("SCAN_CANCELLED");
    }
    let result = parser.finish();
    Ok(ParsedSource {
        source_file_id,
        fact: result.fact,
        final_continuation: result.final_continuation,
        last_processed_offset: read_result.last_complete_offset,
        guard_hash: read_result
            .guard
            .as_ref()
            .map(|guard| guard.as_bytes().to_vec()),
        needs_rebuild: result.needs_rebuild,
        bytes_read: read_result.bytes_read,
        guard_bytes_read: read_result.guard_bytes_read,
        peak_buffered_body_bytes: read_result.peak_buffered_body_bytes,
        complete_line_count: read_result.complete_line_count,
        oversized_complete_line_count: read_result.oversized_complete_line_count,
        has_half_line: read_result.has_half_line,
        diagnostic_count: result.diagnostic_count,
        malformed_record_count: result.malformed_record_count,
    })
}

fn read_state_snapshot(path: &Path) -> StateSnapshot {
    StateIndexReader::read_snapshot(path).unwrap_or_else(|_| StateSnapshot::unavailable(Vec::new()))
}

fn read_session_snapshot(path: &Path) -> SessionNameSnapshot {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return empty_session_snapshot(),
        Err(_) => return unavailable_session_snapshot(),
    };
    SessionIndexReader::read_snapshot(std::io::BufReader::new(file))
        .unwrap_or_else(|_| unavailable_session_snapshot())
}

fn empty_session_snapshot() -> SessionNameSnapshot {
    SessionNameSnapshot {
        names: BTreeMap::new(),
        facts: Vec::new(),
        diagnostics: Vec::new(),
        status: SourceAvailability::Complete,
    }
}

fn unavailable_session_snapshot() -> SessionNameSnapshot {
    SessionNameSnapshot {
        names: BTreeMap::new(),
        facts: Vec::new(),
        diagnostics: Vec::new(),
        status: SourceAvailability::Unavailable,
    }
}

fn expected_guard(
    entry: &MetadataScanStateEntry,
    start_offset: u64,
) -> Result<Option<GuardHash>, &'static str> {
    if start_offset == 0 {
        return Ok(None);
    }
    let Some(checkpoint) = entry.metadata_checkpoint.as_ref() else {
        return Err("CHECKPOINT_GUARD_MISSING");
    };
    let Some(bytes) = checkpoint.guard_hash.as_deref() else {
        return Err("CHECKPOINT_GUARD_MISSING");
    };
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| "CHECKPOINT_GUARD_INVALID")?;
    Ok(Some(GuardHash::from_bytes(bytes)))
}

fn owning_candidates(file: &DiscoveredFile, state: &StateSnapshot) -> OwningThreadCandidates {
    let state_rollout = state
        .threads
        .iter()
        .find(|thread| {
            thread.rollout_path.as_deref().is_some_and(|rollout_path| {
                crate::platform::paths::same_source_path(Path::new(rollout_path), &file.path)
            })
        })
        .map(|thread| OwningThreadCandidate {
            thread_id: thread.thread_id.clone(),
            confidence: crate::codex::rollout::OwningCandidateConfidence::Confirmed,
        });
    let filename =
        file.filename_thread_id_candidate
            .as_ref()
            .map(|thread_id| OwningThreadCandidate {
                thread_id: thread_id.clone(),
                confidence: crate::codex::rollout::OwningCandidateConfidence::Confirmed,
            });
    OwningThreadCandidates {
        state_rollout,
        filename,
    }
}

fn read_error_code(error: ChunkReadError) -> &'static str {
    match error {
        ChunkReadError::SourceSymlinkRejected => "SOURCE_SYMLINK_REJECTED",
        ChunkReadError::SourceNotRegularFile => "SOURCE_NOT_REGULAR_FILE",
        ChunkReadError::SourceChangedBeforeRead => "SOURCE_CHANGED_BEFORE_READ",
        ChunkReadError::SourceChangedDuringRead => "SOURCE_CHANGED_DURING_READ",
        ChunkReadError::CheckpointOutOfRange => "CHECKPOINT_OUT_OF_RANGE",
        ChunkReadError::InvalidGuardPlan => "CHECKPOINT_GUARD_INVALID",
        ChunkReadError::CheckpointGuardMismatch => "CHECKPOINT_GUARD_MISMATCH",
        ChunkReadError::Io { .. } => "SOURCE_READ_FAILED",
    }
}

fn cancelled(cancellation: &AtomicBool) -> bool {
    cancellation.load(Ordering::Acquire)
}

fn record_hard_error(
    report: &mut ScanReport,
    hard_error: &mut Option<&'static str>,
    code: &'static str,
) {
    report.error(code);
    if hard_error.is_none() {
        *hard_error = Some(code);
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}
