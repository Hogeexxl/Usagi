//! Spec 04 usage consumer integrated into the fixed Spec 03 scan round.
//!
//! This module never enumerates rollout files on its own. It consumes the
//! discovery/source-observation snapshot already owned by the scanner, runs the
//! metadata ownership classifier in streaming mode, and commits each bounded
//! usage batch before continuing from the durable checkpoint.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use crate::{
    codex::domain::{MetadataScanStateEntry, SafeFactState},
    codex::normalization::USAGE_PARSER_VERSION,
    codex::storage::rebuild::{ActivationOutcome, CompletionStatus},
    codex::storage::source_state::{UsageCarryObservationProof, UsageCarryObservationRequirement},
    codex::storage::usage::{
        UsageChainState, UsageCheckpointExpectation, UsageContinuationState, UsageGapReason,
        UsagePlanAction, UsageScanState, UsageSourceScanPlan, UsageSourceStateWrite,
        UsageTailStatus, UsageWorkList, UsageWorkThread,
    },
    codex::{
        CompleteRolloutLine, CompleteUsageLine, EnvelopeKind, RecordOwnership, ResumeState,
        RolloutMetadataParser, RolloutParseContext, RolloutThreadFact, StateSnapshot,
    },
    usage::event::EventKind,
};

use super::usage_commit;
use super::usage_pipeline::{
    ClassifiedOversizedUsageLine, ClassifiedUsageItem, ClassifiedUsageLine, FixedViewTail,
    PipelineDisposition, PlanAction, SourceContinuationState, TailStatus, UsagePipeline,
    UsageSourceCommitDto,
};
use crate::codex::storage::CodexStorage;

use super::{
    cancelled,
    chunk_reader::{
        ChunkReadError, ChunkReadPlan, FramedItem, GuardHash, PhysicalIdentity, ReadControl,
        read_chunk_bounded,
    },
    discovery::{DiscoveredFile, DiscoverySnapshot},
    owning_candidates, read_error_code,
    report::ScanReport,
};

const REPLAY_WINDOW_BYTES: u64 = super::usage_pipeline::MAX_BATCH_BYTES;
const REPLAY_WINDOW_LINES: u64 = super::usage_pipeline::MAX_BATCH_LINES;
const MAX_BATCH_BYTES: u64 = super::usage_pipeline::MAX_BATCH_BYTES;
const MAX_BATCH_LINES: u64 = super::usage_pipeline::MAX_BATCH_LINES;
const MAX_BATCH_CANDIDATES: u64 = super::usage_pipeline::MAX_BATCH_CANDIDATES;
const MAX_LEGAL_LINE_BYTES: u64 = super::usage_pipeline::MAX_LEGAL_LINE_BYTES;
const CLEANUP_ROWS_PER_ROUND: usize = 2048;

#[derive(Debug)]
enum UsageReadStep {
    Prepared {
        dto: Box<UsageSourceCommitDto>,
        metrics: UsageCommitMetrics,
    },
    AwaitingOwnership,
    InvalidSessionData,
    NeedsRebuild,
    NeedsRebuildStop,
}

#[derive(Debug)]
enum UsageThreadOutcome {
    Completed,
    GlobalPlanChanged { retry_thread: bool },
    SessionDataError(&'static str),
    OrdinaryError(&'static str),
    FatalReloadError(&'static str),
}

const SESSION_DATA_INVALID: &str = "USAGE_SESSION_DATA_INVALID";

fn shadow_session_data_error(
    action: PlanAction,
    invalid_session_data: bool,
) -> Option<&'static str> {
    (action == PlanAction::BuildFrom && invalid_session_data).then_some(SESSION_DATA_INVALID)
}

fn to_pipeline_action(action: UsagePlanAction) -> PlanAction {
    match action {
        UsagePlanAction::ReadFrom => PlanAction::ReadFrom,
        UsagePlanAction::BuildFrom => PlanAction::BuildFrom,
        UsagePlanAction::LocalReplay => PlanAction::LocalReplay,
        UsagePlanAction::ResumeOwningLive => PlanAction::ResumeOwningLive,
        UsagePlanAction::VerifyRawTail => PlanAction::VerifyRawTail,
        UsagePlanAction::CompleteOnly => PlanAction::CompleteOnly,
        UsagePlanAction::BeginCarry => PlanAction::BeginCarry,
        UsagePlanAction::ResumeCarry => PlanAction::ResumeCarry,
        UsagePlanAction::Skip => PlanAction::Skip,
        UsagePlanAction::BlockedRelationship => PlanAction::BlockedRelationship,
        UsagePlanAction::RebuildRequired => PlanAction::RebuildRequired,
    }
}

pub(super) fn collect_usage_carry_observation_proofs(
    storage: &CodexStorage<'_>,
    discovery: &DiscoverySnapshot,
    report: &mut ScanReport,
) -> Result<Vec<UsageCarryObservationProof>, &'static str> {
    let requirements = storage
        .load_usage_carry_observation_requirements()
        .map_err(|_| "USAGE_CARRY_PROOF_LOAD_FAILED")?;
    let mut proofs = Vec::with_capacity(requirements.len());
    for requirement in requirements {
        let Some(file) = discovery.files.iter().find(|file| {
            file.device_id == requirement.device_id && file.inode == requirement.inode
        }) else {
            continue;
        };
        let guard_matches = verify_carry_observation_requirement(file, &requirement, report)?;
        proofs.push(UsageCarryObservationProof {
            device_id: requirement.device_id,
            inode: requirement.inode,
            active_committed_offset: requirement.active_committed_offset,
            guard_matches,
        });
    }
    Ok(proofs)
}

fn verify_carry_observation_requirement(
    file: &DiscoveredFile,
    requirement: &UsageCarryObservationRequirement,
    report: &mut ScanReport,
) -> Result<bool, &'static str> {
    let offset = u64::try_from(requirement.active_committed_offset)
        .map_err(|_| "USAGE_CARRY_GUARD_INVALID")?;
    let expected_guard = match (offset, requirement.active_guard_hash.as_deref()) {
        (0, None) => None,
        (1.., Some(bytes)) => Some(guard_from_slice(bytes)?),
        _ => return Ok(false),
    };
    let identity = PhysicalIdentity {
        device_id: u64::try_from(requirement.device_id)
            .map_err(|_| "USAGE_SOURCE_IDENTITY_INVALID")?,
        inode: u64::try_from(requirement.inode).map_err(|_| "USAGE_SOURCE_IDENTITY_INVALID")?,
    };
    let started = Instant::now();
    match read_chunk_bounded(
        &ChunkReadPlan {
            path: file.path.clone(),
            identity,
            start_offset: offset,
            observed_size: offset,
            expected_guard,
        },
        |_| ReadControl::Continue,
    ) {
        Ok(result) => {
            report.observe_usage_read(&result, 0, started.elapsed());
            Ok(true)
        }
        Err(
            ChunkReadError::CheckpointGuardMismatch
            | ChunkReadError::SourceChangedBeforeRead
            | ChunkReadError::SourceChangedDuringRead
            | ChunkReadError::CheckpointOutOfRange,
        ) => Ok(false),
        Err(error) => Err(read_error_code(error)),
    }
}

