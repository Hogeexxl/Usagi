//! The fixed metadata scan pipeline and its public scheduling seam.
//!
//! The coordinator owns lifecycle persistence and serial scheduling.  This
//! module supplies the one worker used by that coordinator; callers cannot
//! register an arbitrary consumer or bypass the metadata stages.

use std::{
    collections::BTreeMap,
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    codex::{
        GlobalStateReader, METADATA_PARSER_VERSION, ResumeState, RolloutMetadataParser,
        RolloutParseContext, RolloutThreadFact, SessionIndexReader, SessionNameSnapshot,
        SourceAvailability, StateIndexReader, StateSnapshot,
    },
    domain::{CheckpointRebuildCommand, ConsumerKind, MetadataScanStateEntry, SafeFactState},
    platform::paths,
    storage::Ledger,
};

use crate::codex::rollout::{
    CompleteRolloutLine, OwningThreadCandidate, OwningThreadCandidates, RolloutChunkParser,
};

mod chunk_reader;
mod coordinator;
mod discovery;
mod pipeline;
mod report;
mod usage_consumer;

pub use crate::domain::ScanTrigger;
pub use coordinator::{
    CommitFailureKind, RequestDisposition, ScanConfig, ScanConfigError, ScanHandle,
    ScanRequestError, ScanShutdownError, ScanStartError,
};

use chunk_reader::read_chunk;
use chunk_reader::{ChunkReadError, ChunkReadPlan, FramedItem, GuardHash, PhysicalIdentity};
use discovery::{DiscoveredFile, Discovery, DiscoverySnapshot};
use pipeline::{FilePlan, MetadataPipeline, ParsedSource, PipelinePlan, PipelineResolutionInput};
use report::ScanReport;

/// Paths for the fixed Codex metadata adapters.
///
/// `from_home` follows the current Codex layout. `with_paths` is useful to
/// callers that already have a resolved test or alternate source layout while
/// retaining the same fixed metadata worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexMetadata {
    pub state_index_path: PathBuf,
    pub session_index_path: PathBuf,
    pub global_state_path: PathBuf,
}

impl CodexMetadata {
    pub fn from_home(codex_home: impl Into<PathBuf>) -> Self {
        let codex_home = codex_home.into();
        Self {
            state_index_path: codex_home.join("state_5.sqlite"),
            session_index_path: codex_home.join("session_index.jsonl"),
            global_state_path: codex_home.join(".codex-global-state.json"),
        }
    }

    pub fn with_paths(
        state_index_path: impl Into<PathBuf>,
        session_index_path: impl Into<PathBuf>,
        global_state_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            state_index_path: state_index_path.into(),
            session_index_path: session_index_path.into(),
            global_state_path: global_state_path.into(),
        }
    }
}

impl From<&ScanConfig> for CodexMetadata {
    fn from(config: &ScanConfig) -> Self {
        Self::from_home(config.codex_home.clone())
    }
}

/// Start the coordinator with Usagi's fixed metadata worker.
pub struct ScanCoordinator;

impl ScanCoordinator {
    pub fn start(
        config: ScanConfig,
        ledger: Arc<Ledger>,
        codex_metadata: CodexMetadata,
    ) -> Result<ScanHandle, ScanStartError> {
        let worker = Arc::new(MetadataWorker {
            config: config.clone(),
            ledger: Arc::clone(&ledger),
            codex_metadata,
        });
        coordinator::ScanCoordinator::start(config, ledger, worker)
    }
}

struct MetadataWorker {
    config: ScanConfig,
    ledger: Arc<Ledger>,
    codex_metadata: CodexMetadata,
}

struct ParseSourcesInput<'a> {
    discovery: &'a DiscoverySnapshot,
    outcome: &'a crate::domain::SourceOutcome,
    scan_state: &'a crate::domain::MetadataScanState,
    plan: &'a PipelinePlan,
    state_snapshot: &'a StateSnapshot,
    cancellation: &'a AtomicBool,
    report: &'a mut ScanReport,
    hard_error: &'a mut Option<&'static str>,
}

impl coordinator::ScanWorker for MetadataWorker {
    fn run(&self, _scan_id: &str, cancellation: &AtomicBool) -> coordinator::WorkerResult {
        if cancellation.load(Ordering::Acquire) {
            return coordinator::WorkerResult::Completed;
        }
        match self.run_round(cancellation) {
            Ok(()) => coordinator::WorkerResult::Completed,
            Err(error_code) => coordinator::WorkerResult::Failed(error_code),
        }
    }
}

