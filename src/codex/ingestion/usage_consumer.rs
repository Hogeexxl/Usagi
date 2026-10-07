//! Spec 04 usage consumer integrated into the fixed Spec 03 scan round.
//!
//! This module never enumerates rollout files on its own. It consumes the
//! discovery/source-observation snapshot already owned by the scanner, runs the
//! metadata ownership classifier in streaming mode, and commits each bounded
//! usage batch before continuing from the durable checkpoint.

use std::{
    cell::OnceCell,
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
    UsageSourceCommitDto, reconciliation_request,
};
use super::usage_processor::{Anomaly, AnomalyCode, ReconciliationCarry};
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
const MAX_BATCH_WRITE_UNITS: u64 = super::usage_pipeline::MAX_BATCH_WRITE_UNITS;
const MAX_LEGAL_LINE_BYTES: u64 = super::usage_pipeline::MAX_LEGAL_LINE_BYTES;
const CLEANUP_ROWS_PER_ROUND: usize = 2048;
const PATCH_TOO_LARGE_ERROR_CODE: &str = "USAGE_RECONCILIATION_PATCH_TOO_LARGE";

#[derive(Debug)]
enum UsageReadStep {
    Prepared {
        dto: Box<UsageSourceCommitDto>,
        metrics: UsageCommitMetrics,
    },
    AwaitingOwnership,
    InvalidSessionData,
    PatchTooLarge {
        anomaly: Anomaly,
    },
    FatalAnomaly {
        anomaly: Anomaly,
    },
    Replan,
    NeedsRebuild,
    NeedsRebuildStop,
}