pub(super) fn run_usage_round(
    storage: &CodexStorage<'_>,
    discovery: &DiscoverySnapshot,
    outcome: &crate::codex::domain::SourceOutcome,
    state_snapshot: &StateSnapshot,
    cancellation: &AtomicBool,
    report: &mut ScanReport,
) -> Result<(), &'static str> {
    if discovery.files.len() != outcome.results.len() {
        return Err("USAGE_DISCOVERY_RESULT_MISMATCH");
    }
    let usage = storage;
    let mut present = BTreeMap::<i64, (&DiscoveredFile, i64)>::new();
    for (file, observation) in discovery.files.iter().zip(&outcome.results) {
        present.insert(
            observation.source_file_id,
            (file, observation.file_generation),
        );
    }
    let present_ids = present.keys().copied().collect::<Vec<_>>();
    let discovery_complete =
        discovery.sessions.is_complete() && discovery.archived_sessions.is_complete();

    let quarantine_state = usage
        .active_quarantine_state()
        .map_err(|_| "USAGE_QUARANTINE_STATE_FAILED")?;
    let work_present_ids = if quarantine_state.dirty {
        present_ids.clone()
    } else {
        let skipped = quarantine_state
            .unchanged_source_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        present_ids
            .iter()
            .copied()
            .filter(|source_id| !skipped.contains(source_id))
            .collect::<Vec<_>>()
    };
    let mut worklist = load_work_list(&usage, &work_present_ids, report, false)?;
    let mut first_group_error = None;

    // A global transition invalidates every old worklist.  The reloaded
    // lightweight list is the only source of Thread execution order after a
    // transition; detailed plans are loaded only for the current Thread.
    let mut skip_thread_ids = BTreeSet::new();
    'global_plan: loop {
        if quarantine_state.dirty
            && worklist.epoch.active_epoch > 0
            && worklist.epoch.build_epoch.is_none()
        {
            if !discovery_complete {
                return Ok(());
            }
            usage
                .begin_rebuild(USAGE_PARSER_VERSION, &present_ids, now_ms())
                .map_err(|_| "USAGE_QUARANTINE_RETRY_BEGIN_FAILED")?;
            report.observe_usage_global_replan();
            worklist = load_work_list(&usage, &present_ids, report, true)?;
            skip_thread_ids.clear();
            continue 'global_plan;
        }
        if worklist.epoch.active_epoch == 0 && worklist.epoch.build_epoch.is_none() {
            if !discovery_complete {
                return Ok(());
            }
            usage
                .begin_rebuild(USAGE_PARSER_VERSION, &present_ids, now_ms())
                .map_err(|_| "USAGE_REBUILD_BEGIN_FAILED")?;
            report.observe_usage_global_replan();
            worklist = load_work_list(&usage, &present_ids, report, true)?;
            skip_thread_ids.clear();
            continue 'global_plan;
        }

        // A parser/canonical change is a whole-ledger shadow rebuild. The
        // replacement itself resets all existing build members when the target
        // parser changes, while present IDs cover newly observed members.
        if worklist.epoch.working_parser_version() != USAGE_PARSER_VERSION {
            if !discovery_complete {
                return Ok(());
            }
            if worklist.epoch.build_epoch.is_some() {
                usage
                    .replace_build_sources(
                        USAGE_PARSER_VERSION,
                        &present_ids,
                        &present_ids,
                        now_ms(),
                    )
                    .map_err(|_| "USAGE_REBUILD_REPLACE_FAILED")?;
            } else {
                usage
                    .begin_rebuild(USAGE_PARSER_VERSION, &present_ids, now_ms())
                    .map(|_| ())
                    .map_err(|_| "USAGE_REBUILD_BEGIN_FAILED")?;
            }
            report.observe_usage_global_replan();
            worklist = load_work_list(&usage, &present_ids, report, true)?;
            skip_thread_ids.clear();
            continue 'global_plan;
        }

        for work_thread in worklist.threads.clone() {
            if cancelled(cancellation) {
                break;
            }
            if skip_thread_ids.contains(&work_thread.thread_id) {
                continue;
            }
            match process_thread_group(
                &usage,
                &work_thread,
                &worklist.epoch,
                &present,
                &present_ids,
                state_snapshot,
                discovery_complete,
                cancellation,
                report,
            ) {
                UsageThreadOutcome::Completed => {}
                UsageThreadOutcome::GlobalPlanChanged { retry_thread } => {
                    report.observe_usage_global_replan();
                    worklist = load_work_list(&usage, &present_ids, report, true)?;
                    if !retry_thread {
                        skip_thread_ids.insert(work_thread.thread_id.clone());
                    }
                    continue 'global_plan;
                }
                UsageThreadOutcome::SessionDataError(error_code) => {
                    report.failed_source();
                    report.error(error_code);
                    if worklist.epoch.build_epoch.is_none() {
                        return Err(error_code);
                    }
                    usage
                        .quarantine_thread(&work_thread.thread_id, error_code, now_ms())
                        .map_err(|_| "USAGE_SESSION_QUARANTINE_FAILED")?;
                    report.observe_usage_global_replan();
                    worklist = load_work_list(&usage, &present_ids, report, true)?;
                    skip_thread_ids.clear();
                    continue 'global_plan;
                }
                UsageThreadOutcome::OrdinaryError(error_code) => {
                    report.failed_source();
                    report.error(error_code);
                    first_group_error.get_or_insert(error_code);
                }
                UsageThreadOutcome::FatalReloadError(error_code) => {
                    report.failed_source();
                    report.error(error_code);
                    return Err(error_code);
                }
            }
        }
        break 'global_plan;
    }

    if cancelled(cancellation) {
        return Ok(());
    }

    // Activation requires a complete discovery proof from this same round and
    // a manifest whose every member has a fresh Rebuilt/Carried proof. A failed
    // Thread group necessarily leaves a pending/blocked member, so this proof
    // cannot accidentally activate partial data.
    if discovery_complete && let Some(build_epoch) = worklist.epoch.build_epoch {
        let snapshot = usage
            .begin_rebuild(USAGE_PARSER_VERSION, &present_ids, now_ms())
            .map_err(|_| "USAGE_REBUILD_RESUME_FAILED")?;
        if snapshot.build_epoch == build_epoch
            && snapshot.members.iter().all(|member| {
                matches!(
                    member.completion_status,
                    CompletionStatus::Rebuilt
                        | CompletionStatus::Carried
                        | CompletionStatus::Quarantined
                )
            })
        {
            let ActivationOutcome { .. } = usage
                .activate_rebuild(build_epoch, &present_ids)
                .map_err(|_| "USAGE_REBUILD_ACTIVATE_FAILED")?;
        }
    }

    // Old epochs are invisible after activation. Cleanup is deliberately
    // bounded and does not affect data_revision.
    let _ = usage
        .cleanup_inactive(CLEANUP_ROWS_PER_ROUND)
        .map_err(|_| "USAGE_CLEANUP_FAILED")?;
    match first_group_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "preserve the established scanner group processing seam"
)]
fn process_thread_group(
    usage: &CodexStorage<'_>,
    work_thread: &UsageWorkThread,
    expected_epoch: &crate::domain::SourceUsageEpochState,
    present: &BTreeMap<i64, (&DiscoveredFile, i64)>,
    present_ids: &[i64],
    state_snapshot: &StateSnapshot,
    discovery_complete: bool,
    cancellation: &AtomicBool,
    report: &mut ScanReport,
) -> UsageThreadOutcome {
    let mut scan = match load_exact_state(
        usage,
        &work_thread.source_file_ids,
        expected_epoch.clone(),
        report,
    ) {
        Ok(scan) => scan,
        Err(error_code) => return UsageThreadOutcome::OrdinaryError(error_code),
    };

    'group_loop: loop {
        if cancelled(cancellation) {
            return UsageThreadOutcome::Completed;
        }
        let group = scan.plans.to_vec();
        if group.is_empty() {
            return UsageThreadOutcome::Completed;
        }
        if group.iter().all(|plan| {
            matches!(
                plan.action,
                UsagePlanAction::Skip | UsagePlanAction::BlockedRelationship
            )
        }) {
            return if group
                .iter()
                .any(|plan| plan.action == UsagePlanAction::BlockedRelationship)
            {
                UsageThreadOutcome::SessionDataError("USAGE_SESSION_RELATIONSHIP_INVALID")
            } else {
                UsageThreadOutcome::Completed
            };
        }

        // Planner-owned control transitions happen before any source payload is
        // prepared. Local transitions exact-reload this Thread; global
        // transitions return to the outer worklist loop so no stale worklist
        // or detailed plan can be reused.
        for plan in &group {
            match plan.action {
                UsagePlanAction::Skip | UsagePlanAction::BlockedRelationship => {}
                UsagePlanAction::BeginCarry => {
                    if !discovery_complete {
                        return UsageThreadOutcome::Completed;
                    }
                    if usage.begin_carry(plan.source_file_id, now_ms()).is_err() {
                        return UsageThreadOutcome::OrdinaryError("USAGE_CARRY_BEGIN_FAILED");
                    }
                    if cancelled(cancellation) {
                        return UsageThreadOutcome::Completed;
                    }
                    scan = match load_exact_state(
                        usage,
                        &work_thread.source_file_ids,
                        expected_epoch.clone(),
                        report,
                    ) {
                        Ok(scan) => scan,
                        Err(error_code) => {
                            return UsageThreadOutcome::FatalReloadError(error_code);
                        }
                    };
                    continue 'group_loop;
                }
                UsagePlanAction::ResumeCarry => {
                    if let Some((file, _)) = present.get(&plan.source_file_id).copied()
                        && !match verify_present_carry_prefix(file, plan, report) {
                            Ok(matches) => matches,
                            Err(error_code) => {
                                return UsageThreadOutcome::OrdinaryError(error_code);
                            }
                        }
                    {
                        if !discovery_complete {
                            return UsageThreadOutcome::Completed;
                        }
                        if usage
                            .replace_build_sources(
                                USAGE_PARSER_VERSION,
                                &present_ids,
                                &[plan.source_file_id],
                                now_ms(),
                            )
                            .is_err()
                        {
                            return UsageThreadOutcome::OrdinaryError(
                                "USAGE_REBUILD_REPLACE_FAILED",
                            );
                        }
                        return UsageThreadOutcome::GlobalPlanChanged { retry_thread: true };
                    } else {
                        if let Err(error) = usage.resume_carry(plan.source_file_id, now_ms()) {
                            if error.requires_rebuild() {
                                return UsageThreadOutcome::OrdinaryError("USAGE_CARRY_CONFLICT");
                            } else {
                                return UsageThreadOutcome::OrdinaryError(
                                    "USAGE_CARRY_RESUME_FAILED",
                                );
                            }
                        }
                    }
                    if cancelled(cancellation) {
                        return UsageThreadOutcome::Completed;
                    }
                    scan = match load_exact_state(
                        usage,
                        &work_thread.source_file_ids,
                        expected_epoch.clone(),
                        report,
                    ) {
                        Ok(scan) => scan,
                        Err(error_code) => {
                            return UsageThreadOutcome::FatalReloadError(error_code);
                        }
                    };
                    continue 'group_loop;
                }
                UsagePlanAction::CompleteOnly => {
                    if usage.complete_only(plan.source_file_id, now_ms()).is_err() {
                        return UsageThreadOutcome::OrdinaryError("USAGE_COMPLETE_ONLY_FAILED");
                    }
                    if cancelled(cancellation) {
                        return UsageThreadOutcome::Completed;
                    }
                    scan = match load_exact_state(
                        usage,
                        &work_thread.source_file_ids,
                        expected_epoch.clone(),
                        report,
                    ) {
                        Ok(scan) => scan,
                        Err(error_code) => {
                            return UsageThreadOutcome::FatalReloadError(error_code);
                        }
                    };
                    continue 'group_loop;
                }
                UsagePlanAction::RebuildRequired => {
                    if !discovery_complete {
                        return UsageThreadOutcome::Completed;
                    }
                    if let Err(error_code) =
                        replace_or_begin(usage, &scan, present_ids, [plan.source_file_id])
                    {
                        return UsageThreadOutcome::OrdinaryError(error_code);
                    }
                    return UsageThreadOutcome::GlobalPlanChanged { retry_thread: true };
                }
                UsagePlanAction::ReadFrom
                | UsagePlanAction::BuildFrom
                | UsagePlanAction::LocalReplay
                | UsagePlanAction::ResumeOwningLive
                | UsagePlanAction::VerifyRawTail => {}
            }
        }

        let mut prepared = Vec::<UsageSourceCommitDto>::new();
        let mut metrics = UsageCommitMetrics::default();
        let mut source_ids = Vec::<i64>::new();
        for plan in &group {
            if !matches!(
                plan.action,
                UsagePlanAction::ReadFrom
                    | UsagePlanAction::BuildFrom
                    | UsagePlanAction::LocalReplay
                    | UsagePlanAction::ResumeOwningLive
                    | UsagePlanAction::VerifyRawTail
            ) {
                continue;
            }
            let Some((file, generation)) = present.get(&plan.source_file_id).copied() else {
                continue;
            };
            let metadata_state = usage
                .load_metadata_scan_state(&[plan.source_file_id])
                .map_err(|_| UsageThreadOutcome::OrdinaryError("USAGE_METADATA_STATE_LOAD_FAILED"));
            let metadata_state = match metadata_state {
                Ok(state) => state,
                Err(outcome) => return outcome,
            };
            let Some(metadata_entry) = metadata_state.get(plan.source_file_id) else {
                return UsageThreadOutcome::OrdinaryError("USAGE_METADATA_STATE_MISSING");
            };
            let step = match process_source_batch(
                usage,
                &scan,
                plan,
                file,
                generation,
                metadata_entry,
                state_snapshot,
                cancellation,
                report,
            ) {
                Ok(step) => step,
                Err(error_code) => {
                    return UsageThreadOutcome::OrdinaryError(error_code);
                }
            };
            match step {
                UsageReadStep::Prepared {
                    dto,
                    metrics: source_metrics,
                } => {
                    if !group_budget_allows(&prepared, &dto) {
                        // No database side effect has happened. Commit the
                        // already prepared bounded group; this source will be
                        // reread from its still-durable checkpoint next loop.
                        if prepared.is_empty() {
                            return UsageThreadOutcome::OrdinaryError(
                                "USAGE_GROUP_BATCH_BUDGET_INVALID",
                            );
                        }
                        break;
                    }
                    let exclusive = dto_requires_exclusive_batch(&dto);
                    metrics.add(&source_metrics);
                    source_ids.push(dto.source_file_id);
                    prepared.push(*dto);
                    if exclusive || group_budget_full(&prepared) {
                        break;
                    }
                }
                UsageReadStep::AwaitingOwnership => {
                    // The replay prefix remains memory-only until owning evidence
                    // is found. Awaiting ownership is a valid transient state, even
                    // while a shadow build is open, and is never quarantined by
                    // itself.
                    return UsageThreadOutcome::Completed;
                }
                UsageReadStep::InvalidSessionData => {
                    if !discovery_complete {
                        return UsageThreadOutcome::Completed;
                    }
                    // A contradiction against durable metadata proof gets one
                    // normal rebuild opportunity. If the same contradiction is
                    // observed from BuildFrom, the fixed Session Tree is bad data
                    // and can be isolated without masking storage/system failures.
                    if let Some(error_code) =
                        shadow_session_data_error(to_pipeline_action(plan.action), true)
                    {
                        return UsageThreadOutcome::SessionDataError(error_code);
                    }
                    if let Err(error_code) =
                        replace_or_begin(usage, &scan, present_ids, [plan.source_file_id])
                    {
                        return UsageThreadOutcome::OrdinaryError(error_code);
                    }
                    return UsageThreadOutcome::GlobalPlanChanged { retry_thread: true };
                }
                UsageReadStep::NeedsRebuild => {
                    if !discovery_complete {
                        return UsageThreadOutcome::Completed;
                    }
                    let already_rebuilding = plan.action == UsagePlanAction::BuildFrom;
                    if let Err(error_code) =
                        replace_or_begin(usage, &scan, present_ids, [plan.source_file_id])
                    {
                        return UsageThreadOutcome::OrdinaryError(error_code);
                    }
                    // Generic rebuild requests include legitimate metadata/owner
                    // transitions such as late foreign-meta discovery. Replacing
                    // the shadow source is durable; an existing BuildFrom stops
                    // this scan rather than being misclassified as bad Session data.
                    if already_rebuilding {
                        return UsageThreadOutcome::GlobalPlanChanged {
                            retry_thread: false,
                        };
                    }
                    return UsageThreadOutcome::GlobalPlanChanged { retry_thread: true };
                }
                UsageReadStep::NeedsRebuildStop => {
                    if !discovery_complete {
                        return UsageThreadOutcome::Completed;
                    }
                    if let Err(error_code) =
                        replace_or_begin(usage, &scan, present_ids, [plan.source_file_id])
                    {
                        return UsageThreadOutcome::OrdinaryError(error_code);
                    }
                    // The source changed while this fixed view was being read.
                    // The replacement is durable, but this stale discovery view
                    // must not be reused for a from-zero read in this round.
                    return UsageThreadOutcome::GlobalPlanChanged {
                        retry_thread: false,
                    };
                }
            }
        }

        if prepared.is_empty() {
            return UsageThreadOutcome::Completed;
        }
        let write_started = Instant::now();
        match usage_commit::commit_group(usage, prepared) {
            Ok(outcome) => {
                report.observe_usage_commit(&metrics, &outcome, write_started.elapsed());
                if cancelled(cancellation) {
                    return UsageThreadOutcome::Completed;
                }
                scan = match load_exact_state(
                    usage,
                    &work_thread.source_file_ids,
                    expected_epoch.clone(),
                    report,
                ) {
                    Ok(scan) => scan,
                    Err(error_code) => return UsageThreadOutcome::FatalReloadError(error_code),
                };
            }
            Err(error) => {
                if error.requires_rebuild() && discovery_complete {
                    if let Err(error_code) = replace_or_begin(usage, &scan, present_ids, source_ids)
                    {
                        return UsageThreadOutcome::OrdinaryError(error_code);
                    }
                    return UsageThreadOutcome::GlobalPlanChanged {
                        retry_thread: false,
                    };
                }
                return UsageThreadOutcome::OrdinaryError("USAGE_GROUP_COMMIT_FAILED");
            }
        }
    }
}