impl MetadataWorker {
    fn run_round(&self, cancellation: &AtomicBool) -> Result<(), &'static str> {
        let round_at_ms = now_ms();
        let mut report = ScanReport::new(round_at_ms);
        self.run_round_with_report(cancellation, &mut report)
    }

    fn run_round_with_report(
        &self,
        cancellation: &AtomicBool,
        report: &mut ScanReport,
    ) -> Result<(), &'static str> {
        let round_at_ms = report.started_at_ms;
        let mut hard_error = None;
        let state_snapshot = read_state_snapshot(&self.codex_metadata.state_index_path);
        if !state_snapshot.status.is_complete() || !state_snapshot.spawn_edges_status.is_complete()
        {
            record_hard_error(report, &mut hard_error, "STATE_SOURCE_UNAVAILABLE");
        }
        if cancelled(cancellation) {
            report.finish(now_ms());
            return Ok(());
        }
        let session_name_snapshot = read_session_snapshot(&self.codex_metadata.session_index_path);
        if session_name_snapshot.status == SourceAvailability::Unavailable {
            record_hard_error(report, &mut hard_error, "SESSION_INDEX_UNAVAILABLE");
        }
        let global_state_snapshot =
            GlobalStateReader::read_snapshot(&self.codex_metadata.global_state_path);
        if cancelled(cancellation) {
            report.finish(now_ms());
            return Ok(());
        }

        let discovery = Discovery::discover_at(&self.config.codex_home, round_at_ms);
        report.observe_discovery(discovery.files.len());
        for diagnostic in &discovery.diagnostics {
            if diagnostic.code != "DUPLICATE_PHYSICAL_ALIAS" {
                record_hard_error(report, &mut hard_error, diagnostic.code);
            }
        }
        if !discovery.sessions.is_complete() || !discovery.archived_sessions.is_complete() {
            record_hard_error(report, &mut hard_error, "SOURCE_AREA_UNAVAILABLE");
        }
        let pipeline = match MetadataPipeline::new(METADATA_PARSER_VERSION, round_at_ms) {
            Ok(pipeline) => pipeline,
            Err(_) => return finish_round_error(report, "METADATA_PIPELINE_INVALID"),
        };
        let usage_carry_proofs = match usage_consumer::collect_usage_carry_observation_proofs(
            &self.ledger,
            &discovery,
            report,
        ) {
            Ok(proofs) => proofs,
            Err(error_code) => return finish_round_error(report, error_code),
        };
        let (outcome, scan_state) = match pipeline.record_and_load_with_usage_carry_proofs(
            &self.ledger,
            &discovery,
            &usage_carry_proofs,
        ) {
            Ok(value) => value,
            Err(_) => return finish_round_error(report, "SOURCE_OBSERVATION_FAILED"),
        };
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
                record_hard_error(report, &mut hard_error, error_code);
            }
        }
        let parsed_sources = self.parse_sources(ParseSourcesInput {
            discovery: &discovery,
            outcome: &outcome,
            scan_state: &scan_state,
            plan: &plan,
            state_snapshot: &state_snapshot,
            cancellation,
            report,
            hard_error: &mut hard_error,
        });
        if let Err(error_code) = self.persist_metadata_rebuilds(&parsed_sources) {
            return finish_round_error(report, error_code);
        }
        if cancelled(cancellation) {
            report.finish(now_ms());
            return Ok(());
        }

        let existing_threads = match self.ledger.load_existing_threads() {
            Ok(threads) => threads,
            Err(_) => return finish_round_error(report, "METADATA_STATE_LOAD_FAILED"),
        }
        .into_iter()
        .map(Into::into)
        .collect();
        let resolution = match pipeline.resolve(PipelineResolutionInput {
            state_snapshot: state_snapshot.clone(),
            session_name_snapshot,
            global_state_snapshot,
            scan_state,
            plans: plan,
            parsed_sources,
            existing_threads,
        }) {
            Ok(resolution) => resolution,
            Err(_) => return finish_round_error(report, "METADATA_RESOLUTION_FAILED"),
        };
        if pipeline.commit(&self.ledger, &resolution).is_err() {
            return finish_round_error(report, "METADATA_COMMIT_FAILED");
        }
        if let Err(error_code) = usage_consumer::run_usage_round(
            &self.ledger,
            &discovery,
            &outcome,
            &state_snapshot,
            cancellation,
            report,
        ) {
            record_hard_error(report, &mut hard_error, error_code);
        }
        report.finish(now_ms());
        match hard_error {
            Some(error_code) => Err(error_code),
            None => Ok(()),
        }
    }

    fn parse_sources(&self, input: ParseSourcesInput<'_>) -> Vec<ParsedSource> {
        let ParseSourcesInput {
            discovery,
            outcome,
            scan_state,
            plan,
            state_snapshot,
            cancellation,
            report,
            hard_error,
        } = input;
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
            let parsed_source =
                match self.parse_one(file, entry, file_plan, state_snapshot, cancellation) {
                    Ok(value) => {
                        report.observe_parse(&value);
                        if value.needs_rebuild || !value.stable() {
                            record_hard_error(report, hard_error, "METADATA_CONTINUATION_UNSTABLE");
                        }
                        value
                    }
                    Err(error_code) => {
                        report.failed_source();
                        record_hard_error(report, hard_error, error_code);
                        continue;
                    }
                };
            parsed.push(parsed_source);
        }
        parsed
    }

    fn persist_metadata_rebuilds(
        &self,
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
        self.ledger
            .require_checkpoint_rebuild(command)
            .map_err(|_| "STORAGE_COMMIT_FAILED")?;
        Ok(())
    }

    fn parse_one(
        &self,
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
            FilePlan::Skip { .. } | FilePlan::Reject { .. } => {
                return Err("METADATA_PLAN_NOT_READ");
            }
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
        let mut parser: RolloutChunkParser =
            RolloutMetadataParser::start_chunk(RolloutParseContext {
                source_file_id,
                chunk_start_offset: start_offset,
                candidates,
                resume_state,
                existing_fact,
            });
        let read_result = read_chunk(&chunk_plan, |item| match item {
            FramedItem::Line(line) => {
                let start = line.start_offset();
                if let Some(line) = CompleteRolloutLine::new(start, line.into_bytes_with_newline())
                {
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
        let guard_hash = read_result
            .guard
            .as_ref()
            .map(|guard| guard.as_bytes().to_vec());
        Ok(ParsedSource {
            source_file_id,
            fact: result.fact,
            final_continuation: result.final_continuation,
            last_processed_offset: read_result.last_complete_offset,
            guard_hash,
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

pub(super) fn owning_candidates(
    file: &DiscoveredFile,
    state: &StateSnapshot,
) -> OwningThreadCandidates {
    let state_rollout = state
        .threads
        .iter()
        .find(|thread| {
            thread.rollout_path.as_deref().is_some_and(|rollout_path| {
                paths::same_source_path(Path::new(rollout_path), &file.path)
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

pub(super) fn read_error_code(error: ChunkReadError) -> &'static str {
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
    error_code: &'static str,
) {
    report.error(error_code);
    if hard_error.is_none() {
        *hard_error = Some(error_code);
    }
}

fn finish_round_error(
    report: &mut ScanReport,
    error_code: &'static str,
) -> Result<(), &'static str> {
    report.error(error_code);
    report.finish(now_ms());
    Err(error_code)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        path::{Path, PathBuf},
        sync::atomic::AtomicU64,
    };

    use rusqlite::{Connection, params};
    use serde_json::json;

    use super::*;
    use crate::{
        platform::file_identity,
        storage::LedgerOptions,
        usage::{AggregateReader, SessionPageRequest, TimeRange},
    };

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    type StateThreadInput = (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    type FactProvenance = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "usagi-worker-{label}-{}-{}",
                now_ms(),
                TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create worker test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        _temp: TempDir,
        home: PathBuf,
        ledger: Arc<Ledger>,
        main_id: String,
        child_id: String,
        main_project: PathBuf,
        child_project: PathBuf,
        project_sentinel: PathBuf,
        main_path: PathBuf,
        child_path: PathBuf,
        archived_main_path: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let temp = TempDir::new(label);
            let home = temp.path().join("codex");
            let sessions = home.join("sessions");
            let archived = home.join("archived_sessions");
            let main_project = temp.path().join("project-main");
            let child_project = temp.path().join("project-child");
            let project_sentinel = main_project.join("prompt.txt");
            fs::create_dir_all(&sessions).expect("create sessions directory");
            fs::create_dir_all(&archived).expect("create archived directory");
            fs::create_dir_all(&main_project).expect("create main project directory");
            fs::create_dir_all(&child_project).expect("create child project directory");
            fs::write(&project_sentinel, "PROJECT_FILE_SENTINEL").expect("write project sentinel");

            let main_id = "00000000-0000-4000-8000-000000000001".to_owned();
            let child_id = "00000000-0000-4000-8000-000000000002".to_owned();
            let main_path = sessions.join(format!("rollout-{main_id}.jsonl"));
            let child_path = sessions.join(format!("rollout-{child_id}.jsonl"));
            let archived_main_path = archived.join(format!("rollout-{main_id}.jsonl"));
            let main_cwd = main_project.to_str().unwrap().to_owned();
            let child_cwd = child_project.to_str().unwrap().to_owned();

            fs::write(
                &main_path,
                rollout_bytes(&main_id, &main_cwd, Some("main"), None),
            )
            .expect("write main rollout");
            fs::write(
                &child_path,
                rollout_bytes(&child_id, &child_cwd, None, Some(&main_id)),
            )
            .expect("write child rollout");
            fs::write(
                &archived_main_path,
                rollout_bytes(&main_id, &main_cwd, Some("main"), None),
            )
            .expect("write archived rollout");

            let state_threads = vec![
                (
                    main_id.clone(),
                    main_path.to_str().unwrap().to_owned(),
                    Some(main_cwd),
                    None,
                    Some("main".to_owned()),
                ),
                (
                    child_id.clone(),
                    child_path.to_str().unwrap().to_owned(),
                    Some(child_cwd),
                    None,
                    Some("subagent".to_owned()),
                ),
            ];
            write_state(
                &home,
                &state_threads,
                &[(main_id.as_str(), child_id.as_str())],
            );
            write_session_index(&home, &main_id, &child_id);

            let ledger = Arc::new(
                Ledger::open(LedgerOptions::new(temp.path().join("mu.sqlite3"), &home))
                    .expect("open worker ledger"),
            );
            Self {
                _temp: temp,
                home,
                ledger,
                main_id,
                child_id,
                main_project,
                child_project,
                project_sentinel,
                main_path,
                child_path,
                archived_main_path,
            }
        }

        fn worker(&self) -> MetadataWorker {
            MetadataWorker {
                config: ScanConfig::new(self.home.clone()),
                ledger: Arc::clone(&self.ledger),
                codex_metadata: CodexMetadata::from_home(self.home.clone()),
            }
        }

        fn run(&self) -> Result<(), &'static str> {
            let worker = self.worker();
            worker.run_round(&AtomicBool::new(false))
        }

        fn run_result(&self) -> coordinator::WorkerResult {
            let worker = self.worker();
            coordinator::ScanWorker::run(&worker, "worker-test", &AtomicBool::new(false))
        }

        fn run_observed(&self) -> (Result<(), &'static str>, ScanReport) {
            let worker = self.worker();
            let mut report = ScanReport::new(now_ms());
            let result = worker.run_round_with_report(&AtomicBool::new(false), &mut report);
            (result, report)
        }
    }

    struct UsagePerformanceFixture {
        _temp: TempDir,
        home: PathBuf,
        ledger: Arc<Ledger>,
        thread_ids: Vec<String>,
        paths: Vec<PathBuf>,
    }

    impl UsagePerformanceFixture {
        fn new(label: &str) -> Self {
            let temp = TempDir::new(label);
            let home = temp.path().join("codex");
            let sessions = home.join("sessions");
            fs::create_dir_all(&sessions).expect("create performance sessions directory");

            let thread_ids = (1..=6)
                .map(|index| format!("10000000-0000-4000-8000-{index:012}"))
                .collect::<Vec<_>>();
            let parent_indexes = [None, Some(0), Some(0), Some(2), Some(0), Some(4)];
            let mut paths = Vec::with_capacity(thread_ids.len());
            let mut state_threads = Vec::with_capacity(thread_ids.len());
            let mut edges = Vec::new();
            let mut session_index = Vec::new();

            for (index, thread_id) in thread_ids.iter().enumerate() {
                let project = temp.path().join(format!("project-{index}"));
                fs::create_dir_all(&project).expect("create performance project directory");
                let path = sessions.join(format!("rollout-{thread_id}.jsonl"));
                let parent = parent_indexes[index].map(|parent| thread_ids[parent].as_str());
                fs::write(
                    &path,
                    rollout_bytes(
                        thread_id,
                        project.to_str().unwrap(),
                        (index == 0).then_some("main"),
                        parent,
                    ),
                )
                .expect("write performance rollout");
                state_threads.push((
                    thread_id.clone(),
                    path.to_str().unwrap().to_owned(),
                    Some(project.to_str().unwrap().to_owned()),
                    Some(format!("Thread {index}")),
                    Some(if index == 0 { "main" } else { "subagent" }.to_owned()),
                ));
                if let Some(parent) = parent {
                    edges.push((parent.to_owned(), thread_id.clone()));
                }
                session_index.extend(
                    serde_json::to_vec(&json!({
                        "id": thread_id,
                        "thread_name": format!("Thread {index}"),
                        "updated_at": "2026-08-08T01:02:05Z"
                    }))
                    .expect("serialize performance session index"),
                );
                session_index.push(b'\n');
                paths.push(path);
            }
            let edge_refs = edges
                .iter()
                .map(|(parent, child)| (parent.as_str(), child.as_str()))
                .collect::<Vec<_>>();
            write_state(&home, &state_threads, &edge_refs);
            fs::write(home.join("session_index.jsonl"), session_index)
                .expect("write performance session index");

            let ledger = Arc::new(
                Ledger::open(LedgerOptions::new(temp.path().join("mu.sqlite3"), &home))
                    .expect("open performance ledger"),
            );
            Self {
                _temp: temp,
                home,
                ledger,
                thread_ids,
                paths,
            }
        }

        fn worker(&self) -> MetadataWorker {
            MetadataWorker {
                config: ScanConfig::new(self.home.clone()),
                ledger: Arc::clone(&self.ledger),
                codex_metadata: CodexMetadata::from_home(self.home.clone()),
            }
        }

        fn run_observed(&self) -> (Result<(), &'static str>, ScanReport) {
            let mut report = ScanReport::new(now_ms());
            let result = self
                .worker()
                .run_round_with_report(&AtomicBool::new(false), &mut report);
            (result, report)
        }

        fn stabilize(&self) {
            for _ in 0..3 {
                self.run_observed().0.expect("stabilize usage ledger");
                let (active, build, _) = usage_epochs(&self.ledger);
                if active.unwrap_or(0) > 0 && build.is_none() {
                    return;
                }
            }
            panic!("usage ledger did not reach a stable active epoch");
        }
    }

    fn rollout_bytes(
        thread_id: &str,
        cwd: &str,
        agent_role: Option<&str>,
        parent_thread_id: Option<&str>,
    ) -> Vec<u8> {
        let mut values = vec![json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:02:03Z",
            "payload": {
                "id": thread_id,
                "timestamp": "2026-08-08T01:02:03Z",
                "cwd": cwd,
                "agent_role": agent_role,
                "source": parent_thread_id.map(|parent| json!({
                    "subagent": {
                        "thread_spawn": {"parent_thread_id": parent}
                    }
                })),
            }
        })];
        values.push(json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:02:04Z",
            "payload": {
                "turn_id": "00000000-0000-4000-8000-000000000010",
                "cwd": cwd,
                "model": "rollout-model"
            }
        }));
        values.push(json!({
            "type": "event_msg",
            "payload": {"type": "token_count", "sentinel": "ROLL_OUT_BODY_SENTINEL"}
        }));
        values.push(json!({
            "type": "response_item",
            "payload": {"text": "ROLL_OUT_BODY_SENTINEL"}
        }));
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend(serde_json::to_vec(&value).expect("serialize rollout fixture"));
            bytes.push(b'\n');
        }
        bytes
    }

    fn guardian_rollout_bytes(thread_id: &str, cwd: &str, parent_thread_id: &str) -> Vec<u8> {
        let values = [
            json!({
                "type": "session_meta",
                "timestamp": "2026-08-08T01:02:03Z",
                "payload": {
                    "id": thread_id,
                    "timestamp": "2026-08-08T01:02:03Z",
                    "cwd": cwd,
                    "parent_thread_id": parent_thread_id,
                    "source": {"subagent": {"other": "guardian"}},
                }
            }),
            json!({
                "type": "turn_context",
                "timestamp": "2026-08-08T01:02:04Z",
                "payload": {
                    "turn_id": "00000000-0000-4000-8000-000000000010",
                    "cwd": cwd,
                    "model": "rollout-model"
                }
            }),
            json!({
                "type": "event_msg",
                "payload": {"type": "token_count", "sentinel": "ROLL_OUT_BODY_SENTINEL"}
            }),
            json!({
                "type": "response_item",
                "payload": {"text": "ROLL_OUT_BODY_SENTINEL"}
            }),
        ];
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend(serde_json::to_vec(&value).expect("serialize guardian rollout fixture"));
            bytes.push(b'\n');
        }
        bytes
    }

    fn write_state(home: &Path, threads: &[StateThreadInput], edges: &[(&str, &str)]) {
        let path = home.join("state_5.sqlite");
        let connection = Connection::open(path).expect("create state fixture");
        connection
            .execute_batch(
                "CREATE TABLE threads (
                    id TEXT NOT NULL,
                    rollout_path TEXT,
                    created_at_ms INTEGER,
                    updated_at_ms INTEGER,
                    archived INTEGER,
                    cwd TEXT,
                    title TEXT,
                    name TEXT,
                    model TEXT,
                    agent_role TEXT
                );
                CREATE TABLE thread_spawn_edges (
                    parent_thread_id TEXT NOT NULL,
                    child_thread_id TEXT NOT NULL,
                    status TEXT,
                    observed_at_ms INTEGER
                );",
            )
            .expect("create state fixture schema");
        for (id, rollout_path, cwd, title, role) in threads {
            connection
                .execute(
                    "INSERT INTO threads (
                        id, rollout_path, created_at_ms, updated_at_ms,
                        archived, cwd, title, name, model, agent_role
                    ) VALUES (?1, ?2, 1_700_000_000_000, 1_700_000_000_100,
                              0, ?3, ?4, NULL, 'state-model', ?5)",
                    params![id, rollout_path, cwd, title, role,],
                )
                .expect("insert state thread");
        }
        for (parent, child) in edges {
            connection
                .execute(
                    "INSERT INTO thread_spawn_edges (
                        parent_thread_id, child_thread_id, status, observed_at_ms
                    ) VALUES (?1, ?2, 'confirmed', 1_700_000_000_100)",
                    params![parent, child],
                )
                .expect("insert state edge");
        }
    }

    fn write_session_index(home: &Path, main_id: &str, child_id: &str) {
        let mut bytes = Vec::new();
        for (id, title) in [
            (main_id, "Main session title"),
            (child_id, "Child session title"),
        ] {
            bytes.extend(
                serde_json::to_vec(&json!({
                    "id": id,
                    "thread_name": title,
                    "updated_at": "2026-08-08T01:02:05Z",
                    "preview": "SESSION_INDEX_BODY_SENTINEL"
                }))
                .expect("serialize session fixture"),
            );
            bytes.push(b'\n');
        }
        fs::write(home.join("session_index.jsonl"), bytes).expect("write session index fixture");
    }

    fn update_state_rollout_path(home: &Path, thread_id: &str, path: &Path) {
        let connection = Connection::open(home.join("state_5.sqlite")).unwrap();
        connection
            .execute(
                "UPDATE threads SET rollout_path = ?1 WHERE id = ?2",
                params![path.to_str().unwrap(), thread_id],
            )
            .unwrap();
    }

    fn update_state_thread_id(home: &Path, old_thread_id: &str, new_thread_id: &str) {
        let connection = Connection::open(home.join("state_5.sqlite")).unwrap();
        connection
            .execute(
                "UPDATE thread_spawn_edges
                 SET child_thread_id = ?1
                 WHERE child_thread_id = ?2",
                params![new_thread_id, old_thread_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE threads SET id = ?1 WHERE id = ?2",
                params![new_thread_id, old_thread_id],
            )
            .unwrap();
    }

    fn delete_state_spawn_edge(home: &Path, parent_thread_id: &str, child_thread_id: &str) {
        let connection = Connection::open(home.join("state_5.sqlite")).unwrap();
        connection
            .execute(
                "DELETE FROM thread_spawn_edges
                 WHERE parent_thread_id = ?1 AND child_thread_id = ?2",
                params![parent_thread_id, child_thread_id],
            )
            .unwrap();
    }

    fn update_state_title(home: &Path, thread_id: &str, title: Option<&str>) {
        let connection = Connection::open(home.join("state_5.sqlite")).unwrap();
        connection
            .execute(
                "UPDATE threads SET title = ?1 WHERE id = ?2",
                params![title, thread_id],
            )
            .unwrap();
    }

    fn source_checkpoint(
        ledger: &Ledger,
        path: &Path,
    ) -> (i64, Option<String>, i64, String, Option<Vec<u8>>) {
        let connection = Connection::open(ledger.database_path()).expect("open ledger query");
        connection
            .query_row(
                "SELECT source_file_id, thread_id, committed_offset,
                        processing_status, guard_hash
                 FROM source_files
                 JOIN source_checkpoints USING (source_file_id)
                 WHERE current_path = ?1 AND consumer_kind = 'metadata'",
                [path.to_str().expect("source path utf8")],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("read source checkpoint")
    }

    fn source_identity(ledger: &Ledger, path: &Path) -> (i64, i64, i64, i64, i64) {
        let connection = Connection::open(ledger.database_path()).expect("open ledger query");
        connection
            .query_row(
                "SELECT device_id, inode, file_generation, observed_size,
                        observed_mtime_ns
                 FROM source_files WHERE current_path = ?1",
                [path.to_str().expect("source path utf8")],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("read source identity")
    }

    fn checkpoint_state(
        ledger: &Ledger,
        path: &Path,
        consumer: &str,
    ) -> (i64, i64, String, Option<Vec<u8>>) {
        let connection = Connection::open(ledger.database_path()).expect("open ledger query");
        connection
            .query_row(
                "SELECT parser_version, committed_offset, processing_status, guard_hash
                 FROM source_files
                 JOIN source_checkpoints USING (source_file_id)
                 WHERE current_path = ?1 AND consumer_kind = ?2",
                params![path.to_str().expect("source path utf8"), consumer],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("read checkpoint state")
    }

    fn fact_parser_and_parent(
        ledger: &Ledger,
        source_file_id: i64,
    ) -> (i64, Option<String>, Option<String>) {
        let connection = Connection::open(ledger.database_path()).expect("open ledger query");
        connection
            .query_row(
                "SELECT metadata_parser_version, parent_thread_id_hint,
                        parent_hint_provenance
                 FROM rollout_metadata_facts WHERE source_file_id = ?1",
                [source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read fact parser and parent")
    }

    fn fact_parent_columns(
        ledger: &Ledger,
        source_file_id: i64,
    ) -> (Option<String>, Option<String>, Option<i64>) {
        let connection = Connection::open(ledger.database_path()).expect("open ledger query");
        connection
            .query_row(
                "SELECT parent_thread_id_hint, parent_hint_provenance,
                        parent_hint_record_offset
                 FROM rollout_metadata_facts WHERE source_file_id = ?1",
                [source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read fact parent columns")
    }

    fn usage_epochs(ledger: &Ledger) -> (Option<i64>, Option<i64>, i64) {
        let connection = Connection::open(ledger.database_path()).expect("open ledger query");
        connection
            .query_row(
                "SELECT usage_active_epoch, usage_build_epoch, usage_parser_version
                 FROM app_meta WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read usage epochs")
    }

    fn usage_checkpoint_count(ledger: &Ledger) -> i64 {
        let connection = Connection::open(ledger.database_path()).expect("open ledger query");
        connection
            .query_row(
                "SELECT COUNT(*) FROM source_checkpoints WHERE consumer_kind = 'usage'",
                [],
                |row| row.get(0),
            )
            .expect("count usage checkpoints")
    }

    fn active_usage_event_count(ledger: &Ledger) -> i64 {
        Connection::open(ledger.database_path())
            .expect("open ledger query")
            .query_row(
                "SELECT COUNT(*) FROM usage_events
                 WHERE ledger_epoch=(SELECT usage_active_epoch FROM app_meta WHERE id=1)",
                [],
                |row| row.get(0),
            )
            .expect("count active usage events")
    }

    fn active_quarantine_count(ledger: &Ledger) -> i64 {
        Connection::open(ledger.database_path())
            .expect("open ledger query")
            .query_row(
                "SELECT COUNT(*) FROM usage_session_quarantine
                 WHERE ledger_epoch=(SELECT usage_active_epoch FROM app_meta WHERE id=1)",
                [],
                |row| row.get(0),
            )
            .expect("count active Session quarantines")
    }

    fn fact_provenance(ledger: &Ledger, source_file_id: i64) -> FactProvenance {
        let connection = Connection::open(ledger.database_path()).expect("open ledger query");
        connection
            .query_row(
                "SELECT cwd, cwd_provenance, parent_thread_id_hint,
                        parent_hint_provenance, agent_role_hint, agent_role_provenance
                 FROM rollout_metadata_facts WHERE source_file_id = ?1",
                [source_file_id],
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
            .expect("read safe fact provenance")
    }

    fn data_revision(ledger: &Ledger) -> i64 {
        ledger.app_state().expect("read app state").data_revision
    }

    fn append(path: &Path, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open rollout for append");
        file.write_all(bytes).expect("append rollout");
        file.sync_all().expect("sync rollout append");
    }

    fn token_count_line(total: i64, last: i64) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&json!({
            "timestamp": 1000,
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": total,
                        "cached_input_tokens": 0,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": total
                    },
                    "last_token_usage": {
                        "input_tokens": last,
                        "cached_input_tokens": 0,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": last
                    }
                }
            }
        }))
        .expect("serialize token count");
        bytes.push(b'\n');
        bytes
    }

    #[test]
    fn worker_imports_both_regions_and_keeps_metadata_checkpoint_isolated() {
        let fixture = Fixture::new("checkpoint");

        fixture.run().expect("initial metadata import");
        let (main_source_id, main_thread, initial_offset, status, _) =
            source_checkpoint(&fixture.ledger, &fixture.main_path);
        let (_, child_thread, _, _, _) = source_checkpoint(&fixture.ledger, &fixture.child_path);
        let (_, archived_thread, archived_offset, _, _) =
            source_checkpoint(&fixture.ledger, &fixture.archived_main_path);
        assert_eq!(main_thread.as_deref(), Some(fixture.main_id.as_str()));
        assert_eq!(child_thread.as_deref(), Some(fixture.child_id.as_str()));
        assert_eq!(archived_thread.as_deref(), Some(fixture.main_id.as_str()));
        assert_eq!(status, "ready");
        assert_eq!(initial_offset, archived_offset);
        assert!(initial_offset > 0);
        assert_eq!(usage_checkpoint_count(&fixture.ledger), 3);
        let (main_cwd, main_cwd_provenance, _, _, main_role_hint, main_role_provenance) =
            fact_provenance(&fixture.ledger, main_source_id);
        assert_eq!(
            main_cwd.as_deref(),
            Some(fixture.main_project.to_str().unwrap())
        );
        assert_eq!(main_cwd_provenance.as_deref(), Some("session_meta"));
        assert_eq!(main_role_hint.as_deref(), Some("main"));
        assert_eq!(main_role_provenance.as_deref(), Some("session_meta_role"));
        let (
            child_cwd,
            _,
            child_parent_hint,
            child_parent_provenance,
            child_role_hint,
            child_role_provenance,
        ) = fact_provenance(
            &fixture.ledger,
            source_checkpoint(&fixture.ledger, &fixture.child_path).0,
        );
        assert_eq!(
            child_cwd.as_deref(),
            Some(fixture.child_project.to_str().unwrap())
        );
        assert_eq!(child_parent_hint.as_deref(), Some(fixture.main_id.as_str()));
        assert_eq!(child_parent_provenance.as_deref(), Some("subagent_source"));
        assert_eq!(child_role_hint.as_deref(), Some("subagent"));
        assert_eq!(child_role_provenance.as_deref(), Some("subagent_source"));

        let connection = Connection::open(fixture.ledger.database_path()).unwrap();
        let (main_role, child_role, child_parent, main_title): (
            String,
            String,
            Option<String>,
            String,
        ) = connection
            .query_row(
                "SELECT
                    (SELECT agent_role FROM threads WHERE thread_id = ?1),
                    (SELECT agent_role FROM threads WHERE thread_id = ?2),
                    (SELECT parent_thread_id FROM threads WHERE thread_id = ?2),
                    (SELECT title FROM threads WHERE thread_id = ?1)",
                params![fixture.main_id, fixture.child_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(main_role, "main");
        assert_eq!(child_role, "subagent");
        assert_eq!(child_parent.as_deref(), Some(fixture.main_id.as_str()));
        assert_eq!(main_title, "Main session title");

        update_state_title(&fixture.home, &fixture.main_id, Some("State title"));
        fixture.run().expect("state title update is observed");
        let state_title: String = Connection::open(fixture.ledger.database_path())
            .unwrap()
            .query_row(
                "SELECT title FROM threads WHERE thread_id = ?1",
                [fixture.main_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state_title, "State title");
        update_state_title(&fixture.home, &fixture.main_id, None);
        fixture.run().expect("session title fallback is observed");
        let fallback_title: String = Connection::open(fixture.ledger.database_path())
            .unwrap()
            .query_row(
                "SELECT title FROM threads WHERE thread_id = ?1",
                [fixture.main_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fallback_title, "Main session title");

        update_state_rollout_path(&fixture.home, &fixture.main_id, &fixture.child_path);
        update_state_rollout_path(&fixture.home, &fixture.child_id, &fixture.main_path);
        fs::write(
            &fixture.main_path,
            rollout_bytes(
                &fixture.main_id,
                fixture.main_project.to_str().unwrap(),
                Some("main"),
                None,
            ),
        )
        .unwrap();
        assert_eq!(
            fixture.run_result(),
            coordinator::WorkerResult::Failed("METADATA_CONTINUATION_UNSTABLE")
        );
        let (_, conflict_thread, conflict_offset, conflict_status, _) =
            source_checkpoint(&fixture.ledger, &fixture.main_path);
        let (_, conflict_archive_thread, conflict_archive_offset, _, _) =
            source_checkpoint(&fixture.ledger, &fixture.archived_main_path);
        assert!(conflict_thread.is_none());
        assert_eq!(
            conflict_archive_thread.as_deref(),
            Some(fixture.main_id.as_str())
        );
        assert_eq!(conflict_archive_offset, archived_offset);
        assert_eq!(conflict_offset, 0);
        assert_eq!(conflict_status, "rebuild_required");
        update_state_rollout_path(&fixture.home, &fixture.main_id, &fixture.main_path);
        update_state_rollout_path(&fixture.home, &fixture.child_id, &fixture.child_path);
        fixture.run().expect("restored owning identity rebuilds");
        let (_, restored_thread, restored_offset, restored_status, _) =
            source_checkpoint(&fixture.ledger, &fixture.main_path);
        assert_eq!(restored_thread.as_deref(), Some(fixture.main_id.as_str()));
        assert_eq!(restored_status, "ready");
        assert_eq!(
            restored_offset,
            fs::metadata(&fixture.main_path).unwrap().len() as i64
        );
        assert_eq!(
            fs::read(&fixture.project_sentinel).unwrap(),
            b"PROJECT_FILE_SENTINEL"
        );

        let before_revision = data_revision(&fixture.ledger);
        fixture.run().expect("unchanged source should Skip");
        assert_eq!(data_revision(&fixture.ledger), before_revision);

        append(
            &fixture.main_path,
            br#"{"type":"event_msg","payload":{"type":"token_count","sentinel":"ROLL_OUT_BODY_SENTINEL"}}
not-json
{"type":"response_item","payload":{"text":"ROLL_OUT_BODY_SENTINEL"}}
{"type":"event_msg","payload":{"type":"token_count"}}"#,
        );
        let before_append = source_checkpoint(&fixture.ledger, &fixture.main_path).2;
        fixture.run().expect("token and malformed rows are safe");
        let after_append = source_checkpoint(&fixture.ledger, &fixture.main_path).2;
        assert!(after_append > before_append);
        assert_eq!(data_revision(&fixture.ledger), before_revision);
        assert_eq!(usage_checkpoint_count(&fixture.ledger), 3);

        append(
            &fixture.main_path,
            br#"{"type":"event_msg","payload":{"type":"token_count"}}"#,
        );
        let before_half_line = source_checkpoint(&fixture.ledger, &fixture.main_path).2;
        fixture.run().expect("half line remains a safe no-op");
        assert_eq!(
            source_checkpoint(&fixture.ledger, &fixture.main_path).2,
            before_half_line
        );
        append(&fixture.main_path, b"\n");
        fixture
            .run()
            .expect("completed token line advances metadata");
        assert!(source_checkpoint(&fixture.ledger, &fixture.main_path).2 > before_half_line);

        let source_id = source_checkpoint(&fixture.ledger, &fixture.main_path).0;
        let connection = Connection::open(fixture.ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE source_checkpoints
                 SET parser_version = 1
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [source_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE rollout_metadata_facts
                 SET metadata_parser_version = 1
                 WHERE source_file_id = ?1",
                [source_id],
            )
            .unwrap();
        drop(connection);
        fixture.run().expect("parser-version rebuild");
        assert_eq!(
            source_checkpoint(&fixture.ledger, &fixture.main_path).2,
            fs::metadata(&fixture.main_path).unwrap().len() as i64
        );
        assert_eq!(usage_checkpoint_count(&fixture.ledger), 3);
        let database = fs::read(fixture.ledger.database_path()).unwrap();
        assert!(!String::from_utf8_lossy(&database).contains("ROLL_OUT_BODY_SENTINEL"));
        assert!(!String::from_utf8_lossy(&database).contains("SESSION_INDEX_BODY_SENTINEL"));
    }

    #[test]
    fn stale_guardian_fact_replays_from_zero_and_leaves_usage_checkpoint_untouched() {
        let fixture = Fixture::new("s6-guardian");
        let guardian_bytes = guardian_rollout_bytes(
            &fixture.child_id,
            fixture.child_project.to_str().unwrap(),
            &fixture.main_id,
        );
        fs::write(&fixture.child_path, &guardian_bytes).expect("write guardian rollout fixture");
        delete_state_spawn_edge(&fixture.home, &fixture.main_id, &fixture.child_id);
        fixture.run().expect("initial guardian metadata import");

        let user_version: i64 = Connection::open(fixture.ledger.database_path())
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read guardian fixture schema version");
        assert_eq!(user_version, 10);
        let child_source_id = source_checkpoint(&fixture.ledger, &fixture.child_path).0;
        let file_before = fs::File::open(&fixture.child_path).expect("open guardian rollout");
        let metadata_before = file_before.metadata().expect("stat guardian rollout");
        let identity_before = file_identity::identity_from_file(&file_before).unwrap();
        let file_identity_before = (
            metadata_before.len(),
            identity_before.device_id,
            identity_before.inode,
            file_identity::modified_ns(&metadata_before).unwrap(),
        );
        let source_identity_before = source_identity(&fixture.ledger, &fixture.child_path);
        let usage_checkpoint_before =
            checkpoint_state(&fixture.ledger, &fixture.child_path, "usage");
        let usage_epochs_before = usage_epochs(&fixture.ledger);
        let (parser_before, parent_before, provenance_before) =
            fact_parser_and_parent(&fixture.ledger, child_source_id);
        assert_eq!(parser_before, METADATA_PARSER_VERSION);
        assert_eq!(parent_before.as_deref(), Some(fixture.main_id.as_str()));
        assert_eq!(provenance_before.as_deref(), Some("session_meta_parent"));

        let connection = Connection::open(fixture.ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE source_checkpoints
                 SET parser_version = 1
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [child_source_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE rollout_metadata_facts
                 SET metadata_parser_version = 1,
                     parent_thread_id_hint = NULL,
                     parent_hint_provenance = NULL,
                     parent_hint_record_offset = NULL
                 WHERE source_file_id = ?1",
                [child_source_id],
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            checkpoint_state(&fixture.ledger, &fixture.child_path, "metadata").0,
            1
        );
        assert_eq!(
            fact_parser_and_parent(&fixture.ledger, child_source_id).0,
            1
        );
        assert_eq!(
            fact_parent_columns(&fixture.ledger, child_source_id),
            (None, None, None)
        );

        let (result, report) = fixture.run_observed();
        result.expect("stale v1 guardian fact rebuild");
        assert_eq!(report.body_open_attempts, 1);
        assert_eq!(report.bytes_read, guardian_bytes.len() as u64);
        assert_eq!(report.usage_bytes_read, 0);

        let (_, child_thread, committed_offset, status, _) =
            source_checkpoint(&fixture.ledger, &fixture.child_path);
        assert_eq!(child_thread.as_deref(), Some(fixture.child_id.as_str()));
        assert_eq!(status, "ready");
        assert_eq!(committed_offset, guardian_bytes.len() as i64);
        let (metadata_parser_after, _, _, _) =
            checkpoint_state(&fixture.ledger, &fixture.child_path, "metadata");
        assert_eq!(metadata_parser_after, METADATA_PARSER_VERSION);
        let (parser_after, parent_after, provenance_after) =
            fact_parser_and_parent(&fixture.ledger, child_source_id);
        assert_eq!(parser_after, METADATA_PARSER_VERSION);
        assert_eq!(parent_after.as_deref(), Some(fixture.main_id.as_str()));
        assert_eq!(provenance_after.as_deref(), Some("session_meta_parent"));
        let (thread_parent, thread_root, thread_role): (Option<String>, Option<String>, String) =
            Connection::open(fixture.ledger.database_path())
                .unwrap()
                .query_row(
                    "SELECT parent_thread_id, root_session_id, agent_role
                 FROM threads WHERE thread_id = ?1",
                    [fixture.child_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("read Guardian thread parent and root");
        assert_eq!(thread_parent.as_deref(), Some(fixture.main_id.as_str()));
        assert_eq!(thread_root.as_deref(), Some(fixture.main_id.as_str()));
        assert_eq!(thread_role, "subagent");

        let file_after = fs::File::open(&fixture.child_path).expect("reopen guardian rollout");
        let metadata_after = file_after.metadata().expect("restat guardian rollout");
        let identity_after = file_identity::identity_from_file(&file_after).unwrap();
        let file_identity_after = (
            metadata_after.len(),
            identity_after.device_id,
            identity_after.inode,
            file_identity::modified_ns(&metadata_after).unwrap(),
        );
        assert_eq!(file_identity_after, file_identity_before);
        assert_eq!(
            source_identity(&fixture.ledger, &fixture.child_path),
            source_identity_before
        );
        assert_eq!(
            checkpoint_state(&fixture.ledger, &fixture.child_path, "usage"),
            usage_checkpoint_before
        );
        assert_eq!(usage_epochs(&fixture.ledger), usage_epochs_before);
    }

    #[test]
    fn failed_stale_guardian_replay_does_not_upgrade_fact_or_checkpoint_parser() {
        let fixture = Fixture::new("s6-guardian-failure");
        let guardian_bytes = guardian_rollout_bytes(
            &fixture.child_id,
            fixture.child_project.to_str().unwrap(),
            &fixture.main_id,
        );
        fs::write(&fixture.child_path, &guardian_bytes).expect("write guardian rollout fixture");
        delete_state_spawn_edge(&fixture.home, &fixture.main_id, &fixture.child_id);
        fixture.run().expect("initial guardian metadata import");
        let user_version: i64 = Connection::open(fixture.ledger.database_path())
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read guardian fixture schema version");
        assert_eq!(user_version, 10);
        let child_source_id = source_checkpoint(&fixture.ledger, &fixture.child_path).0;
        let usage_checkpoint_before =
            checkpoint_state(&fixture.ledger, &fixture.child_path, "usage");
        let usage_epochs_before = usage_epochs(&fixture.ledger);

        let connection = Connection::open(fixture.ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE source_checkpoints
                 SET parser_version = 1
                 WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                [child_source_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE rollout_metadata_facts
                 SET metadata_parser_version = 1,
                     parent_thread_id_hint = NULL,
                     parent_hint_provenance = NULL,
                     parent_hint_record_offset = NULL
                 WHERE source_file_id = ?1",
                [child_source_id],
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            fact_parent_columns(&fixture.ledger, child_source_id),
            (None, None, None)
        );

        // Keep the file and observation identity stable, but make the state
        // index's owning candidate disagree with its filename candidate.
        let conflicting_id = "00000000-0000-4000-8000-000000000003";
        update_state_thread_id(&fixture.home, &fixture.child_id, conflicting_id);
        let (result, report) = fixture.run_observed();
        assert_eq!(result, Err("METADATA_CONTINUATION_UNSTABLE"));
        assert!(
            report
                .error_codes
                .contains(&"METADATA_CONTINUATION_UNSTABLE")
        );
        assert_eq!(report.bytes_read, guardian_bytes.len() as u64);
        assert_eq!(
            checkpoint_state(&fixture.ledger, &fixture.child_path, "metadata").0,
            1
        );
        assert_eq!(
            fact_parser_and_parent(&fixture.ledger, child_source_id).0,
            1
        );
        assert_eq!(
            checkpoint_state(&fixture.ledger, &fixture.child_path, "metadata").2,
            "rebuild_required"
        );
        assert_eq!(
            checkpoint_state(&fixture.ledger, &fixture.child_path, "usage"),
            usage_checkpoint_before
        );
        assert_eq!(usage_epochs(&fixture.ledger), usage_epochs_before);
    }

    #[test]
    fn oversized_complete_metadata_line_advances_continuation_and_reuses_fact() {
        let fixture = Fixture::new("oversized-metadata");
        fixture.run().expect("initial metadata import");

        let mut oversized = vec![b'x'; chunk_reader::MAX_LINE_BYTES as usize + 1];
        oversized.push(b'\n');
        let mut appended = oversized;
        appended.extend(
            serde_json::to_vec(&json!({
                "type": "turn_context",
                "timestamp": "2026-08-08T01:02:06Z",
                "payload": {
                    "turn_id": "00000000-0000-4000-8000-000000000011",
                    "cwd": fixture.main_project.to_str().unwrap(),
                    "model": "rollout-model"
                }
            }))
            .expect("serialize synthetic continuation line"),
        );
        appended.push(b'\n');
        append(&fixture.main_path, &appended);

        let (result, report) = fixture.run_observed();
        result.expect("oversized complete line remains a completed metadata scan");
        assert_eq!(report.oversized_complete_lines, 1);
        assert!(
            !report
                .error_codes
                .contains(&"METADATA_CONTINUATION_UNSTABLE")
        );

        let (source_file_id, thread_id, committed_offset, status, _) =
            source_checkpoint(&fixture.ledger, &fixture.main_path);
        assert_eq!(thread_id.as_deref(), Some(fixture.main_id.as_str()));
        assert_eq!(status, "ready");
        assert_eq!(
            committed_offset,
            fs::metadata(&fixture.main_path).unwrap().len() as i64
        );
        let (continuation_state, fact_thread_id, resolved_offset): (String, String, i64) =
            Connection::open(fixture.ledger.database_path())
                .unwrap()
                .query_row(
                    "SELECT continuation_state, owning_thread_id, resolved_through_offset
                     FROM rollout_metadata_facts WHERE source_file_id = ?1",
                    [source_file_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("read metadata fact after oversized line");
        assert_eq!(continuation_state, "owning_live");
        assert_eq!(fact_thread_id, fixture.main_id);
        assert_eq!(resolved_offset, committed_offset);

        let mut next_line = serde_json::to_vec(&json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:02:07Z",
            "payload": {
                "turn_id": "00000000-0000-4000-8000-000000000012",
                "cwd": fixture.main_project.to_str().unwrap(),
                "model": "rollout-model"
            }
        }))
        .expect("serialize second synthetic continuation line");
        next_line.push(b'\n');
        append(&fixture.main_path, &next_line);
        let (reused_result, reused_report) = fixture.run_observed();
        reused_result.expect("stable metadata fact resumes after oversized line");
        assert!(
            !reused_report
                .error_codes
                .contains(&"METADATA_CONTINUATION_UNSTABLE")
        );
        assert_eq!(
            source_checkpoint(&fixture.ledger, &fixture.main_path).2,
            fs::metadata(&fixture.main_path).unwrap().len() as i64
        );
    }

    #[test]
    fn worker_does_not_commit_unconfirmed_source_and_marks_unavailable_snapshots_failed() {
        let fixture = Fixture::new("unconfirmed");
        let unknown_path = fixture
            .home
            .join("sessions")
            .join("rollout-unconfirmed.jsonl");
        fs::write(
            &unknown_path,
            br#"{"type":"turn_context","payload":{"turn_id":"00000000-0000-4000-8000-000000000010"}}
"#,
        )
        .unwrap();

        let result = fixture.run_result();
        assert_eq!(
            result,
            coordinator::WorkerResult::Failed("METADATA_CONTINUATION_UNSTABLE")
        );
        let (_, thread, offset, status, _) = source_checkpoint(&fixture.ledger, &unknown_path);
        assert!(thread.is_none());
        assert_eq!(offset, 0);
        assert_eq!(status, "pending");
        let (_, main_thread, main_offset, main_status, _) =
            source_checkpoint(&fixture.ledger, &fixture.main_path);
        assert_eq!(main_thread.as_deref(), Some(fixture.main_id.as_str()));
        assert!(main_offset > 0);
        assert_eq!(main_status, "ready");
        let (title_before_unavailable, role_before_unavailable): (String, String) =
            Connection::open(fixture.ledger.database_path())
                .unwrap()
                .query_row(
                    "SELECT title, agent_role FROM threads WHERE thread_id = ?1",
                    [fixture.main_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();

        let worker = MetadataWorker {
            config: ScanConfig::new(fixture.home.clone()),
            ledger: Arc::clone(&fixture.ledger),
            codex_metadata: CodexMetadata::with_paths(
                fixture.home.join("missing-state.sqlite"),
                fixture.home.join("session_index.jsonl"),
                fixture.home.join(".codex-global-state.json"),
            ),
        };
        assert_eq!(
            coordinator::ScanWorker::run(&worker, "state-unavailable", &AtomicBool::new(false)),
            coordinator::WorkerResult::Failed("STATE_SOURCE_UNAVAILABLE")
        );
        let unavailable_session_path = fixture.home.join("session-index-unavailable");
        fs::create_dir(&unavailable_session_path).unwrap();
        let worker = MetadataWorker {
            config: ScanConfig::new(fixture.home.clone()),
            ledger: Arc::clone(&fixture.ledger),
            codex_metadata: CodexMetadata::with_paths(
                fixture.home.join("state_5.sqlite"),
                unavailable_session_path,
                fixture.home.join(".codex-global-state.json"),
            ),
        };
        assert_eq!(
            coordinator::ScanWorker::run(&worker, "session-unavailable", &AtomicBool::new(false),),
            coordinator::WorkerResult::Failed("SESSION_INDEX_UNAVAILABLE")
        );
        let connection = Connection::open(fixture.ledger.database_path()).unwrap();
        let (unknown_title, unknown_role): (Option<String>, Option<String>) = connection
            .query_row(
                "SELECT title, agent_role FROM threads WHERE thread_id = ?1",
                [fixture.main_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            unknown_title.as_deref(),
            Some(title_before_unavailable.as_str())
        );
        assert_eq!(unknown_role.as_deref(), Some("main"));
        assert_eq!(role_before_unavailable, "main");
    }

    #[test]
    fn pipeline_commit_keeps_other_thread_group_after_injected_stale_binding() {
        let fixture = Fixture::new("isolation");
        let worker = fixture.worker();
        let cancellation = AtomicBool::new(false);
        let started_at_ms = now_ms();
        let discovery = Discovery::discover_at(&fixture.home, started_at_ms);
        let pipeline = MetadataPipeline::new(1, started_at_ms).unwrap();
        let (outcome, scan_state) = pipeline
            .record_and_load(&fixture.ledger, &discovery)
            .unwrap();
        let mut report = ScanReport::new(started_at_ms);
        let plan = pipeline.plan_files(&discovery, &outcome, &scan_state);
        let mut hard_error = None;
        let parsed_sources = worker.parse_sources(ParseSourcesInput {
            discovery: &discovery,
            outcome: &outcome,
            scan_state: &scan_state,
            plan: &plan,
            state_snapshot: &read_state_snapshot(&fixture.home.join("state_5.sqlite")),
            cancellation: &cancellation,
            report: &mut report,
            hard_error: &mut hard_error,
        });
        let existing_threads = fixture
            .ledger
            .load_existing_threads()
            .unwrap()
            .into_iter()
            .map(Into::into)
            .collect();
        let resolution = pipeline
            .resolve(PipelineResolutionInput {
                state_snapshot: read_state_snapshot(&fixture.home.join("state_5.sqlite")),
                session_name_snapshot: read_session_snapshot(
                    &fixture.home.join("session_index.jsonl"),
                ),
                global_state_snapshot: GlobalStateReader::read_snapshot(
                    fixture.home.join(".codex-global-state.json"),
                ),
                scan_state,
                plans: plan,
                parsed_sources,
                existing_threads,
            })
            .unwrap();
        let batch = resolution.commit_batch.as_ref().expect("groups to commit");
        assert!(batch.groups.len() >= 2);
        let failed_group = &batch.groups[0];
        let failed_source = failed_group.sources[0].source_file_id;
        let connection = Connection::open(fixture.ledger.database_path()).unwrap();
        connection
            .execute(
                "UPDATE source_files SET thread_id = 'injected-stale-thread'
                 WHERE source_file_id = ?1",
                [failed_source],
            )
            .unwrap();

        assert!(pipeline.commit(&fixture.ledger, &resolution).is_err());
        let failed_state = fixture
            .ledger
            .load_metadata_scan_state([failed_source])
            .unwrap();
        assert_eq!(
            failed_state.entries[0]
                .metadata_checkpoint
                .as_ref()
                .unwrap()
                .processing_status,
            crate::domain::CheckpointProcessingStatus::Pending
        );
        let successful_source = batch
            .groups
            .iter()
            .skip(1)
            .flat_map(|group| group.sources.first())
            .map(|source| source.source_file_id)
            .next()
            .expect("a second group source");
        let successful_state = fixture
            .ledger
            .load_metadata_scan_state([successful_source])
            .unwrap();
        assert_eq!(
            successful_state.entries[0]
                .metadata_checkpoint
                .as_ref()
                .unwrap()
                .processing_status,
            crate::domain::CheckpointProcessingStatus::Ready
        );
    }

    #[test]
    fn generated_fixture_records_the_performance_and_privacy_baseline() {
        let fixture = Fixture::new("performance-baseline");
        let expected_initial_bytes = [
            &fixture.main_path,
            &fixture.child_path,
            &fixture.archived_main_path,
        ]
        .into_iter()
        .map(|path| fs::metadata(path).unwrap().len())
        .sum::<u64>();

        let (initial_result, initial) = fixture.run_observed();
        initial_result.expect("generated fixture initial import");
        assert_eq!(initial.discovered_files, 3);
        assert_eq!(initial.body_open_attempts, 3);
        assert_eq!(initial.bytes_read, expected_initial_bytes);
        assert!(initial.guard_bytes_read > 0);
        assert!(initial.elapsed_ms >= 0);
        assert!(initial.peak_buffered_body_bytes > 0);
        assert!(
            initial.peak_buffered_body_bytes <= chunk_reader::MAX_BUFFERED_BODY_BYTES,
            "streaming body buffers must remain bounded independently of fixture size"
        );

        let (unchanged_result, unchanged) = fixture.run_observed();
        unchanged_result.expect("unchanged generated fixture scan");
        assert_eq!(unchanged.discovered_files, 3);
        assert_eq!(unchanged.skipped_sources, 3);
        assert_eq!(unchanged.body_open_attempts, 0);
        assert_eq!(unchanged.bytes_read, 0);
        assert_eq!(unchanged.guard_bytes_read, 0);
        assert_eq!(unchanged.usage_bytes_read, 0);

        let appended = br#"{"type":"event_msg","payload":{"type":"token_count","sentinel":"PERFORMANCE_BODY_SENTINEL"}}
"#;
        append(&fixture.main_path, appended);
        let (incremental_result, incremental) = fixture.run_observed();
        incremental_result.expect("generated fixture incremental scan");
        assert_eq!(incremental.body_open_attempts, 1);
        assert_eq!(incremental.bytes_read, appended.len() as u64);
        assert_eq!(incremental.usage_bytes_read, appended.len() as u64);
        assert!(incremental.guard_bytes_read > 0);
        assert!(
            incremental.guard_bytes_read <= 2 * chunk_reader::GUARD_WINDOW_BYTES,
            "incremental I/O is the appended range plus bounded guard reads"
        );
        let rendered_report = format!("{initial:?}{unchanged:?}{incremental:?}");
        assert!(!rendered_report.contains("PERFORMANCE_BODY_SENTINEL"));
        assert!(!rendered_report.contains("ROLL_OUT_BODY_SENTINEL"));
    }

    #[test]
    fn stable_usage_round_loads_only_the_empty_worklist() {
        let fixture = UsagePerformanceFixture::new("t-perf-004");
        fixture.stabilize();
        assert_eq!(fixture.thread_ids.len(), 6);

        let revision_before = data_revision(&fixture.ledger);
        let (result, report) = fixture.run_observed();
        result.expect("stable usage round");

        assert_eq!(report.usage_worklist_loads, 1);
        assert_eq!(report.usage_worklist_candidates, 0);
        assert_eq!(report.usage_detail_plan_loads, 0);
        assert_eq!(report.usage_detail_sources_loaded, 0);
        assert_eq!(report.usage_global_replans, 0);
        assert_eq!(report.body_open_attempts, 0);
        assert_eq!(report.usage_bytes_read, 0);
        assert_eq!(report.usage_events_inserted, 0);
        assert_eq!(report.usage_db_write_duration_ms, 0);
        assert_eq!(data_revision(&fixture.ledger), revision_before);
    }

    #[test]
    fn incremental_usage_round_loads_only_changed_threads() {
        let fixture = UsagePerformanceFixture::new("t-perf-005");
        fixture.stabilize();

        let source_ids = fixture
            .paths
            .iter()
            .map(|path| source_checkpoint(&fixture.ledger, path).0)
            .collect::<Vec<_>>();
        let unchanged_before = fixture.paths[3..]
            .iter()
            .map(|path| checkpoint_state(&fixture.ledger, path, "usage"))
            .collect::<Vec<_>>();
        let mut appended_bytes = 0;
        for (path, input_tokens) in fixture.paths[..3].iter().zip([10, 20, 30]) {
            let line = token_count_line(input_tokens, input_tokens);
            appended_bytes += line.len() as u64;
            append(path, &line);
        }

        let (result, report) = fixture.run_observed();
        result.expect("incremental usage round");
        let mut expected_detail_source_ids = source_ids[..3].to_vec();
        expected_detail_source_ids.sort_unstable();
        assert_eq!(report.usage_worklist_loads, 1);
        assert_eq!(report.usage_worklist_candidates, 3);
        assert_eq!(report.usage_global_replans, 0);
        assert_eq!(report.usage_detail_source_ids, expected_detail_source_ids);
        assert_eq!(report.usage_bytes_read, appended_bytes);
        assert_eq!(report.usage_events_inserted, 3);
        for path in &fixture.paths[..3] {
            assert_eq!(
                checkpoint_state(&fixture.ledger, path, "usage").1,
                fs::metadata(path).unwrap().len() as i64
            );
        }
        assert_eq!(
            fixture.paths[3..]
                .iter()
                .map(|path| checkpoint_state(&fixture.ledger, path, "usage"))
                .collect::<Vec<_>>(),
            unchanged_before
        );
        assert_eq!(active_usage_event_count(&fixture.ledger), 3);

        let connection = Connection::open(fixture.ledger.database_path()).unwrap();
        let sessions = AggregateReader::new(&connection)
            .sessions(
                TimeRange::new(0, i64::MAX).unwrap(),
                SessionPageRequest::new(10),
            )
            .unwrap();
        let root = sessions
            .rows
            .iter()
            .find(|row| row.root_session_id == fixture.thread_ids[0])
            .expect("main session aggregate");
        assert_eq!(root.self_usage.input_tokens, 10);
        assert_eq!(root.subagent_usage.input_tokens, 50);
        assert_eq!(root.inclusive_usage.input_tokens, 60);
        drop(connection);

        let (repeat_result, repeat) = fixture.run_observed();
        repeat_result.expect("unchanged round after incremental usage");
        assert_eq!(repeat.usage_events_inserted, 0);
        assert_eq!(active_usage_event_count(&fixture.ledger), 3);
    }

    #[test]
    fn usage_round_honors_the_frozen_discovery_boundary() {
        let fixture = UsagePerformanceFixture::new("t-perf-006");
        fixture.stabilize();
        let a_path = &fixture.paths[0];
        let b_path = &fixture.paths[1];
        append(a_path, &token_count_line(10, 10));
        let b_first = token_count_line(20, 20);
        let split = b_first.len() / 2;
        append(b_path, &b_first[..split]);

        let round_at_ms = now_ms();
        let discovery = Discovery::discover_at(&fixture.home, round_at_ms);
        let b_fixed_size = discovery
            .files
            .iter()
            .find(|file| file.path == *b_path)
            .expect("B in frozen discovery")
            .size;
        let mut report = ScanReport::new(round_at_ms);
        let carry_proofs = usage_consumer::collect_usage_carry_observation_proofs(
            &fixture.ledger,
            &discovery,
            &mut report,
        )
        .unwrap();
        let pipeline = MetadataPipeline::new(METADATA_PARSER_VERSION, round_at_ms).unwrap();
        let (outcome, _) = pipeline
            .record_and_load_with_usage_carry_proofs(&fixture.ledger, &discovery, &carry_proofs)
            .unwrap();
        let b_checkpoint_before = checkpoint_state(&fixture.ledger, b_path, "usage").1;

        append(b_path, &b_first[split..]);
        append(b_path, &token_count_line(25, 5));
        let state_snapshot = read_state_snapshot(&fixture.home.join("state_5.sqlite"));
        usage_consumer::run_usage_round(
            &fixture.ledger,
            &discovery,
            &outcome,
            &state_snapshot,
            &AtomicBool::new(false),
            &mut report,
        )
        .expect("usage round against frozen discovery");
        report.finish(now_ms());

        assert_eq!(report.usage_worklist_candidates, 2);
        assert_eq!(report.usage_events_inserted, 1);
        assert_eq!(
            checkpoint_state(&fixture.ledger, b_path, "usage").1,
            b_checkpoint_before
        );
        assert!(checkpoint_state(&fixture.ledger, b_path, "usage").1 <= b_fixed_size);

        let (next_result, next) = fixture.run_observed();
        next_result.expect("next discovery consumes B remainder");
        assert_eq!(next.usage_events_inserted, 2);
        assert_eq!(
            checkpoint_state(&fixture.ledger, b_path, "usage").1,
            fs::metadata(b_path).unwrap().len() as i64
        );
        assert_eq!(active_usage_event_count(&fixture.ledger), 3);

        let (repeat_result, repeat) = fixture.run_observed();
        repeat_result.expect("B remainder is not counted twice");
        assert_eq!(repeat.usage_events_inserted, 0);
        assert_eq!(active_usage_event_count(&fixture.ledger), 3);
    }

    #[test]
    fn multi_batch_usage_replans_only_the_current_thread() {
        let fixture = UsagePerformanceFixture::new("t-perf-007-local");
        fixture.stabilize();
        let path = &fixture.paths[0];
        let source_id = source_checkpoint(&fixture.ledger, path).0;
        let mut bytes = Vec::new();
        for total in 1..=2049 {
            bytes.extend(token_count_line(total, 1));
        }
        append(path, &bytes);

        let (result, report) = fixture.run_observed();
        result.expect("bounded multi-batch usage round");
        assert_eq!(report.usage_worklist_loads, 1);
        assert_eq!(report.usage_worklist_candidates, 1);
        assert_eq!(report.usage_global_replans, 0);
        assert_eq!(report.usage_detail_source_ids, vec![source_id]);
        assert!(report.usage_detail_plan_loads >= 3);
        assert_eq!(report.usage_events_inserted, 2049);
        assert_eq!(active_usage_event_count(&fixture.ledger), 2049);
        assert_eq!(
            checkpoint_state(&fixture.ledger, path, "usage").1,
            fs::metadata(path).unwrap().len() as i64
        );
    }

    #[test]
    fn epoch_and_parser_transitions_reload_the_lightweight_worklist() {
        let fixture = UsagePerformanceFixture::new("t-perf-007-global");
        let (initial_result, initial) = fixture.run_observed();
        initial_result.expect("epoch zero rebuild and activation");
        assert_eq!(initial.usage_global_replans, 1);
        assert_eq!(initial.usage_worklist_loads, 2);
        assert_eq!(usage_epochs(&fixture.ledger).1, None);

        Connection::open(fixture.ledger.database_path())
            .unwrap()
            .execute("UPDATE app_meta SET usage_parser_version=1 WHERE id=1", [])
            .unwrap();
        let (parser_result, parser_transition) = fixture.run_observed();
        parser_result.expect("parser mismatch rebuild and activation");
        assert_eq!(parser_transition.usage_global_replans, 1);
        assert_eq!(parser_transition.usage_worklist_loads, 2);
        assert_eq!(usage_epochs(&fixture.ledger).1, None);
    }

    #[test]
    fn repeated_shadow_rebuild_data_failure_quarantines_the_session_tree() {
        let fixture = UsagePerformanceFixture::new("t-perf-008-quarantine");
        fixture.stabilize();
        let active_before = usage_epochs(&fixture.ledger).0.expect("active epoch");
        let bad_source_id = source_checkpoint(&fixture.ledger, &fixture.paths[0]).0;

        let connection = Connection::open(fixture.ledger.database_path()).unwrap();
        connection
            .execute("UPDATE app_meta SET usage_parser_version=1 WHERE id=1", [])
            .unwrap();
        connection
            .execute(
                "UPDATE rollout_metadata_facts
                 SET owning_records_start_offset=1
                 WHERE source_file_id=?1",
                [bad_source_id],
            )
            .unwrap();
        drop(connection);

        let (result, report) = fixture.run_observed();
        result.expect("bad Session data is quarantined without failing the round");
        assert!(report.error_codes.contains(&"USAGE_SESSION_DATA_INVALID"));
        assert_eq!(report.failed_sources, 1);

        let (active_after, build_after, parser_after) = usage_epochs(&fixture.ledger);
        assert!(active_after.expect("new active epoch") > active_before);
        assert_eq!(build_after, None);
        assert_eq!(parser_after, crate::usage::USAGE_PARSER_VERSION);
        assert_eq!(active_quarantine_count(&fixture.ledger), 1);
    }

    #[test]
    fn ordinary_usage_group_error_does_not_block_the_next_thread() {
        let fixture = UsagePerformanceFixture::new("t-perf-008-isolation");
        fixture.stabilize();
        let failed_path = &fixture.paths[0];
        let successful_path = &fixture.paths[1];
        let failed_source_id = source_checkpoint(&fixture.ledger, failed_path).0;
        append(failed_path, &token_count_line(10, 10));
        fixture
            .run_observed()
            .0
            .expect("seed failed group processor state");
        let failed_checkpoint_before = checkpoint_state(&fixture.ledger, failed_path, "usage").1;
        Connection::open(fixture.ledger.database_path())
            .unwrap()
            .execute(
                "UPDATE usage_source_states SET previous_total_fingerprint=X'00'
                 WHERE source_file_id=?1
                   AND ledger_epoch=(SELECT usage_active_epoch FROM app_meta WHERE id=1)",
                [failed_source_id],
            )
            .unwrap();
        append(failed_path, &token_count_line(15, 5));
        append(successful_path, &token_count_line(20, 20));

        let (result, report) = fixture.run_observed();
        assert_eq!(result, Err("USAGE_GROUP_COMMIT_FAILED"));
        assert!(report.error_codes.contains(&"USAGE_GROUP_COMMIT_FAILED"));
        assert_eq!(report.failed_sources, 1);
        assert_eq!(report.usage_events_inserted, 1);
        assert_eq!(
            checkpoint_state(&fixture.ledger, failed_path, "usage").1,
            failed_checkpoint_before
        );
        assert_eq!(
            checkpoint_state(&fixture.ledger, successful_path, "usage").1,
            fs::metadata(successful_path).unwrap().len() as i64
        );
        assert_eq!(active_usage_event_count(&fixture.ledger), 2);
        assert_eq!(active_quarantine_count(&fixture.ledger), 0);
    }

    #[test]
    fn required_usage_reload_failures_have_fatal_control_flow() {
        let source = include_str!("usage_consumer.rs").replace("\r\n", "\n");
        assert!(source.contains("USAGE_WORKLIST_RELOAD_FAILED"));
        assert!(source.contains("USAGE_PLAN_RELOAD_FAILED"));
        assert!(source.contains(
            "UsageThreadOutcome::FatalReloadError(error_code) => {
                    report.failed_source();
                    report.error(error_code);
                    return Err(error_code);
                }"
        ));
        assert!(source.contains(
            "Err(error_code) => {
                            return UsageThreadOutcome::FatalReloadError(error_code);
                        }"
        ));
        assert!(!source.contains("load_scan_state(&present_ids"));
        assert!(!source.contains("load_scan_state(present_ids"));
    }
}