#[derive(Debug)]
enum UsageThreadOutcome {
    Completed,
    GlobalPlanChanged {
        retry_thread: bool,
    },
    SessionDataError {
        code: &'static str,
        quarantine_diagnostic: Option<crate::codex::storage::usage::UsageQuarantineDiagnostic>,
    },
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

fn patch_too_large_build_failure(
    anomaly: Anomaly,
    source_file_id: i64,
    file_generation: i64,
    owning_thread_id: &str,
    detected_at_ms: i64,
) -> UsageThreadOutcome {
    typed_anomaly_build_failure(
        anomaly,
        PATCH_TOO_LARGE_ERROR_CODE,
        source_file_id,
        file_generation,
        owning_thread_id,
        detected_at_ms,
    )
}

fn typed_anomaly_build_failure(
    anomaly: Anomaly,
    code: &'static str,
    source_file_id: i64,
    file_generation: i64,
    owning_thread_id: &str,
    detected_at_ms: i64,
) -> UsageThreadOutcome {
    let anomaly = match usage_commit::anomaly_write_for(
        &anomaly,
        source_file_id,
        file_generation,
        owning_thread_id,
        detected_at_ms,
    ) {
        Ok(anomaly) => anomaly,
        Err(_) => {
            return UsageThreadOutcome::OrdinaryError("USAGE_RECONCILIATION_DIAGNOSTIC_INVALID");
        }
    };
    UsageThreadOutcome::SessionDataError {
        code,
        quarantine_diagnostic: Some(crate::codex::storage::usage::UsageQuarantineDiagnostic {
            source_file_id,
            file_generation,
            owning_thread_id: owning_thread_id.to_owned(),
            anomaly,
        }),
    }
}

fn fatal_anomaly_error_code(code: AnomalyCode) -> Option<&'static str> {
    match code {
        AnomalyCode::ArithmeticOverflow => Some("USAGE_TOKEN_ARITHMETIC_OVERFLOW"),
        AnomalyCode::ReconciliationPatchTooLarge => Some(PATCH_TOO_LARGE_ERROR_CODE),
        AnomalyCode::ResponseUsageConflict => Some("USAGE_RESPONSE_USAGE_CONFLICT"),
        AnomalyCode::ResponseOwnershipMismatch => Some("USAGE_RESPONSE_OWNERSHIP_MISMATCH"),
        AnomalyCode::CompactionIdentityMismatch => Some("USAGE_COMPACTION_IDENTITY_MISMATCH"),
        AnomalyCode::LegacyCoverageAmbiguous => Some("USAGE_LEGACY_COVERAGE_AMBIGUOUS"),
        _ => None,
    }
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
                UsageThreadOutcome::SessionDataError {
                    code: error_code,
                    quarantine_diagnostic,
                } => {
                    report.failed_source();
                    report.error(error_code);
                    if worklist.epoch.build_epoch.is_none() {
                        return Err(error_code);
                    }
                    if usage
                        .quarantine_thread(
                            &work_thread.thread_id,
                            error_code,
                            quarantine_diagnostic,
                            now_ms(),
                        )
                        .is_err()
                    {
                        return Err("USAGE_SESSION_QUARANTINE_FAILED");
                    }
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

    let mut stale_context_replan_used = false;
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
                UsageThreadOutcome::SessionDataError {
                    code: "USAGE_SESSION_RELATIONSHIP_INVALID",
                    quarantine_diagnostic: None,
                }
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
                        return UsageThreadOutcome::SessionDataError {
                            code: error_code,
                            quarantine_diagnostic: None,
                        };
                    }
                    if let Err(error_code) =
                        replace_or_begin(usage, &scan, present_ids, [plan.source_file_id])
                    {
                        return UsageThreadOutcome::OrdinaryError(error_code);
                    }
                    return UsageThreadOutcome::GlobalPlanChanged { retry_thread: true };
                }
                UsageReadStep::PatchTooLarge { anomaly } => {
                    report.error(PATCH_TOO_LARGE_ERROR_CODE);
                    if !discovery_complete {
                        return UsageThreadOutcome::Completed;
                    }
                    if plan.action == UsagePlanAction::BuildFrom {
                        return patch_too_large_build_failure(
                            anomaly,
                            plan.source_file_id,
                            generation,
                            &work_thread.thread_id,
                            now_ms(),
                        );
                    }
                    if let Err(error_code) =
                        replace_or_begin(usage, &scan, present_ids, [plan.source_file_id])
                    {
                        return UsageThreadOutcome::OrdinaryError(error_code);
                    }
                    return UsageThreadOutcome::GlobalPlanChanged { retry_thread: true };
                }
                UsageReadStep::FatalAnomaly { anomaly } => {
                    let Some(error_code) = fatal_anomaly_error_code(anomaly.code) else {
                        return UsageThreadOutcome::OrdinaryError(
                            "USAGE_FATAL_ANOMALY_CODE_INVALID",
                        );
                    };
                    report.error(error_code);
                    if !discovery_complete {
                        return UsageThreadOutcome::Completed;
                    }
                    if plan.action == UsagePlanAction::BuildFrom {
                        return typed_anomaly_build_failure(
                            anomaly,
                            error_code,
                            plan.source_file_id,
                            generation,
                            &work_thread.thread_id,
                            now_ms(),
                        );
                    }
                    if let Err(error_code) =
                        replace_or_begin(usage, &scan, present_ids, [plan.source_file_id])
                    {
                        return UsageThreadOutcome::OrdinaryError(error_code);
                    }
                    return UsageThreadOutcome::GlobalPlanChanged { retry_thread: true };
                }
                UsageReadStep::Replan => {
                    if stale_context_replan_used {
                        return UsageThreadOutcome::OrdinaryError(
                            "USAGE_RECONCILIATION_CONTEXT_REPEATEDLY_STALE",
                        );
                    }
                    stale_context_replan_used = true;
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
                stale_context_replan_used = false;
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

fn dto_adapter_counts(dto: &UsageSourceCommitDto) -> Option<(u64, u64, u64)> {
    Some((
        dto.source_bytes_consumed
            .checked_sub(dto.replayed_prefix_bytes)?,
        dto.complete_line_count
            .checked_sub(dto.replayed_prefix_lines)?,
        dto.write_unit_count,
    ))
}

fn dto_requires_exclusive_batch(dto: &UsageSourceCommitDto) -> bool {
    dto_adapter_counts(dto).is_some_and(|(bytes, lines, _)| lines == 1 && bytes > MAX_BATCH_BYTES)
}

fn group_budget_allows(existing: &[UsageSourceCommitDto], next: &UsageSourceCommitDto) -> bool {
    if dto_requires_exclusive_batch(next) {
        return existing.is_empty();
    }
    if existing.iter().any(dto_requires_exclusive_batch) {
        return false;
    }
    let (mut bytes, mut lines, mut write_units) = (0u64, 0u64, 0u64);
    for dto in existing.iter().chain(std::iter::once(next)) {
        let Some(counts) = dto_adapter_counts(dto) else {
            return false;
        };
        let Some(next_bytes) = bytes.checked_add(counts.0) else {
            return false;
        };
        let Some(next_lines) = lines.checked_add(counts.1) else {
            return false;
        };
        let Some(next_write_units) = write_units.checked_add(counts.2) else {
            return false;
        };
        bytes = next_bytes;
        lines = next_lines;
        write_units = next_write_units;
    }
    bytes <= MAX_BATCH_BYTES && lines <= MAX_BATCH_LINES && write_units <= MAX_BATCH_WRITE_UNITS
}

fn group_budget_full(dtos: &[UsageSourceCommitDto]) -> bool {
    let (mut bytes, mut lines, mut write_units) = (0u64, 0u64, 0u64);
    for dto in dtos {
        let Some(counts) = dto_adapter_counts(dto) else {
            return true;
        };
        let Some(next_bytes) = bytes.checked_add(counts.0) else {
            return true;
        };
        let Some(next_lines) = lines.checked_add(counts.1) else {
            return true;
        };
        let Some(next_write_units) = write_units.checked_add(counts.2) else {
            return true;
        };
        bytes = next_bytes;
        lines = next_lines;
        write_units = next_write_units;
    }
    bytes >= MAX_BATCH_BYTES || lines >= MAX_BATCH_LINES || write_units >= MAX_BATCH_WRITE_UNITS
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
        let mut replay_window_bytes = 0u64;
        let mut replay_window_lines = 0u64;
        let mut unknown_ownership = false;
        let mut invalid_session_data = false;
        let mut response_ownership_mismatch_offset = None;
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
                if classification.response_ownership_mismatch {
                    response_ownership_mismatch_offset.get_or_insert(start);
                }
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

                let oversized = matches!(item, ClassifiedUsageItem::Oversized(_));
                let fits = if adapter_lines == 0 {
                    oversized || bytes <= MAX_LEGAL_LINE_BYTES
                } else {
                    !oversized
                        && adapter_bytes
                            .checked_add(bytes)
                            .is_some_and(|total| total <= MAX_BATCH_BYTES)
                        && adapter_lines < MAX_BATCH_LINES
                };
                if !fits {
                    return ReadControl::StopBefore;
                }
                adapter_bytes += bytes;
                adapter_lines += 1;
                retained.push(item);

                if oversized
                    || bytes > MAX_BATCH_BYTES
                    || adapter_bytes >= MAX_BATCH_BYTES
                    || adapter_lines >= MAX_BATCH_LINES
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
                error @ (ChunkReadError::CheckpointGuardMismatch
                | ChunkReadError::SourceChangedBeforeRead
                | ChunkReadError::SourceChangedDuringRead),
            ) => {
                return Ok(UsageReadStep::NeedsRebuildStop);
            }
            Err(error) => return Err(read_error_code(error)),
        };
        report.observe_usage_read(&chunk, token_records_seen, parsing_started.elapsed());

        if unknown_ownership {
            if let Some(offset) = response_ownership_mismatch_offset {
                return Ok(UsageReadStep::FatalAnomaly {
                    anomaly: Anomaly {
                        code: AnomalyCode::ResponseOwnershipMismatch,
                        source_start_offset: Some(offset),
                        turn_key: None,
                    },
                });
            }
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
        let mut pipeline_plan = match pipeline_plan(
            scan,
            source,
            file_generation,
            file.device_id,
            file.inode,
            fixed_observed_size,
            pipeline_read_start,
            replayed_prefix_bytes,
            replayed_prefix_lines,
        ) {
            Ok(plan) => plan,
            Err("USAGE_RECONCILIATION_CARRY_INVALID") => {
                return Ok(UsageReadStep::NeedsRebuild);
            }
            Err(_) => return Err("USAGE_PIPELINE_PLAN_FAILED"),
        };
        pipeline_plan.allow_replay_tail = allow_replay_tail;
        let (Some(owning_thread_id), Some(root_session_id)) = (
            pipeline_plan.owning_thread_id.as_deref(),
            pipeline_plan.root_session_id.as_deref(),
        ) else {
            return Ok(UsageReadStep::AwaitingOwnership);
        };
        let current_turn_key = pipeline_plan
            .state
            .as_ref()
            .and_then(|state| state.processor_state.open_turn.as_ref())
            .map(|turn| turn.turn_key.as_str());
        let empty_carry = ReconciliationCarry::default();
        let carry = pipeline_plan
            .state
            .as_ref()
            .map(|state| &state.processor_state.reconciliation_carry)
            .unwrap_or(&empty_carry);
        let request =
            match reconciliation_request(&retained, owning_thread_id, current_turn_key, carry) {
                Ok(request) => request,
                Err(anomaly) => return Ok(UsageReadStep::FatalAnomaly { anomaly }),
            };
        let reconciliation_context = match usage.load_usage_reconciliation_context(
            scan.epoch.working_epoch(),
            crate::codex::ingestion::usage_processor::UsageContext {
                source_file_id: pipeline_plan.source_file_id,
                file_generation: pipeline_plan.file_generation,
                owning_thread_id: owning_thread_id.to_owned(),
                root_session_id: root_session_id.to_owned(),
            },
            request.clone(),
            crate::codex::storage::usage::UsageReconciliationBasicProof {
                device_id: file.device_id,
                inode: file.inode,
                observed_raw_size: file.size,
                expected_checkpoint: source.checkpoint.clone(),
                expected_state: source.state.clone(),
            },
        ) {
            Ok(context) if context.context.request == request => context,
            Ok(_) => return Err("USAGE_RECONCILIATION_CONTEXT_INVALID"),
            Err(error) if error.reconciliation_plan_stale() => {
                return Ok(UsageReadStep::Replan);
            }
            Err(error) if error.requires_rebuild() => {
                return Ok(UsageReadStep::NeedsRebuildStop);
            }
            Err(_) => return Err("USAGE_RECONCILIATION_CONTEXT_LOAD_FAILED"),
        };
        if let Some(open_turn) = pipeline_plan
            .state
            .as_ref()
            .and_then(|state| state.processor_state.open_turn.as_ref())
        {
            let key = crate::codex::ingestion::usage_processor::PersistedTurnKey {
                source_file_id: pipeline_plan.source_file_id,
                file_generation: pipeline_plan.file_generation,
                turn_key: open_turn.turn_key.clone(),
            };
            let Some(affected) = reconciliation_context.context.affected_turns.get(&key) else {
                return Ok(UsageReadStep::NeedsRebuildStop);
            };
            if affected.snapshot.status
                != crate::codex::ingestion::usage_processor::PersistedTurnStatus::Open
                || affected.snapshot.state != *open_turn
            {
                return Ok(UsageReadStep::NeedsRebuildStop);
            }
        }
        let mut context = reconciliation_context.context;
        context.window_metadata = reconciliation_context
            .window_metadata
            .into_iter()
            .map(|(key, metadata)| {
                (
                    key,
                    crate::codex::ingestion::usage_processor::ReconciliationWindowMetadata {
                        source_file_id: metadata.source_file_id,
                        file_generation: metadata.file_generation,
                        source_start_offset: metadata.source_start_offset,
                        source_end_offset: metadata.source_end_offset,
                        owning_thread_id: metadata.owning_thread_id,
                        turn_key: metadata.turn_key,
                    },
                )
            })
            .collect();
        context.window_proposals = reconciliation_context
            .window_proposals
            .into_iter()
            .map(|(key, bindings)| {
                (
                    key,
                    bindings
                        .into_iter()
                        .map(|binding| {
                            crate::codex::ingestion::usage_processor::WindowProposalBinding {
                                proposal: binding.proposal,
                                fact: binding.fact,
                                occurrences: binding.occurrences,
                            }
                        })
                        .collect(),
                )
            })
            .collect();
        context.response_occurrences = reconciliation_context.response_occurrences;
        context.closure_response_keys = reconciliation_context.closure_response_keys;
        pipeline_plan.reconciliation_context = context;
        let tail = tail_from_read(&chunk);
        let guard = chunk.guard.map(|hash| hash.as_bytes().to_vec());
        let disposition =
            UsagePipeline::process_chunk(pipeline_plan, retained, tail, guard, false, now_ms())
                .map_err(|_| "USAGE_PIPELINE_FAILED")?;
        match disposition {
            PipelineDisposition::Commit(dto) => {
                let mut dto = dto;
                if let Some(anomaly) = dto
                    .patch
                    .anomalies
                    .iter()
                    .find(|anomaly| anomaly.code == AnomalyCode::ReconciliationPatchTooLarge)
                {
                    return Ok(UsageReadStep::PatchTooLarge {
                        anomaly: anomaly.clone(),
                    });
                }
                if dto.last_complete_offset < chunk.last_complete_offset {
                    let accepted_guard = match accepted_checkpoint_guard(
                        &file.path,
                        identity,
                        dto.last_complete_offset,
                    ) {
                        Ok(guard) => guard,
                        Err(
                            ChunkReadError::CheckpointGuardMismatch
                            | ChunkReadError::SourceChangedBeforeRead
                            | ChunkReadError::SourceChangedDuringRead,
                        ) => return Ok(UsageReadStep::NeedsRebuildStop),
                        Err(error) => {
                            return Err(read_error_code(error));
                        }
                    };
                    dto.next_guard_hash = accepted_guard.map(|guard| guard.as_bytes().to_vec());
                }
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
            PipelineDisposition::FatalAnomaly(anomaly) => {
                return Ok(UsageReadStep::FatalAnomaly { anomaly });
            }
            PipelineDisposition::NeedsRebuild => return Ok(UsageReadStep::NeedsRebuild),
            PipelineDisposition::Skip | PipelineDisposition::BlockedRelationship => {
                return Ok(UsageReadStep::AwaitingOwnership);
            }
        }
    }
}