fn load_work_list(
    usage: &CodexStorage<'_>,
    present_ids: &[i64],
    report: &mut ScanReport,
    reload: bool,
) -> Result<UsageWorkList, &'static str> {
    let started = Instant::now();
    let result = usage.load_usage_work_list(present_ids, USAGE_PARSER_VERSION);
    let worklist = match result {
        Ok(worklist) => worklist,
        Err(_) => {
            return Err(if reload {
                "USAGE_WORKLIST_RELOAD_FAILED"
            } else {
                "USAGE_WORKLIST_LOAD_FAILED"
            });
        }
    };
    let candidates = worklist
        .threads
        .iter()
        .map(|thread| thread.source_file_ids.len())
        .sum();
    report.observe_usage_worklist_load(candidates, started.elapsed());
    Ok(worklist)
}

fn load_exact_state(
    usage: &CodexStorage<'_>,
    source_ids: &[i64],
    expected_epoch: crate::domain::SourceUsageEpochState,
    report: &mut ScanReport,
) -> Result<UsageScanState, &'static str> {
    let started = Instant::now();
    let result =
        usage.load_usage_scan_state_exact(source_ids, USAGE_PARSER_VERSION, expected_epoch);
    report.observe_usage_detail_plan_load(source_ids, started.elapsed());
    result.map_err(|_| "USAGE_PLAN_RELOAD_FAILED")
}

fn replace_or_begin(
    usage: &CodexStorage<'_>,
    scan: &UsageScanState,
    present_ids: &[i64],
    invalidated: impl IntoIterator<Item = i64>,
) -> Result<(), &'static str> {
    let invalidated = invalidated.into_iter().collect::<Vec<_>>();
    if scan.epoch.build_epoch.is_some() {
        usage
            .replace_build_sources(USAGE_PARSER_VERSION, present_ids, &invalidated, now_ms())
            .map_err(|_| "USAGE_REBUILD_REPLACE_FAILED")
    } else {
        usage
            .begin_rebuild(USAGE_PARSER_VERSION, present_ids, now_ms())
            .map(|_| ())
            .map_err(|_| "USAGE_REBUILD_BEGIN_FAILED")
    }
}

fn dto_adapter_counts(dto: &UsageSourceCommitDto) -> (u64, u64, u64) {
    (
        dto.source_bytes_consumed
            .saturating_sub(dto.replayed_prefix_bytes),
        dto.complete_line_count
            .saturating_sub(dto.replayed_prefix_lines),
        dto.candidate_count,
    )
}

fn dto_requires_exclusive_batch(dto: &UsageSourceCommitDto) -> bool {
    let (bytes, lines, _) = dto_adapter_counts(dto);
    lines == 1 && bytes > MAX_BATCH_BYTES
}