fn accepted_checkpoint_guard(
    path: &std::path::Path,
    identity: PhysicalIdentity,
    offset: u64,
) -> Result<Option<GuardHash>, ChunkReadError> {
    read_chunk_bounded(
        &ChunkReadPlan {
            path: path.to_path_buf(),
            identity,
            start_offset: offset,
            observed_size: offset,
            expected_guard: None,
        },
        |_| ReadControl::Continue,
    )
    .map(|chunk| chunk.guard)
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
        reconciliation_context:
            crate::codex::ingestion::usage_processor::ReconciliationContext::default(),
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
        reconciliation_carry:
            crate::codex::ingestion::usage_processor::ReconciliationCarry::from_json(
                &state.reconciliation_state_json,
            )
            .map_err(|_| "USAGE_RECONCILIATION_CARRY_INVALID")?,
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
                    decoded: OnceCell::new(),
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
    let checkpoint_guard = match (
        u64::try_from(checkpoint.committed_offset)
            .map_err(|_| "USAGE_CHECKPOINT_OFFSET_INVALID")?,
        checkpoint.guard_hash.as_deref(),
    ) {
        (0, None) => Ok(None),
        (1.., Some(bytes)) => guard_from_slice(bytes).map(Some),
        _ => Err("USAGE_CHECKPOINT_GUARD_INVALID"),
    }?;
    if source.start_offset == 0 {
        Ok(None)
    } else {
        Ok(checkpoint_guard)
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
            anomalies: dto.patch.anomalies.len() as u64,
        };
        for event in &dto.patch.events {
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
    fn compaction_pipeline_context_budgeted_resume_uses_accepted_offset_guard() {
        use std::{fs, sync::atomic::AtomicU64};

        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "usagi-usage-consumer-accepted-guard-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("session.jsonl");
        let bytes = b"accepted\nreader-tail\n";
        fs::write(&path, bytes).unwrap();
        let identity = crate::platform::file_identity::identity_from_path(&path).unwrap();
        let accepted_offset = b"accepted\n".len() as u64;
        let accepted_guard = accepted_checkpoint_guard(&path, identity, accepted_offset)
            .unwrap()
            .unwrap();

        let resumed = read_chunk_bounded(
            &ChunkReadPlan {
                path: path.clone(),
                identity,
                start_offset: accepted_offset,
                observed_size: bytes.len() as u64,
                expected_guard: Some(accepted_guard),
            },
            |_| ReadControl::Continue,
        )
        .unwrap();
        assert_eq!(resumed.last_complete_offset, bytes.len() as u64);

        let replay_checkpoint = UsageCheckpointExpectation {
            parser_version: USAGE_PARSER_VERSION,
            committed_offset: bytes.len() as i64,
            guard_hash: Some(
                accepted_checkpoint_guard(&path, identity, bytes.len() as u64)
                    .unwrap()
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
            ),
            processing_status: crate::codex::domain::CheckpointProcessingStatus::RebuildRequired,
        };
        let local_replay = UsageSourceScanPlan {
            source_file_id: 1,
            action: UsagePlanAction::LocalReplay,
            start_offset: 0,
            observed_size: bytes.len() as i64,
            owning_thread_id: Some("owner".to_owned()),
            root_session_id: Some("root".to_owned()),
            checkpoint: Some(replay_checkpoint.clone()),
            state: None,
            open_turn: None,
            build: None,
        };
        let replay_guard = checkpoint_guard(&local_replay).unwrap();
        assert_eq!(replay_guard, None);
        assert_eq!(local_replay.checkpoint, Some(replay_checkpoint));

        let reader_chunk = read_chunk_bounded(
            &ChunkReadPlan {
                path: path.clone(),
                identity,
                start_offset: 0,
                observed_size: bytes.len() as u64,
                expected_guard: replay_guard,
            },
            |_| ReadControl::Continue,
        )
        .unwrap();
        assert_ne!(reader_chunk.guard, Some(accepted_guard));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compaction_storage_oversized_patch_is_quarantined_with_structured_offset() {
        use std::{
            fs,
            sync::{
                Arc,
                atomic::{AtomicU64, Ordering},
            },
        };

        use crate::{
            codex::storage::CodexStorage,
            source::{SourceId, SourceStorage},
            storage::{Ledger, LedgerOptions},
        };
        use rusqlite::params;

        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

        let root = std::env::temp_dir().join(format!(
            "usagi-usage-consumer-quarantine-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let ledger = Arc::new(Ledger::open(LedgerOptions::new(root.join("mu.sqlite3"))).unwrap());
        {
            let connection = ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE codex_adapter_state
                     SET home_fingerprint='test-fixture',binding_status='ready' WHERE id=1",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO threads(
                        thread_id,source,native_session_id,parent_thread_id,root_session_id,
                        agent_role,project_kind,archived,metadata_quality_status,metadata_resolved_at_ms
                     ) VALUES ('root','codex','root',NULL,'root','main','unknown',0,'complete',1)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO threads(
                        thread_id,source,native_session_id,parent_thread_id,root_session_id,
                        agent_role,project_kind,archived,metadata_quality_status,metadata_resolved_at_ms
                     ) VALUES ('child','codex','child','root','root','subagent','unknown',0,'complete',1)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO codex_source_files(
                        source_file_id,thread_id,current_path,source_area,device_id,inode,
                        file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms
                     ) VALUES (1,'child',?1,'sessions',1,1,1,100,1,'present',1)",
                    [root.join("session.jsonl").to_string_lossy().as_ref()],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO codex_source_checkpoints(
                        source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,
                        processing_status,last_successful_scan_at_ms,last_error_code
                     ) VALUES (1,'metadata',1,80,?1,'ready',1,NULL),
                              (1,'usage',?2,0,NULL,'pending',NULL,NULL)",
                    params![
                        vec![8_u8; 32],
                        crate::codex::normalization::USAGE_PARSER_VERSION
                    ],
                )
                .unwrap();
        }

        let source = SourceStorage::with_ledger("test", SourceId::CODEX, Arc::clone(&ledger));
        let storage = CodexStorage::new(&source).unwrap();
        storage
            .begin_rebuild(crate::codex::normalization::USAGE_PARSER_VERSION, &[1], 1)
            .unwrap();
        let checkpoint_before: i64 = ledger
            .connection()
            .unwrap()
            .query_row(
                "SELECT committed_offset FROM codex_source_checkpoints
                 WHERE source_file_id=1 AND consumer_kind='usage'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        let outcome = patch_too_large_build_failure(
            Anomaly {
                code: AnomalyCode::ReconciliationPatchTooLarge,
                source_start_offset: Some(56),
                turn_key: Some("turn".to_owned()),
            },
            1,
            1,
            "child",
            2,
        );
        let UsageThreadOutcome::SessionDataError {
            code,
            quarantine_diagnostic: Some(diagnostic),
        } = outcome
        else {
            panic!("oversized patch must carry a typed quarantine diagnostic");
        };
        assert_eq!(code, PATCH_TOO_LARGE_ERROR_CODE);
        storage
            .quarantine_thread("child", code, Some(diagnostic), 3)
            .unwrap();

        let connection = ledger.connection().unwrap();
        let quarantine_code: String = connection
            .query_row(
                "SELECT primary_error_code FROM codex_usage_session_quarantine
                 WHERE root_session_id='root'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(quarantine_code, PATCH_TOO_LARGE_ERROR_CODE);
        let diagnostic: (String, i64, i64, i64, String, String) = connection
            .query_row(
                "SELECT thread_id,source_file_id,file_generation,source_start_offset,
                        anomaly_type,details_json
                 FROM codex_ingest_anomalies
                 WHERE anomaly_type='RECONCILIATION_PATCH_TOO_LARGE'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            diagnostic,
            (
                "child".to_owned(),
                1,
                1,
                56,
                "RECONCILIATION_PATCH_TOO_LARGE".to_owned(),
                r#"{"turn_key":"turn"}"#.to_owned()
            )
        );
        let completion_status: String = connection
            .query_row(
                "SELECT completion_status FROM codex_usage_build_sources
                 WHERE source_file_id=1
                   AND build_epoch=(SELECT build_epoch FROM source_usage_epochs WHERE source='codex')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(completion_status, "quarantined");
        let checkpoint_after: i64 = connection
            .query_row(
                "SELECT committed_offset FROM codex_source_checkpoints
                 WHERE source_file_id=1 AND consumer_kind='usage'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(checkpoint_after, checkpoint_before);
        drop(connection);
        drop(storage);
        drop(source);
        drop(ledger);
        fs::remove_dir_all(root).unwrap();
    }

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