fn group_budget_allows(existing: &[UsageSourceCommitDto], next: &UsageSourceCommitDto) -> bool {
    if dto_requires_exclusive_batch(next) {
        return existing.is_empty();
    }
    if existing.iter().any(dto_requires_exclusive_batch) {
        return false;
    }
    let (mut bytes, mut lines, mut candidates) = (0u64, 0u64, 0u64);
    for dto in existing.iter().chain(std::iter::once(next)) {
        let counts = dto_adapter_counts(dto);
        bytes = bytes.saturating_add(counts.0);
        lines = lines.saturating_add(counts.1);
        candidates = candidates.saturating_add(counts.2);
    }
    bytes <= MAX_BATCH_BYTES && lines <= MAX_BATCH_LINES && candidates <= MAX_BATCH_CANDIDATES
}

fn group_budget_full(dtos: &[UsageSourceCommitDto]) -> bool {
    let (mut bytes, mut lines, mut candidates) = (0u64, 0u64, 0u64);
    for dto in dtos {
        let counts = dto_adapter_counts(dto);
        bytes = bytes.saturating_add(counts.0);
        lines = lines.saturating_add(counts.1);
        candidates = candidates.saturating_add(counts.2);
    }
    bytes >= MAX_BATCH_BYTES || lines >= MAX_BATCH_LINES || candidates >= MAX_BATCH_CANDIDATES
}

#[expect(
    clippy::too_many_arguments,
    reason = "preserve the established scanner source processing seam"
)]
fn process_source_batch(
    usage: &CodexStorage<'_>,
    scan: &UsageScanState,
    source: &UsageSourceScanPlan,
    file: &DiscoveredFile,
    file_generation: i64,
    metadata_entry: &MetadataScanStateEntry,
    state_snapshot: &StateSnapshot,
    cancellation: &AtomicBool,
    report: &mut ScanReport,
) -> Result<UsageReadStep, &'static str> {
    let fixed_observed_size = u64::try_from(file.size).map_err(|_| "USAGE_SOURCE_SIZE_INVALID")?;
    let identity = PhysicalIdentity {
        device_id: u64::try_from(file.device_id).map_err(|_| "USAGE_SOURCE_IDENTITY_INVALID")?,
        inode: u64::try_from(file.inode).map_err(|_| "USAGE_SOURCE_IDENTITY_INVALID")?,
    };

    let initial_start =
        u64::try_from(source.start_offset).map_err(|_| "USAGE_SOURCE_OFFSET_INVALID")?;
    let initial_guard = checkpoint_guard(source)?;
    let (resume_state, existing_fact) = if initial_start == 0 {
        (ResumeState::AwaitOwningMeta, None)
    } else {
        let Some(owning_thread_id) = source.owning_thread_id.clone() else {
            return Ok(UsageReadStep::NeedsRebuild);
        };
        let Some(usage_state) = source.state.as_ref() else {
            return Ok(UsageReadStep::NeedsRebuild);
        };
        let SafeFactState::Matching(fact) = &metadata_entry.safe_fact else {
            return Ok(UsageReadStep::NeedsRebuild);
        };
        let fact =
            RolloutThreadFact::from_safe_fact(fact).map_err(|_| "USAGE_SAFE_FACT_INVALID")?;
        let resume = match usage_state.continuation_state {
            UsageContinuationState::ReplayedAncestor => {
                ResumeState::ReplayedAncestor { owning_thread_id }
            }
            UsageContinuationState::OwningLive => ResumeState::OwningLive { owning_thread_id },
        };
        (resume, Some(fact))
    };
    let mut parser = RolloutMetadataParser::start_chunk(RolloutParseContext {
        source_file_id: source.source_file_id,
        chunk_start_offset: initial_start,
        candidates: owning_candidates(file, state_snapshot),
        resume_state,
        existing_fact,
    });

    let establishing = initial_start == 0 && source.state.is_none();
    let metadata_replay_tail = matches!(
        &metadata_entry.safe_fact,
        SafeFactState::Matching(fact)
            if fact.continuation_state == crate::codex::domain::ContinuationState::ReplayedAncestor
    );
    let allow_replay_tail = metadata_replay_tail
        || source.state.as_ref().is_some_and(|state| {
            matches!(
                state.continuation_state,
                UsageContinuationState::ReplayedAncestor
            )
        });
    // Metadata has already parsed this exact fixed view.  At offset 0 we still
    // replay the shared ownership classifier ourselves, but the durable safe
    // fact tells us which classifier boundary must be re-observed before a
    // nonzero usage checkpoint is legal.  This matters for subagent rollouts:
    // their own session_meta may precede an embedded ancestor replay, while
    // owning_records_start_offset points at the first stable OwningLive record.
    let owning_boundary_offset = if establishing {
        match &metadata_entry.safe_fact {
            SafeFactState::Matching(fact) => fact
                .owning_records_start_offset
                .map(|value| u64::try_from(value).map_err(|_| "USAGE_OWNERSHIP_BOUNDARY_INVALID"))
                .transpose()?,
            _ => None,
        }
    } else {
        None
    };
    let mut cursor = initial_start;
    let mut expected_guard = initial_guard;
    let mut replayed_prefix_bytes = 0u64;
    let mut replayed_prefix_lines = 0u64;
    let mut ownership_established = initial_start > 0;

    loop {
        if cancellation.load(Ordering::Acquire) {
            return Ok(UsageReadStep::AwaitingOwnership);
        }
        let mut retained = Vec::<ClassifiedUsageItem>::new();
        let mut adapter_bytes = 0u64;
        let mut adapter_lines = 0u64;
        let mut potential_candidates = 0u64;
        let mut replay_window_bytes = 0u64;
        let mut replay_window_lines = 0u64;
        let mut unknown_ownership = false;
        let mut invalid_session_data = false;
        let mut token_records_seen = 0u64;
        let mut saw_owning_boundary = ownership_established;
        let parsing_started = Instant::now();
        let chunk = read_chunk_bounded(
            &ChunkReadPlan {
                path: file.path.clone(),
                identity,
                start_offset: cursor,
                observed_size: fixed_observed_size,
                expected_guard,
            },
            |framed| {
                let item = match classify_framed(&mut parser, framed) {
                    Some(item) => item,
                    None => {
                        unknown_ownership = true;
                        return ReadControl::StopAfter;
                    }
                };
                let start = item_start(&item);
                let end = item_end(&item);
                let bytes = end.saturating_sub(start);
                let classification = item_classification(&item);
                if classification.envelope == EnvelopeKind::TokenCount {
                    token_records_seen = token_records_seen.saturating_add(1);
                }

                if !saw_owning_boundary {
                    if let Some(boundary) = owning_boundary_offset {
                        if start < boundary {
                            // Everything before the already-confirmed stable
                            // owning boundary is an ownership-establish prefix.
                            // It advances only the in-memory classifier and is
                            // deliberately excluded from adapter/event arrays.
                            replayed_prefix_bytes = replayed_prefix_bytes.saturating_add(bytes);
                            replayed_prefix_lines = replayed_prefix_lines.saturating_add(1);
                            replay_window_bytes = replay_window_bytes.saturating_add(bytes);
                            replay_window_lines = replay_window_lines.saturating_add(1);
                            if bytes > MAX_BATCH_BYTES
                                || replay_window_bytes >= REPLAY_WINDOW_BYTES
                                || replay_window_lines >= REPLAY_WINDOW_LINES
                            {
                                return ReadControl::StopAfter;
                            }
                            return ReadControl::Continue;
                        }
                        if start > boundary || classification.ownership != RecordOwnership::Owning {
                            unknown_ownership = true;
                            invalid_session_data = true;
                            return ReadControl::StopAfter;
                        }
                        // The shared classifier independently re-observed the
                        // stable boundary promised by the metadata safe fact.
                        saw_owning_boundary = true;
                        ownership_established = true;
                    } else {
                        match classification.ownership {
                            RecordOwnership::ReplayedAncestor => {
                                replayed_prefix_bytes = replayed_prefix_bytes.saturating_add(bytes);
                                replayed_prefix_lines = replayed_prefix_lines.saturating_add(1);
                                replay_window_bytes = replay_window_bytes.saturating_add(bytes);
                                replay_window_lines = replay_window_lines.saturating_add(1);
                                if bytes > MAX_BATCH_BYTES
                                    || replay_window_bytes >= REPLAY_WINDOW_BYTES
                                    || replay_window_lines >= REPLAY_WINDOW_LINES
                                {
                                    return ReadControl::StopAfter;
                                }
                                return ReadControl::Continue;
                            }
                            RecordOwnership::UnknownOwnership => {
                                unknown_ownership = true;
                                return ReadControl::StopAfter;
                            }
                            RecordOwnership::Owning => {
                                // Fallback for a source with no persisted
                                // boundary yet: only session_meta may establish
                                // the first durable ownership checkpoint.
                                if classification.envelope != EnvelopeKind::SessionMeta {
                                    unknown_ownership = true;
                                    return ReadControl::StopAfter;
                                }
                                saw_owning_boundary = true;
                                ownership_established = true;
                            }
                        }
                    }
                } else if classification.ownership != RecordOwnership::Owning {
                    match classification.ownership {
                        RecordOwnership::ReplayedAncestor if allow_replay_tail => {
                            replay_window_bytes = replay_window_bytes.saturating_add(bytes);
                            replay_window_lines = replay_window_lines.saturating_add(1);
                            retained.push(item);
                            if bytes > MAX_BATCH_BYTES
                                || replay_window_bytes >= REPLAY_WINDOW_BYTES
                                || replay_window_lines >= REPLAY_WINDOW_LINES
                            {
                                return ReadControl::StopAfter;
                            }
                            return ReadControl::Continue;
                        }
                        _ => {
                            retained.push(item);
                            return ReadControl::StopAfter;
                        }
                    }
                }

                let candidate = matches!(
                    classification.envelope,
                    EnvelopeKind::TokenCount | EnvelopeKind::Lifecycle
                );
                let oversized = matches!(item, ClassifiedUsageItem::Oversized(_));
                let would_candidates = potential_candidates.saturating_add(u64::from(candidate));
                let fits = if adapter_lines == 0 {
                    (oversized || bytes <= MAX_LEGAL_LINE_BYTES)
                        && would_candidates <= MAX_BATCH_CANDIDATES
                } else {
                    !oversized
                        && adapter_bytes.saturating_add(bytes) <= MAX_BATCH_BYTES
                        && adapter_lines < MAX_BATCH_LINES
                        && would_candidates <= MAX_BATCH_CANDIDATES
                };
                if !fits {
                    return ReadControl::StopBefore;
                }
                adapter_bytes = adapter_bytes.saturating_add(bytes);
                adapter_lines += 1;
                potential_candidates = would_candidates;
                retained.push(item);

                if oversized
                    || bytes > MAX_BATCH_BYTES
                    || adapter_bytes >= MAX_BATCH_BYTES
                    || adapter_lines >= MAX_BATCH_LINES
                    || potential_candidates >= MAX_BATCH_CANDIDATES
                    || (establishing && ownership_established && !allow_replay_tail)
                {
                    ReadControl::StopAfter
                } else {
                    ReadControl::Continue
                }
            },
        );
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(
                ChunkReadError::CheckpointGuardMismatch
                | ChunkReadError::SourceChangedBeforeRead
                | ChunkReadError::SourceChangedDuringRead,
            ) => return Ok(UsageReadStep::NeedsRebuildStop),
            Err(error) => return Err(read_error_code(error)),
        };
        report.observe_usage_read(&chunk, token_records_seen, parsing_started.elapsed());

        if unknown_ownership {
            return Ok(if invalid_session_data {
                UsageReadStep::InvalidSessionData
            } else if establishing {
                UsageReadStep::AwaitingOwnership
            } else {
                UsageReadStep::NeedsRebuild
            });
        }

        cursor = chunk.last_complete_offset;
        expected_guard = chunk.guard;
        if retained.is_empty() && !ownership_established {
            if chunk.fixed_view_exhausted {
                return Ok(UsageReadStep::AwaitingOwnership);
            }
            // A bounded replay-only window has no durable side effects. Keep
            // only classifier state/counters and continue from its guard.
            continue;
        }

        let pipeline_read_start = if initial_start == 0 {
            initial_start.saturating_add(replayed_prefix_bytes)
        } else {
            initial_start
        };
        let mut pipeline_plan = pipeline_plan(
            scan,
            source,
            file_generation,
            file.device_id,
            file.inode,
            fixed_observed_size,
            pipeline_read_start,
            replayed_prefix_bytes,
            replayed_prefix_lines,
        )
        .map_err(|_| "USAGE_PIPELINE_PLAN_FAILED")?;
        pipeline_plan.allow_replay_tail = allow_replay_tail;
        let tail = tail_from_read(&chunk);
        let guard = chunk.guard.map(|hash| hash.as_bytes().to_vec());
        let disposition =
            UsagePipeline::process_chunk(pipeline_plan, retained, tail, guard, false, now_ms())
                .map_err(|_| "USAGE_PIPELINE_FAILED")?;
        match disposition {
            PipelineDisposition::Commit(dto) => {
                let metrics = UsageCommitMetrics::from_dto(&dto);
                return Ok(UsageReadStep::Prepared {
                    dto: Box::new(dto),
                    metrics,
                });
            }
            PipelineDisposition::AwaitingOwningMeta => {
                if chunk.fixed_view_exhausted {
                    return Ok(UsageReadStep::AwaitingOwnership);
                }
                if initial_start == 0 {
                    continue;
                }
                return Ok(UsageReadStep::NeedsRebuild);
            }
            PipelineDisposition::NeedsRebuild => return Ok(UsageReadStep::NeedsRebuild),
            PipelineDisposition::Skip | PipelineDisposition::BlockedRelationship => {
                return Ok(UsageReadStep::AwaitingOwnership);
            }
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "preserve the established source planning seam"
)]
fn pipeline_plan(
    scan: &UsageScanState,
    source: &UsageSourceScanPlan,
    file_generation: i64,
    device_id: i64,
    inode: i64,
    fixed_observed_size: u64,
    read_start_offset: u64,
    replayed_prefix_bytes_before_chunk: u64,
    replayed_prefix_lines_before_chunk: u64,
) -> Result<super::usage_pipeline::UsagePipelinePlan, &'static str> {
    let checkpoint = match source.checkpoint.as_ref() {
        Some(checkpoint) => pipeline_checkpoint(checkpoint)?,
        None => super::usage_pipeline::CheckpointExpectation {
            parser_version: scan.epoch.working_parser_version(),
            committed_offset: 0,
            guard_hash: None,
            status: super::usage_pipeline::CheckpointStatus::Ready,
        },
    };
    let state = source
        .state
        .as_ref()
        .map(|state| pipeline_state(state, source.open_turn.as_ref()))
        .transpose()?;
    let start_offset =
        u64::try_from(source.start_offset).map_err(|_| "USAGE_SOURCE_OFFSET_INVALID")?;
    Ok(super::usage_pipeline::UsagePipelinePlan {
        ledger_epoch: scan.epoch.working_epoch(),
        parser_version: scan.epoch.working_parser_version(),
        source_file_id: source.source_file_id,
        file_generation,
        device_id,
        inode,
        action: to_pipeline_action(source.action),
        start_offset,
        read_start_offset,
        fixed_observed_size,
        owning_thread_id: source.owning_thread_id.clone(),
        root_session_id: source.root_session_id.clone(),
        checkpoint,
        state,
        allow_replay_tail: false,
        replayed_prefix_bytes_before_chunk,
        replayed_prefix_lines_before_chunk,
    })
}

fn pipeline_checkpoint(
    checkpoint: &UsageCheckpointExpectation,
) -> Result<super::usage_pipeline::CheckpointExpectation, &'static str> {
    Ok(super::usage_pipeline::CheckpointExpectation {
        parser_version: checkpoint.parser_version,
        committed_offset: u64::try_from(checkpoint.committed_offset)
            .map_err(|_| "USAGE_CHECKPOINT_OFFSET_INVALID")?,
        guard_hash: checkpoint.guard_hash.clone(),
        status: match checkpoint.processing_status {
            crate::codex::domain::CheckpointProcessingStatus::Pending => {
                super::usage_pipeline::CheckpointStatus::Pending
            }
            crate::codex::domain::CheckpointProcessingStatus::Ready => {
                super::usage_pipeline::CheckpointStatus::Ready
            }
            crate::codex::domain::CheckpointProcessingStatus::Error => {
                super::usage_pipeline::CheckpointStatus::Error
            }
            crate::codex::domain::CheckpointProcessingStatus::RebuildRequired => {
                super::usage_pipeline::CheckpointStatus::RebuildRequired
            }
        },
    })
}

fn pipeline_state(
    state: &UsageSourceStateWrite,
    open_turn: Option<&crate::codex::ingestion::usage_processor::TurnState>,
) -> Result<super::usage_pipeline::SourceStateProof, &'static str> {
    let processor_state = crate::codex::ingestion::usage_processor::UsageSourceState {
        chain_state: match state.chain_state {
            UsageChainState::Continuous => {
                crate::codex::ingestion::usage_processor::ChainState::Continuous
            }
            UsageChainState::Interrupted(reason) => {
                crate::codex::ingestion::usage_processor::ChainState::Interrupted(match reason {
                    UsageGapReason::Malformed => {
                        crate::codex::ingestion::usage_processor::GapKind::Malformed
                    }
                    UsageGapReason::Oversized => {
                        crate::codex::ingestion::usage_processor::GapKind::Oversized
                    }
                    UsageGapReason::TotalInvalid => {
                        crate::codex::ingestion::usage_processor::GapKind::RequiredInvalid
                    }
                    UsageGapReason::OwnershipGap => {
                        crate::codex::ingestion::usage_processor::GapKind::Ownership
                    }
                    UsageGapReason::ParserGap => {
                        crate::codex::ingestion::usage_processor::GapKind::Parser
                    }
                })
            }
        },
        previous_total: state
            .previous_total
            .as_ref()
            .map(|snapshot| snapshot.vector.clone()),
        previous_total_offset: state
            .previous_total_offset
            .map(|offset| u64::try_from(offset).map_err(|_| "USAGE_STATE_OFFSET_INVALID"))
            .transpose()?,
        active_model: state.active_model.clone(),
        active_reasoning_effort: state.active_reasoning_effort.clone(),
        open_turn: open_turn.cloned(),
    };
    if state.active_turn_key.is_some() && processor_state.open_turn.is_none() {
        return Err("USAGE_OPEN_TURN_STATE_MISSING");
    }
    Ok(super::usage_pipeline::SourceStateProof {
        file_generation: state.file_generation,
        device_id: state.device_id,
        inode: state.inode,
        parser_version: state.usage_parser_version,
        canonical_algorithm_version: state.canonical_algorithm_version,
        resolved_through_offset: u64::try_from(state.resolved_through_offset)
            .map_err(|_| "USAGE_STATE_OFFSET_INVALID")?,
        observed_raw_size: u64::try_from(state.observed_raw_size)
            .map_err(|_| "USAGE_STATE_OFFSET_INVALID")?,
        raw_tail_status: match state.raw_tail_status {
            UsageTailStatus::Unverified => super::usage_pipeline::TailStatus::Unverified,
            UsageTailStatus::None => super::usage_pipeline::TailStatus::None,
            UsageTailStatus::HalfLine => super::usage_pipeline::TailStatus::HalfLine,
        },
        raw_tail_start_offset: state
            .raw_tail_start_offset
            .map(|offset| u64::try_from(offset).map_err(|_| "USAGE_STATE_OFFSET_INVALID"))
            .transpose()?,
        owning_thread_id: state.owning_thread_id.clone(),
        root_session_id: state.root_session_id.clone(),
        continuation_state: match state.continuation_state {
            UsageContinuationState::ReplayedAncestor => SourceContinuationState::ReplayedAncestor,
            UsageContinuationState::OwningLive => SourceContinuationState::OwningLive,
        },
        processor_state,
        active_model_offset: state
            .active_model_offset
            .map(|offset| u64::try_from(offset).map_err(|_| "USAGE_STATE_OFFSET_INVALID"))
            .transpose()?,
        active_reasoning_effort_offset: state
            .active_reasoning_effort_offset
            .map(|offset| u64::try_from(offset).map_err(|_| "USAGE_STATE_OFFSET_INVALID"))
            .transpose()?,
        updated_at_ms: state.updated_at_ms,
    })
}

fn classify_framed(
    parser: &mut crate::codex::rollout::RolloutChunkParser,
    framed: FramedItem,
) -> Option<ClassifiedUsageItem> {
    match framed {
        FramedItem::Line(line) => {
            let start = line.start_offset();
            let bytes = line.into_bytes_with_newline();
            let usage = CompleteUsageLine::new(start, bytes.clone())?;
            let rollout = CompleteRolloutLine::new(start, bytes)?;
            let classification = parser.push_classified(rollout)?;
            Some(
                ClassifiedUsageLine {
                    line: usage,
                    classification,
                }
                .into(),
            )
        }
        FramedItem::OversizedCompleteLine(diagnostic) => {
            let classification =
                parser.push_opaque_classified(diagnostic.start_offset, diagnostic.end_offset);
            Some(
                ClassifiedOversizedUsageLine {
                    start_offset: diagnostic.start_offset,
                    end_offset: diagnostic.end_offset,
                    classification,
                }
                .into(),
            )
        }
    }
}

fn checkpoint_guard(source: &UsageSourceScanPlan) -> Result<Option<GuardHash>, &'static str> {
    let Some(checkpoint) = source.checkpoint.as_ref() else {
        return Ok(None);
    };
    match (
        u64::try_from(checkpoint.committed_offset)
            .map_err(|_| "USAGE_CHECKPOINT_OFFSET_INVALID")?,
        checkpoint.guard_hash.as_deref(),
    ) {
        (0, None) => Ok(None),
        (1.., Some(bytes)) => guard_from_slice(bytes).map(Some),
        _ => Err("USAGE_CHECKPOINT_GUARD_INVALID"),
    }
}

fn verify_present_carry_prefix(
    file: &DiscoveredFile,
    source: &UsageSourceScanPlan,
    report: &mut ScanReport,
) -> Result<bool, &'static str> {
    let Some(build) = source.build.as_ref() else {
        return Ok(false);
    };
    let identity = PhysicalIdentity {
        device_id: u64::try_from(build.expected_device_id)
            .map_err(|_| "USAGE_SOURCE_IDENTITY_INVALID")?,
        inode: u64::try_from(build.expected_inode).map_err(|_| "USAGE_SOURCE_IDENTITY_INVALID")?,
    };
    let expected_guard = match (
        build.active_committed_offset,
        build.active_guard_hash.as_deref(),
    ) {
        (0, None) => None,
        (1.., Some(bytes)) => Some(guard_from_slice(bytes)?),
        _ => return Ok(false),
    };
    let started = Instant::now();
    let result = read_chunk_bounded(
        &ChunkReadPlan {
            path: file.path.clone(),
            identity,
            start_offset: u64::try_from(build.active_committed_offset)
                .map_err(|_| "USAGE_CARRY_OFFSET_INVALID")?,
            observed_size: u64::try_from(build.active_committed_offset)
                .map_err(|_| "USAGE_CARRY_OFFSET_INVALID")?,
            expected_guard,
        },
        |_| ReadControl::Continue,
    );
    match result {
        Ok(result) => {
            report.observe_usage_read(&result, 0, started.elapsed());
            Ok(true)
        }
        Err(
            ChunkReadError::CheckpointGuardMismatch
            | ChunkReadError::SourceChangedBeforeRead
            | ChunkReadError::SourceChangedDuringRead,
        ) => Ok(false),
        Err(error) => Err(read_error_code(error)),
    }
}

fn guard_from_slice(bytes: &[u8]) -> Result<GuardHash, &'static str> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "USAGE_CHECKPOINT_GUARD_INVALID")?;
    Ok(GuardHash::from_bytes(bytes))
}

fn tail_from_read(read: &super::chunk_reader::ChunkReadResult) -> FixedViewTail {
    if !read.fixed_view_exhausted {
        FixedViewTail {
            exhausted: false,
            status: TailStatus::Unverified,
            half_line_start: None,
        }
    } else if read.has_half_line {
        FixedViewTail {
            exhausted: true,
            status: TailStatus::HalfLine,
            half_line_start: Some(read.last_complete_offset),
        }
    } else {
        FixedViewTail {
            exhausted: true,
            status: TailStatus::None,
            half_line_start: None,
        }
    }
}

fn item_start(item: &ClassifiedUsageItem) -> u64 {
    match item {
        ClassifiedUsageItem::Line(line) => line.line.start_offset(),
        ClassifiedUsageItem::Oversized(line) => line.start_offset,
    }
}

fn item_end(item: &ClassifiedUsageItem) -> u64 {
    match item {
        ClassifiedUsageItem::Line(line) => line.line.end_offset(),
        ClassifiedUsageItem::Oversized(line) => line.end_offset,
    }
}

fn item_classification(item: &ClassifiedUsageItem) -> &crate::codex::RecordClassification {
    match item {
        ClassifiedUsageItem::Line(line) => &line.classification,
        ClassifiedUsageItem::Oversized(line) => &line.classification,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct UsageCommitMetrics {
    pub normal_events: u64,
    pub recovered_events: u64,
    pub compensation_events: u64,
    pub anomalies: u64,
}

impl UsageCommitMetrics {
    fn add(&mut self, other: &Self) {
        self.normal_events = self.normal_events.saturating_add(other.normal_events);
        self.recovered_events = self.recovered_events.saturating_add(other.recovered_events);
        self.compensation_events = self
            .compensation_events
            .saturating_add(other.compensation_events);
        self.anomalies = self.anomalies.saturating_add(other.anomalies);
    }

    fn from_dto(dto: &UsageSourceCommitDto) -> Self {
        let mut value = Self {
            normal_events: 0,
            recovered_events: 0,
            compensation_events: 0,
            anomalies: dto.anomalies.len() as u64,
        };
        for event in &dto.events {
            match event.kind {
                EventKind::Normal => value.normal_events += 1,
                EventKind::Recovered => value.recovered_events += 1,
                EventKind::TurnCompensation => value.compensation_events += 1,
            }
        }
        value
    }
}

fn now_ms() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod resilience_tests {
    use super::*;

    #[test]
    fn repeated_shadow_rebuild_data_failure_is_session_scoped_only() {
        assert_eq!(
            shadow_session_data_error(PlanAction::BuildFrom, true),
            Some("USAGE_SESSION_DATA_INVALID")
        );
        assert_eq!(
            shadow_session_data_error(PlanAction::BuildFrom, false),
            None
        );
        for action in [
            PlanAction::ReadFrom,
            PlanAction::LocalReplay,
            PlanAction::AwaitOwningMeta,
            PlanAction::ResumeOwningLive,
            PlanAction::VerifyRawTail,
            PlanAction::CompleteOnly,
            PlanAction::BeginCarry,
            PlanAction::ResumeCarry,
            PlanAction::Skip,
            PlanAction::BlockedRelationship,
            PlanAction::RebuildRequired,
        ] {
            assert_eq!(shadow_session_data_error(action, true), None, "{action:?}");
        }
    }
}
