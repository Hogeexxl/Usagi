use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use usagi::{
    antigravity::quota::AntigravityQuotaService,
    api::{AppContext, ProcessShutdown, QueryApi},
    codex::{
        CodexAdapter, CodexConfig, CodexSessionDetailSidecar, CodexSessionErrorSidecar,
        quota::CodexQuotaService,
    },
    domain::ScanResult,
    ingestion::{IngestionConfig, IngestionCoordinator, RequestDisposition},
    platform::{browser::SystemBrowser, paths},
    source::SourceRegistry,
    storage::{Ledger, LedgerOptions},
    update::UpdateService,
    usage::{
        AggregateError, SessionCompactionProjection, SessionDetail, SessionDetailSidecar,
        SessionDetailSnapshot, SessionErrorSidecar, SummaryQuery, TimeRange, UsageFilter,
        UsageLedger, UsageLedgerError,
    },
};

const FIXTURE_MANIFEST: &str = include_str!("fixtures/codex/compaction/manifest.json");
const FIXTURE_BYTES: &[(&str, &[u8])] = &[
    (
        "normal_only.jsonl",
        include_bytes!("fixtures/codex/compaction/normal_only.jsonl"),
    ),
    (
        "modern_one.jsonl",
        include_bytes!("fixtures/codex/compaction/modern_one.jsonl"),
    ),
    (
        "modern_two.jsonl",
        include_bytes!("fixtures/codex/compaction/modern_two.jsonl"),
    ),
    (
        "legacy_only.jsonl",
        include_bytes!("fixtures/codex/compaction/legacy_only.jsonl"),
    ),
    (
        "reset.jsonl",
        include_bytes!("fixtures/codex/compaction/reset.jsonl"),
    ),
    (
        "resume.jsonl",
        include_bytes!("fixtures/codex/compaction/resume.jsonl"),
    ),
    (
        "fork.jsonl",
        include_bytes!("fixtures/codex/compaction/fork.jsonl"),
    ),
    (
        "subagent.jsonl",
        include_bytes!("fixtures/codex/compaction/subagent.jsonl"),
    ),
];

const CODEX_USAGE_PARSER_VERSION: i64 = 12;
const CODEX_CANONICAL_ALGORITHM_VERSION: i64 = 6;

#[derive(Clone, Deserialize)]
struct Manifest {
    real_full_metering_fixtures: Vec<ManifestFixture>,
}

#[derive(Clone, Deserialize)]
struct ManifestFixture {
    fixture_file: String,
    fixture_sha256: String,
    fixture_line_count: usize,
    owning_thread_id_fixture: String,
    actual_expected: ActualExpected,
    #[serde(default)]
    responses_by_model_effort: BTreeMap<String, i64>,
}

#[derive(Clone, Deserialize)]
struct ActualExpected {
    unique_responses: i64,
    actual: SixDimensions,
    canonical_total_input_plus_output: i64,
    compaction_marker_count: i64,
    compaction_total: Option<i64>,
    compaction_responses_in_usage: i64,
}

#[derive(Clone, Deserialize)]
struct SixDimensions {
    input: i64,
    cached_input: i64,
    cache_write: i64,
    output: i64,
    reasoning: i64,
    raw_total: i64,
}

fn manifest_fixture(file_name: &str) -> ManifestFixture {
    serde_json::from_str::<Manifest>(FIXTURE_MANIFEST)
        .expect("compaction fixture manifest must be valid JSON")
        .real_full_metering_fixtures
        .into_iter()
        .find(|entry| entry.fixture_file == file_name)
        .unwrap_or_else(|| panic!("manifest has no full-metering fixture named {file_name}"))
}

fn fixture_bytes(file_name: &str) -> &'static [u8] {
    FIXTURE_BYTES
        .iter()
        .find_map(|(name, bytes)| (*name == file_name).then_some(*bytes))
        .unwrap_or_else(|| panic!("no embedded compaction fixture named {file_name}"))
}

fn verify_manifest_fixture(entry: &ManifestFixture) -> &'static [u8] {
    let bytes = fixture_bytes(&entry.fixture_file);
    let actual_hash = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        actual_hash, entry.fixture_sha256,
        "{} SHA-256",
        entry.fixture_file
    );
    assert_eq!(
        std::str::from_utf8(bytes)
            .expect("sanitized rollout fixture must be UTF-8")
            .lines()
            .count(),
        entry.fixture_line_count,
        "{} line count",
        entry.fixture_file
    );
    assert_eq!(
        entry.actual_expected.canonical_total_input_plus_output,
        entry.actual_expected.actual.raw_total,
        "manifest total definition for {}",
        entry.fixture_file
    );
    bytes
}

fn fixture_parent_thread_id(entry: &ManifestFixture) -> Option<String> {
    let bytes = verify_manifest_fixture(entry);
    std::str::from_utf8(bytes)
        .expect("sanitized rollout fixture must be UTF-8")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record.get("type").and_then(Value::as_str) == Some("session_meta"))
        .find_map(|record| {
            record
                .get("payload")
                .and_then(|payload| payload.get("parent_thread_id"))
                .and_then(Value::as_str)
                .filter(|parent| *parent != entry.owning_thread_id_fixture)
                .map(str::to_owned)
        })
}

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after Unix epoch")
            .as_nanos();
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "usagi-codex-compaction-{}-{stamp}-{sequence}",
            std::process::id(),
        ));
        fs::create_dir_all(&path).expect("create isolated compaction test root");
        Self(path)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct StateThread {
    parent_thread_id: Option<String>,
    rollout_path: Option<PathBuf>,
}

struct CompactionHarness {
    scanner: Option<usagi::ingestion::ScanHandle>,
    ledger: Arc<Ledger>,
    entries: Vec<ManifestFixture>,
    session_paths: BTreeMap<String, PathBuf>,
    home: PathBuf,
    database: PathBuf,
    _root: TempRoot,
}

struct DetailSidecarCallCount(AtomicU64);

impl SessionDetailSidecar for DetailSidecarCallCount {
    fn compaction_usage(
        &self,
        _connection: &Connection,
        _range: TimeRange,
        _filter: &UsageFilter,
        _detail: &SessionDetail,
    ) -> Result<Vec<SessionCompactionProjection>, AggregateError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(Vec::new())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct UsageScanProgress {
    epoch: (i64, Option<i64>, i64, Option<i64>),
    build_sources: Vec<(i64, String, Option<String>, Option<i64>, Option<i64>)>,
    checkpoints: Vec<(i64, i64, i64, String)>,
}

impl UsageScanProgress {
    fn is_ready(&self) -> bool {
        self.epoch.0 > 0
            && self.epoch.1.is_none()
            && self.build_sources.is_empty()
            && !self.checkpoints.is_empty()
            && self
                .checkpoints
                .iter()
                .all(|(_, parser, _, processing_status)| {
                    *parser == CODEX_USAGE_PARSER_VERSION && processing_status == "ready"
                })
    }
}

impl CompactionHarness {
    fn new(file_names: &[&str]) -> Self {
        let root = TempRoot::new();
        let home = root.0.join("codex");
        fs::create_dir_all(home.join("sessions")).expect("create temporary sessions directory");
        fs::create_dir_all(home.join("archived_sessions"))
            .expect("create temporary archived sessions directory");

        let entries = file_names
            .iter()
            .map(|name| manifest_fixture(name))
            .collect::<Vec<_>>();
        let mut session_paths = BTreeMap::new();
        for entry in &entries {
            let path = install_snapshot(&home, entry, "sessions");
            let previous = session_paths.insert(entry.fixture_file.clone(), path);
            assert!(
                previous.is_none(),
                "fixture installed twice: {}",
                entry.fixture_file
            );
        }
        write_state_stub(&home, &entries, &session_paths);

        let database = root.0.join("mu.sqlite3");
        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(&database))
                .expect("open isolated temporary usage ledger"),
        );
        Self {
            scanner: None,
            ledger,
            entries,
            session_paths,
            home,
            database,
            _root: root,
        }
    }

    fn start_and_wait(&mut self) {
        let mut registry = SourceRegistry::new();
        registry
            .register(CodexAdapter::new(CodexConfig::from_home(self.home.clone())))
            .expect("register Codex adapter against temporary home");
        self.scanner = Some(
            IngestionCoordinator::start(
                IngestionConfig::default(),
                Arc::clone(&self.ledger),
                registry,
            )
            .expect("start isolated Codex ingestion coordinator"),
        );
        self.wait_for_scan(None, "initial fixture scan");
        assert_scan_completed(self, "initial fixture scan");
        self.finish_pending_usage_build();
    }

    fn wait_for_scan(&self, wanted_id: Option<&str>, phase: &str) {
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let scan = self.ledger.app_state().expect("read scanner state").scan;
            let finished = match wanted_id {
                Some(id) => scan.last_finished_scan_id.as_deref() == Some(id),
                None => scan.last_finished_scan_id.is_some(),
            } && scan.active_scan_id.is_none();
            if finished {
                return;
            }
            if Instant::now() >= deadline {
                let progress = self.usage_scan_progress();
                let reports = self
                    .scanner
                    .as_ref()
                    .expect("scanner is running while waiting")
                    .last_source_reports();
                panic!(
                    "Codex fixture scan timed out at {phase}; wanted={wanted_id:?}; scan={scan:#?}; usage_progress={progress:#?}; source_reports={reports:#?}"
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn scan_now(&self) -> String {
        let scan_id = self.request_scan_and_wait();
        self.finish_pending_usage_build();
        scan_id
    }

    fn request_scan_and_wait(&self) -> String {
        let disposition = self
            .scanner
            .as_ref()
            .expect("scanner must be running")
            .request(usagi::domain::ScanTrigger::Manual)
            .expect("request manual fixture scan");
        let scan_id = match disposition {
            RequestDisposition::Started { scan_id, .. } => scan_id,
            RequestDisposition::Coalesced {
                followup_scan_id, ..
            } => followup_scan_id,
        };
        self.wait_for_scan(Some(&scan_id), "manual fixture scan");
        assert_scan_completed(self, "manual fixture scan");
        scan_id
    }

    fn usage_scan_progress(&self) -> UsageScanProgress {
        let epoch = self.active_epoch();
        let connection = Connection::open(&self.database).expect("open usage progress ledger");
        let build_sources = if let Some(build_epoch) = epoch.1 {
            let mut statement = connection
                .prepare(
                    "SELECT source_file_id,completion_status,completion_error_code,
                            completed_through_offset,required_through_offset
                     FROM codex_usage_build_sources WHERE build_epoch=?1
                     ORDER BY source_file_id",
                )
                .expect("prepare usage build progress query");
            statement
                .query_map([build_epoch], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .expect("query usage build progress")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("collect usage build progress")
        } else {
            Vec::new()
        };
        let mut statement = connection
            .prepare(
                "SELECT source_file_id,parser_version,committed_offset,processing_status
                 FROM codex_source_checkpoints WHERE consumer_kind='usage'
                 ORDER BY source_file_id",
            )
            .expect("prepare usage checkpoint progress query");
        let checkpoints = statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("query usage checkpoint progress")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect usage checkpoint progress");
        UsageScanProgress {
            epoch,
            build_sources,
            checkpoints,
        }
    }

    fn finish_pending_usage_build(&self) {
        const MAX_FOLLOWUP_SCANS: usize = 16;

        let mut previous = self.usage_scan_progress();
        for attempt in 1..=MAX_FOLLOWUP_SCANS {
            if previous.is_ready() {
                return;
            }
            let scan_id = self.request_scan_and_wait();
            let current = self.usage_scan_progress();
            assert_ne!(
                current, previous,
                "follow-up scan {attempt} ({scan_id}) must advance the pending usage build"
            );
            if previous.epoch.1.is_some() && current.epoch.1.is_some() {
                assert_eq!(
                    current.epoch.0, previous.epoch.0,
                    "active epoch remains stable while the replacement build is pending"
                );
            }
            previous = current;
        }
        assert!(
            previous.is_ready(),
            "usage build did not activate with ready checkpoints within {MAX_FOLLOWUP_SCANS} follow-up scans: {previous:?}"
        );
    }

    fn install_archived_snapshot(&self, entry: &ManifestFixture) -> PathBuf {
        install_snapshot(&self.home, entry, "archived_sessions")
    }

    fn source_file_id(&self, path: &Path) -> i64 {
        let normalized = paths::normalize_source_path(path).expect("normalize fixture source path");
        Connection::open(&self.database)
            .expect("open isolated ledger for source lookup")
            .query_row(
                "SELECT source_file_id FROM codex_source_files WHERE current_path=?1 AND file_status='present'",
                [normalized.to_str().expect("normalized path must be UTF-8")],
                |row| row.get(0),
            )
            .optional()
            .expect("query fixture source id")
            .unwrap_or_else(|| panic!("scanner did not register source {}", path.display()))
    }

    fn request_checkpoint_local_replay(&self, source_file_id: i64) -> i64 {
        let connection = Connection::open(&self.database).expect("open isolated replay ledger");
        let before: (i64, Option<Vec<u8>>, String) = connection
            .query_row(
                "SELECT committed_offset,guard_hash,processing_status
                 FROM codex_source_checkpoints
                 WHERE source_file_id=?1 AND consumer_kind='usage'",
                [source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read ready source checkpoint before local replay");
        assert_eq!(
            before.2, "ready",
            "local replay starts from a ready checkpoint"
        );
        let changed = connection
            .execute(
                "UPDATE codex_source_checkpoints
                 SET processing_status='rebuild_required',last_error_code=NULL
                 WHERE source_file_id=?1 AND consumer_kind='usage'",
                [source_file_id],
            )
            .expect("request replay through the existing usage planner");
        assert_eq!(changed, 1, "mark exactly the requested source for replay");
        before.0
    }

    fn assert_checkpoint_ready_at(&self, source_file_id: i64, offset: i64) {
        let checkpoint: (i64, String) = Connection::open(&self.database)
            .expect("open isolated checkpoint ledger")
            .query_row(
                "SELECT committed_offset,processing_status
                 FROM codex_source_checkpoints
                 WHERE source_file_id=?1 AND consumer_kind='usage'",
                [source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read source checkpoint after scan");
        assert_eq!(checkpoint, (offset, "ready".to_owned()));
    }

    fn active_epoch(&self) -> (i64, Option<i64>, i64, Option<i64>) {
        Connection::open(&self.database)
            .expect("open isolated epoch ledger")
            .query_row(
                "SELECT active_epoch,build_epoch,active_parser_version,build_parser_version
                 FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("read Codex epoch state")
    }

    fn usage_summary(&self) -> usagi::usage::UsageSummary {
        let error_sidecar = CodexSessionErrorSidecar;
        let sidecars: [&dyn SessionErrorSidecar; 1] = [&error_sidecar];
        UsageLedger::new(&self.ledger, &sidecars)
            .summary(SummaryQuery::new(
                TimeRange::new(0, i64::MAX).expect("valid full usage range"),
                UsageFilter::default(),
            ))
            .expect("read usage summary through the public ledger")
    }

    fn session_detail_snapshot(
        &self,
        root_session_id: String,
        expected_data_revision: Option<i64>,
    ) -> Result<SessionDetailSnapshot, UsageLedgerError> {
        let error_sidecar = CodexSessionErrorSidecar;
        let error_sidecars: [&dyn SessionErrorSidecar; 1] = [&error_sidecar];
        let detail_sidecar = CodexSessionDetailSidecar;
        let detail_sidecars: [&dyn SessionDetailSidecar; 1] = [&detail_sidecar];
        UsageLedger::new(&self.ledger, &error_sidecars).session_detail_snapshot(
            TimeRange::new(0, i64::MAX).expect("valid full detail range"),
            UsageFilter::default(),
            expected_data_revision,
            root_session_id,
            &detail_sidecars,
        )
    }

    fn fixture_root(&self, entry: &ManifestFixture) -> String {
        distinct_roots(
            &Connection::open(&self.database).expect("open ledger for fixture root lookup"),
            self.active_epoch().0,
            &entry.owning_thread_id_fixture,
        )
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("{} has no canonical root", entry.fixture_file))
    }

    fn seed_parser11_algorithm5_history(&self, old_active_epoch: i64, entry: &ManifestFixture) {
        assert_eq!(CODEX_USAGE_PARSER_VERSION, 12);
        assert_eq!(CODEX_CANONICAL_ALGORITHM_VERSION, 6);

        let mut connection =
            Connection::open(&self.database).expect("open isolated historical ledger fixture");
        let transaction = connection
            .transaction()
            .expect("begin isolated parser 11 / algorithm 5 history seed");
        let old_epoch = transaction
            .execute(
                "UPDATE source_usage_epochs SET active_parser_version=11
                 WHERE source='codex' AND active_epoch=?1 AND build_epoch IS NULL",
                [old_active_epoch],
            )
            .expect("seed the temporary historical epoch version");
        assert_eq!(old_epoch, 1, "seed exactly the active Codex epoch");
        let seeded_checkpoints = transaction
            .execute(
                "UPDATE codex_source_checkpoints SET parser_version=11
                 WHERE consumer_kind='usage'",
                [],
            )
            .expect("seed usage checkpoints from the parser 11 installation");
        assert!(
            seeded_checkpoints > 0,
            "historical seed has usage checkpoints"
        );
        let seeded_states = transaction
            .execute(
                r#"UPDATE codex_usage_source_states
                 SET usage_parser_version=11,canonical_algorithm_version=5,
                     reconciliation_state_json='{"version":1,"open_window_start_offset":null,"pending_response_ids":[],"modern_counter_domain":null,"modern_counter_total":null,"pending_evidence":[]}'
                 WHERE ledger_epoch=?1"#,
                [old_active_epoch],
            )
            .expect("seed old parser and canonical algorithm source-state rows");
        assert!(
            seeded_states > 0,
            "historical seed has source-state vectors"
        );

        // Parser 11 history had no schema-14 classification or hold rows.
        // The old canonical events remain ordinary usage rows with their
        // historical six-dimensional payloads in the active epoch.
        transaction
            .execute(
                "DELETE FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=?1",
                [old_active_epoch],
            )
            .expect("remove schema-14 markers from the historical epoch seed");
        transaction
            .execute(
                "DELETE FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=?1",
                [old_active_epoch],
            )
            .expect("remove schema-14 holds from the historical epoch seed");
        transaction
            .execute(
                "DELETE FROM codex_usage_reconciliation_windows
                 WHERE source='codex' AND ledger_epoch=?1",
                [old_active_epoch],
            )
            .expect("remove schema-14 reconciliation windows from historical epoch");
        transaction
            .execute(
                "DELETE FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=?1",
                [old_active_epoch],
            )
            .expect("remove schema-14 event classifications from historical epoch");
        seed_v5_source_state_fingerprints(&transaction, old_active_epoch);
        seed_v5_turn_fingerprints(&transaction, old_active_epoch);
        transaction
            .commit()
            .expect("commit isolated parser 11 / algorithm 5 history seed");
        drop(connection);

        assert_parser11_algorithm5_history(self, old_active_epoch, entry);
        assert_eq!(self.active_epoch(), (old_active_epoch, None, 11, None));
    }

    fn rebuild_seeded_v5_history_through_scanner(
        &self,
        old_active_epoch: i64,
        revision_before: i64,
        entry: &ManifestFixture,
    ) {
        self.seed_parser11_algorithm5_history(old_active_epoch, entry);
        assert_active_epoch_stays_visible(self, entry, old_active_epoch);

        let disposition = self
            .scanner
            .as_ref()
            .expect("scanner must be running")
            .request(usagi::domain::ScanTrigger::Manual)
            .expect("request parser upgrade through scanner");
        let scan_id = match disposition {
            RequestDisposition::Started { scan_id, .. } => scan_id,
            RequestDisposition::Coalesced {
                followup_scan_id, ..
            } => followup_scan_id,
        };

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut observed_building_epoch = false;
        loop {
            let epoch = self.active_epoch();
            if epoch.1.is_some() {
                assert_eq!(
                    epoch.0, old_active_epoch,
                    "old active epoch remains selected"
                );
                assert_eq!(epoch.1, Some(old_active_epoch + 1));
                assert_eq!(
                    epoch.2, 11,
                    "old active parser remains visible during build"
                );
                assert_eq!(epoch.3, Some(CODEX_USAGE_PARSER_VERSION));
                if build_has_private_evidence(&self.database, epoch.1.unwrap()) {
                    assert_active_epoch_stays_visible(self, entry, old_active_epoch);
                    assert_build_epoch_is_private(
                        &self.database,
                        old_active_epoch,
                        epoch.1.unwrap(),
                    );
                    observed_building_epoch = true;
                    break;
                }
            }
            let scan = self
                .ledger
                .app_state()
                .expect("read upgrade scan state")
                .scan;
            if scan.last_finished_scan_id.as_deref() == Some(&scan_id)
                && scan.active_scan_id.is_none()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "scanner did not expose a populated building epoch before activation"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            observed_building_epoch
                || self.active_epoch()
                    == (old_active_epoch + 1, None, CODEX_USAGE_PARSER_VERSION, None),
            "parser 11 / algorithm 5 history must remain visible during a populated shadow build or complete through normal activation before polling observes it"
        );

        self.wait_for_scan(Some(&scan_id), "parser 11 / algorithm 5 shadow rebuild");
        assert_scan_completed(
            self,
            "scanner rebuild from parser 11 / algorithm 5 history to parser 12 / algorithm 6",
        );
        const MAX_FOLLOWUP_SCANS: usize = 16;
        let mut progress = self.usage_scan_progress();
        for attempt in 1..=MAX_FOLLOWUP_SCANS {
            if progress.is_ready() {
                break;
            }
            let build_epoch = progress
                .epoch
                .1
                .expect("unfinished parser upgrade retains a build epoch");
            assert_eq!(progress.epoch.0, old_active_epoch);
            assert_eq!(build_epoch, old_active_epoch + 1);
            assert_eq!(progress.epoch.2, 11);
            assert_eq!(progress.epoch.3, Some(CODEX_USAGE_PARSER_VERSION));
            assert_active_epoch_stays_visible(self, entry, old_active_epoch);
            assert_build_epoch_is_private(&self.database, old_active_epoch, build_epoch);

            let previous = progress;
            let followup_id = self.request_scan_and_wait();
            progress = self.usage_scan_progress();
            assert_ne!(
                progress, previous,
                "follow-up shadow scan {attempt} ({followup_id}) must advance the private build"
            );
            if progress.epoch.1.is_some() {
                assert_eq!(
                    progress.epoch.0, old_active_epoch,
                    "parser 11 stays active while parser 12 build remains pending"
                );
                assert_eq!(progress.epoch.1, Some(old_active_epoch + 1));
                assert_eq!(progress.epoch.2, 11);
                assert_eq!(progress.epoch.3, Some(CODEX_USAGE_PARSER_VERSION));
                assert_active_epoch_stays_visible(self, entry, old_active_epoch);
                assert_build_epoch_is_private(&self.database, old_active_epoch, build_epoch);
            }
        }
        assert!(
            progress.is_ready(),
            "parser upgrade did not complete within {MAX_FOLLOWUP_SCANS} follow-up scans: {progress:?}"
        );
        assert_eq!(
            self.active_epoch(),
            (old_active_epoch + 1, None, CODEX_USAGE_PARSER_VERSION, None),
            "normal rebuild activation advances one epoch and clears build state"
        );
        assert!(
            self.ledger
                .app_state()
                .expect("read data revision")
                .data_revision
                > revision_before,
            "scanner activation publishes the rebuilt v12 / v6 canonical and Compaction evidence"
        );
    }

    fn shutdown(&mut self) {
        if let Some(scanner) = self.scanner.take() {
            scanner.shutdown().expect("stop temporary Codex scanner");
        }
    }
}

fn old_v5_fingerprint(vector: [Option<i64>; 6]) -> Option<Vec<u8>> {
    let [input, cached, cache_write, output, reasoning, total] = vector;
    let required = [input, cached, output, reasoning, total];
    if required.iter().all(Option::is_none) && cache_write.is_none() {
        return None;
    }
    assert!(
        required.iter().all(Option::is_some),
        "historical algorithm 5 vector must be complete or absent"
    );

    let mut bytes = Vec::with_capacity(8 * 7 + 1);
    bytes.extend_from_slice(&5_i64.to_be_bytes());
    bytes.extend_from_slice(&input.unwrap().to_be_bytes());
    bytes.extend_from_slice(&cached.unwrap().to_be_bytes());
    match cache_write {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        None => bytes.push(0),
    }
    bytes.extend_from_slice(&output.unwrap().to_be_bytes());
    bytes.extend_from_slice(&reasoning.unwrap().to_be_bytes());
    bytes.extend_from_slice(&total.unwrap().to_be_bytes());
    Some(blake3::hash(&bytes).as_bytes().to_vec())
}

fn read_v5_vector(
    row: &rusqlite::Row<'_>,
    first_column: usize,
) -> rusqlite::Result<[Option<i64>; 6]> {
    Ok([
        row.get(first_column)?,
        row.get(first_column + 1)?,
        row.get(first_column + 2)?,
        row.get(first_column + 3)?,
        row.get(first_column + 4)?,
        row.get(first_column + 5)?,
    ])
}

fn seed_v5_source_state_fingerprints(transaction: &rusqlite::Transaction<'_>, epoch: i64) {
    let mut statement = transaction
        .prepare(
            "SELECT source_file_id,previous_total_input_tokens,previous_total_cached_tokens,
                    previous_total_cache_write_tokens,previous_total_output_tokens,
                    previous_total_reasoning_tokens,previous_total_total_tokens
             FROM codex_usage_source_states WHERE ledger_epoch=?1",
        )
        .expect("prepare historical source-state vectors");
    let rows = statement
        .query_map([epoch], |row| {
            Ok((row.get::<_, i64>(0)?, read_v5_vector(row, 1)?))
        })
        .expect("read historical source-state vectors")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect historical source-state vectors");
    drop(statement);
    assert!(
        !rows.is_empty(),
        "historical epoch has source-state vectors"
    );

    for (source_file_id, vector) in rows {
        transaction
            .execute(
                "UPDATE codex_usage_source_states SET previous_total_fingerprint=?1
                 WHERE ledger_epoch=?2 AND source_file_id=?3",
                params![old_v5_fingerprint(vector), epoch, source_file_id],
            )
            .expect("write parser 11 / algorithm 5 source-state fingerprint");
    }
}

fn seed_v5_turn_fingerprints(transaction: &rusqlite::Transaction<'_>, epoch: i64) {
    let mut statement = transaction
        .prepare(
            "SELECT source_file_id,file_generation,turn_key,
                    start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,
                    start_total_output_tokens,start_total_reasoning_tokens,start_total_total_tokens,
                    last_total_input_tokens,last_total_cached_tokens,last_total_cache_write_tokens,
                    last_total_output_tokens,last_total_reasoning_tokens,last_total_total_tokens,
                    accounted_input_tokens,accounted_cached_tokens,accounted_cache_write_tokens,
                    accounted_output_tokens,accounted_reasoning_tokens,accounted_total_tokens
             FROM codex_turns WHERE ledger_epoch=?1",
        )
        .expect("prepare historical Turn vectors");
    let rows = statement
        .query_map([epoch], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                read_v5_vector(row, 3)?,
                read_v5_vector(row, 9)?,
                read_v5_vector(row, 15)?,
            ))
        })
        .expect("read historical Turn vectors")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect historical Turn vectors");
    drop(statement);
    assert!(
        !rows.is_empty(),
        "historical epoch has persisted Turn vectors"
    );

    for (source_file_id, file_generation, turn_key, start, last, accounted) in rows {
        transaction
            .execute(
                "UPDATE codex_turns SET start_total_fingerprint=?1,last_total_fingerprint=?2,
                    accounted_fingerprint=?3
                 WHERE ledger_epoch=?4 AND source_file_id=?5 AND file_generation=?6 AND turn_key=?7",
                params![
                    old_v5_fingerprint(start),
                    old_v5_fingerprint(last),
                    old_v5_fingerprint(accounted),
                    epoch,
                    source_file_id,
                    file_generation,
                    turn_key
                ],
            )
            .expect("write parser 11 / algorithm 5 Turn fingerprints");
    }
}

fn assert_parser11_algorithm5_history(
    harness: &CompactionHarness,
    epoch: i64,
    entry: &ManifestFixture,
) {
    let connection =
        Connection::open(&harness.database).expect("open isolated parser 11 / algorithm 5 history");
    let incompatible_sources: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM codex_usage_source_states
             WHERE ledger_epoch=?1 AND (usage_parser_version<>11 OR canonical_algorithm_version<>5)",
            [epoch],
            |row| row.get(0),
        )
        .expect("validate every historical source-state version");
    let source_states: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM codex_usage_source_states WHERE ledger_epoch=?1",
            [epoch],
            |row| row.get(0),
        )
        .expect("count historical source-state vectors");
    assert!(
        source_states > 0,
        "historical v5 state includes source vectors"
    );
    assert_eq!(
        incompatible_sources, 0,
        "all active source vectors use 11/5"
    );

    let turn_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM codex_turns WHERE ledger_epoch=?1",
            [epoch],
            |row| row.get(0),
        )
        .expect("count historical Turn vectors");
    assert!(turn_count > 0, "historical v5 state includes Turn vectors");
    let mut statement = connection
        .prepare(
            "SELECT source_file_id,previous_total_input_tokens,previous_total_cached_tokens,
                    previous_total_cache_write_tokens,previous_total_output_tokens,
                    previous_total_reasoning_tokens,previous_total_total_tokens,
                    previous_total_fingerprint
             FROM codex_usage_source_states WHERE ledger_epoch=?1",
        )
        .expect("prepare historical source-state fingerprint verification");
    let source_states = statement
        .query_map([epoch], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                read_v5_vector(row, 1)?,
                row.get::<_, Option<Vec<u8>>>(7)?,
            ))
        })
        .expect("read historical source-state fingerprints")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect historical source-state fingerprints");
    for (source_file_id, vector, fingerprint) in source_states {
        assert_eq!(
            fingerprint,
            old_v5_fingerprint(vector),
            "parser 11 / algorithm 5 source-state vector hash for source {source_file_id}"
        );
    }
    drop(statement);

    let mut statement = connection
        .prepare(
            "SELECT start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,
                    start_total_output_tokens,start_total_reasoning_tokens,start_total_total_tokens,
                    start_total_fingerprint,last_total_input_tokens,last_total_cached_tokens,
                    last_total_cache_write_tokens,last_total_output_tokens,last_total_reasoning_tokens,
                    last_total_total_tokens,last_total_fingerprint,accounted_input_tokens,
                    accounted_cached_tokens,accounted_cache_write_tokens,accounted_output_tokens,
                    accounted_reasoning_tokens,accounted_total_tokens,accounted_fingerprint
             FROM codex_turns WHERE ledger_epoch=?1",
        )
        .expect("prepare historical v5 fingerprint verification");
    let turns = statement
        .query_map([epoch], |row| {
            Ok((
                read_v5_vector(row, 0)?,
                row.get::<_, Option<Vec<u8>>>(6)?,
                read_v5_vector(row, 7)?,
                row.get::<_, Option<Vec<u8>>>(13)?,
                read_v5_vector(row, 14)?,
                row.get::<_, Option<Vec<u8>>>(20)?,
            ))
        })
        .expect("read historical Turn fingerprints")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect historical Turn fingerprints");
    for (start, start_hash, last, last_hash, accounted, accounted_hash) in turns {
        assert_eq!(
            start_hash,
            old_v5_fingerprint(start),
            "v5 start-vector hash"
        );
        assert_eq!(last_hash, old_v5_fingerprint(last), "v5 last-vector hash");
        assert_eq!(
            accounted_hash,
            old_v5_fingerprint(accounted),
            "v5 accounted-vector hash"
        );
    }
    drop(statement);

    let (usage_checkpoints, checkpoint_mismatches, nonready_checkpoints): (i64, i64, i64) =
        connection
            .query_row(
                "SELECT COUNT(*),
                    COALESCE(SUM(parser_version<>11),0),
                    COALESCE(SUM(processing_status<>'ready'),0)
             FROM codex_source_checkpoints WHERE consumer_kind='usage'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("validate old usage checkpoint parser versions and completion");
    assert!(
        usage_checkpoints > 0,
        "historical seed has usage checkpoints"
    );
    assert_eq!(
        checkpoint_mismatches, 0,
        "old usage checkpoints use parser 11"
    );
    assert_eq!(
        nonready_checkpoints, 0,
        "historical usage checkpoints are complete"
    );

    let old_private_rows: i64 = connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=?1)
              + (SELECT COUNT(*) FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=?1)
              + (SELECT COUNT(*) FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=?1)
              + (SELECT COUNT(*) FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=?1)",
            [epoch],
            |row| row.get(0),
        )
        .expect("count schema-14 private rows absent from historical parser-11 data");
    assert_eq!(
        old_private_rows, 0,
        "v5 history has no schema-14 evidence rows"
    );

    let actual = &entry.actual_expected.actual;
    let historical_totals: SixDimensions = connection
        .query_row(
            "SELECT COALESCE(SUM(input_tokens),0),COALESCE(SUM(cached_tokens),0),
                    COALESCE(SUM(cache_write_tokens),0),COALESCE(SUM(output_tokens),0),
                    COALESCE(SUM(reasoning_tokens),0),COALESCE(SUM(total_tokens),0)
             FROM usage_events WHERE source='codex' AND source_epoch=?1 AND thread_id=?2",
            params![epoch, entry.owning_thread_id_fixture],
            |row| {
                Ok(SixDimensions {
                    input: row.get(0)?,
                    cached_input: row.get(1)?,
                    cache_write: row.get(2)?,
                    output: row.get(3)?,
                    reasoning: row.get(4)?,
                    raw_total: row.get(5)?,
                })
            },
        )
        .expect("read historical canonical six-dimensional usage");
    assert_eq!(historical_totals.input, actual.input, "v5 input vector");
    assert_eq!(
        historical_totals.cached_input, actual.cached_input,
        "v5 cached-input vector"
    );
    assert_eq!(
        historical_totals.cache_write, actual.cache_write,
        "v5 cache-write vector"
    );
    assert_eq!(historical_totals.output, actual.output, "v5 output vector");
    assert_eq!(
        historical_totals.reasoning, actual.reasoning,
        "v5 reasoning vector"
    );
    assert_eq!(
        historical_totals.raw_total, actual.raw_total,
        "v5 canonical total"
    );
}

fn build_has_private_evidence(database: &Path, build_epoch: i64) -> bool {
    let connection = Connection::open(database).expect("open isolated building epoch");
    connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=?1)
              + (SELECT COUNT(*) FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=?1)
              + (SELECT COUNT(*) FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=?1)",
            [build_epoch],
            |row| row.get::<_, i64>(0),
        )
        .expect("read private evidence from the in-progress shadow epoch")
        > 0
}

fn assert_build_epoch_is_private(database: &Path, active_epoch: i64, build_epoch: i64) {
    let connection = Connection::open(database).expect("open isolated shadow-build ledger");
    let (build_parser, build_members, proved_members, active_private_rows):
        (Option<i64>, i64, i64, i64) = connection
        .query_row(
            "SELECT
                (SELECT MIN(target_parser_version) FROM codex_usage_build_sources WHERE build_epoch=?2),
                (SELECT COUNT(*) FROM codex_usage_build_sources WHERE build_epoch=?2),
                (SELECT COUNT(*) FROM codex_usage_build_sources
                 WHERE build_epoch=?2 AND active_state_fingerprint IS NOT NULL),
                (SELECT
                    (SELECT COUNT(*) FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=?1)
                  + (SELECT COUNT(*) FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=?1)
                  + (SELECT COUNT(*) FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=?1))",
            params![active_epoch, build_epoch],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("validate planner-owned build membership and old active visibility");
    assert_eq!(build_parser, Some(CODEX_USAGE_PARSER_VERSION));
    assert!(build_members > 0, "scanner planner created build members");
    assert_eq!(
        proved_members, build_members,
        "planner captured proof from every old v5 source-state vector"
    );
    assert_eq!(
        active_private_rows, 0,
        "building evidence does not leak into v5 active"
    );
}

impl Drop for CompactionHarness {
    fn drop(&mut self) {
        if let Some(scanner) = self.scanner.take() {
            let _ = scanner.shutdown();
        }
    }
}

fn install_snapshot(home: &Path, entry: &ManifestFixture, area: &str) -> PathBuf {
    let bytes = verify_manifest_fixture(entry);
    let file_name = format!("rollout-{}.jsonl", entry.owning_thread_id_fixture);
    let path = home.join(area).join(file_name);
    fs::write(&path, bytes).expect("write exact sanitized rollout fixture to temporary Codex home");
    path
}

fn write_state_stub(
    home: &Path,
    entries: &[ManifestFixture],
    session_paths: &BTreeMap<String, PathBuf>,
) {
    let mut threads = BTreeMap::<String, StateThread>::new();
    for entry in entries {
        let owner = entry.owning_thread_id_fixture.clone();
        let parent = fixture_parent_thread_id(entry);
        let owner_state = threads.entry(owner.clone()).or_default();
        owner_state.parent_thread_id = parent.clone();
        owner_state.rollout_path = session_paths.get(&entry.fixture_file).cloned();
        if let Some(parent) = parent {
            threads.entry(parent).or_default();
        }
    }

    let connection = Connection::open(home.join("state_5.sqlite"))
        .expect("create temporary Codex state metadata stub");
    connection
        .execute_batch(
            "CREATE TABLE threads (
                id TEXT NOT NULL, rollout_path TEXT, created_at_ms INTEGER, updated_at_ms INTEGER,
                archived INTEGER, cwd TEXT, title TEXT, name TEXT, model TEXT, agent_role TEXT
             );
             CREATE TABLE thread_spawn_edges (
                parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL,
                status TEXT, observed_at_ms INTEGER
             );",
        )
        .expect("create the existing Codex metadata fixture schema");

    for (thread_id, state) in &threads {
        let role = if state.parent_thread_id.is_some() {
            "subagent"
        } else {
            "main"
        };
        let rollout_path = state
            .rollout_path
            .as_ref()
            .map(|path| path.to_str().expect("temporary path must be UTF-8"));
        connection
            .execute(
                "INSERT INTO threads(
                    id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role
                 ) VALUES (?1,?2,1,2,0,'/temporary/fixture','Fixture',NULL,'fixture-model',?3)",
                params![thread_id, rollout_path, role],
            )
            .expect("insert temporary main, child, or parent-stub thread");
        if let Some(parent) = &state.parent_thread_id {
            connection
                .execute(
                    "INSERT INTO thread_spawn_edges(
                        parent_thread_id,child_thread_id,status,observed_at_ms
                     ) VALUES (?1,?2,'spawned',3)",
                    params![parent, thread_id],
                )
                .expect("insert temporary parent relationship");
        }
    }
    drop(connection);

    let mut index = Vec::new();
    for thread_id in threads.keys() {
        index.extend_from_slice(
            serde_json::to_string(&serde_json::json!({
                "id": thread_id,
                "thread_name": format!("fixture-{thread_id}")
            }))
            .expect("serialize temporary session index row")
            .as_bytes(),
        );
        index.push(b'\n');
    }
    fs::write(home.join("session_index.jsonl"), index)
        .expect("write temporary Codex session index");
}

fn assert_scan_completed(harness: &CompactionHarness, phase: &str) {
    let state = harness
        .ledger
        .app_state()
        .expect("read completed scan state");
    assert_eq!(
        state.scan.last_finished_scan_result,
        Some(ScanResult::Completed),
        "{phase} failed with {:?}",
        state.scan.last_scan_error_code
    );
    assert!(
        state.scan.active_scan_id.is_none(),
        "{phase} is fully quiescent"
    );
}

fn assert_manifest_actual(harness: &CompactionHarness, entry: &ManifestFixture) {
    assert_manifest_actual_with_marker_count(
        harness,
        entry,
        entry.actual_expected.compaction_marker_count,
    );
}

fn assert_manifest_actual_with_marker_count(
    harness: &CompactionHarness,
    entry: &ManifestFixture,
    expected_marker_count: i64,
) {
    let actual = &entry.actual_expected.actual;
    let summary = harness.usage_summary();
    assert_eq!(
        summary.totals.input_tokens, actual.input,
        "{} input",
        entry.fixture_file
    );
    assert_eq!(
        summary.totals.cached_tokens, actual.cached_input,
        "{} cached input",
        entry.fixture_file
    );
    assert_eq!(
        summary.totals.cache_write_tokens,
        Some(actual.cache_write),
        "{} cache write",
        entry.fixture_file
    );
    assert_eq!(
        summary.totals.output_tokens, actual.output,
        "{} output",
        entry.fixture_file
    );
    assert_eq!(
        summary.totals.reasoning_tokens, actual.reasoning,
        "{} reasoning",
        entry.fixture_file
    );
    assert_eq!(
        summary.totals.total_tokens, actual.raw_total,
        "{} canonical input+output total",
        entry.fixture_file
    );

    let (active_epoch, build_epoch, active_parser, build_parser) = harness.active_epoch();
    assert!(
        active_epoch > 0,
        "{} has an active canonical epoch",
        entry.fixture_file
    );
    assert_eq!(build_epoch, None, "{} build completed", entry.fixture_file);
    assert_eq!(active_parser, CODEX_USAGE_PARSER_VERSION);
    assert_eq!(build_parser, None);

    let connection = Connection::open(&harness.database).expect("open isolated result ledger");
    assert_schema14(&connection, &entry.fixture_file);
    let (responses, compaction_responses, compaction_total): (i64, i64, i64) = connection
        .query_row(
            "SELECT
                (SELECT COUNT(DISTINCT f.response_id)
                 FROM codex_usage_event_facts f
                 WHERE f.source='codex' AND f.ledger_epoch=?1
                   AND f.owning_thread_id=?2 AND f.evidence_kind='explicit'
                   AND f.response_id IS NOT NULL),
                (SELECT COUNT(DISTINCT f.response_id)
                 FROM codex_usage_event_facts f
                 WHERE f.source='codex' AND f.ledger_epoch=?1
                   AND f.owning_thread_id=?2 AND f.operation='compaction'),
                (SELECT COALESCE(SUM(e.total_tokens),0)
                 FROM codex_usage_event_facts f JOIN usage_events e
                   ON e.source=f.source AND e.source_epoch=f.ledger_epoch AND e.event_id=f.event_id
                 WHERE f.source='codex' AND f.ledger_epoch=?1
                   AND f.owning_thread_id=?2 AND f.operation='compaction')",
            params![active_epoch, entry.owning_thread_id_fixture],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query unique response identities and Compaction classification");
    assert_eq!(
        responses, entry.actual_expected.unique_responses,
        "{} response identities",
        entry.fixture_file
    );
    assert_eq!(
        compaction_responses, entry.actual_expected.compaction_responses_in_usage,
        "{} classified Compaction response identities",
        entry.fixture_file
    );
    let (explicit_fact_rows, distinct_response_ids, missing_response_ids): (i64, i64, i64) =
        connection
            .query_row(
                "SELECT COUNT(*),COUNT(DISTINCT response_id),COALESCE(SUM(response_id IS NULL),0)
                 FROM codex_usage_event_facts
                 WHERE source='codex' AND ledger_epoch=?1 AND owning_thread_id=?2
                   AND evidence_kind='explicit'",
                params![active_epoch, entry.owning_thread_id_fixture],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("query explicit response identity cardinality");
    assert_eq!(
        (
            explicit_fact_rows,
            distinct_response_ids,
            missing_response_ids
        ),
        (
            entry.actual_expected.unique_responses,
            entry.actual_expected.unique_responses,
            0
        ),
        "{} explicit facts preserve one non-null row per response identity",
        entry.fixture_file
    );

    let marker_counts: (i64, i64) = connection
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(resolved_event_id IS NULL AND unknown_reason IS NOT NULL),0)
             FROM codex_compaction_markers
             WHERE source='codex' AND ledger_epoch=?1 AND owning_thread_id=?2",
            params![active_epoch, entry.owning_thread_id_fixture],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query persisted Compaction marker classification state");
    assert_eq!(
        marker_counts.0, expected_marker_count,
        "{} physical Compaction markers",
        entry.fixture_file
    );

    match entry.actual_expected.compaction_total {
        Some(expected) => {
            assert_eq!(
                compaction_total, expected,
                "{} unique Compaction total",
                entry.fixture_file
            );
            assert_eq!(
                marker_counts.1, 0,
                "{} measured markers are resolved",
                entry.fixture_file
            );
        }
        None => {
            assert!(
                marker_counts.0 > 0,
                "{} has an explicit unknown marker",
                entry.fixture_file
            );
            assert_eq!(
                marker_counts.1, marker_counts.0,
                "{} markers remain unknown",
                entry.fixture_file
            );
            assert_eq!(
                compaction_responses, 0,
                "{} unknown marker creates no measured Compaction",
                entry.fixture_file
            );
        }
    }

    let expected_root =
        fixture_parent_thread_id(entry).unwrap_or_else(|| entry.owning_thread_id_fixture.clone());
    let roots = distinct_roots(&connection, active_epoch, &entry.owning_thread_id_fixture);
    assert_eq!(
        roots,
        vec![expected_root.clone()],
        "{} usage keeps its fixture ownership root",
        entry.fixture_file
    );
    let foreign_owner_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM usage_events
             WHERE source='codex' AND source_epoch=?1
               AND (thread_id<>?2 OR root_session_id<>?3)",
            params![active_epoch, entry.owning_thread_id_fixture, expected_root],
            |row| row.get(0),
        )
        .expect("count canonical events with a different owner or root");
    assert_eq!(
        foreign_owner_events, 0,
        "{} canonical usage never borrows ancestor history or another root",
        entry.fixture_file
    );
    let fact_owner_mismatches: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM codex_usage_event_facts f
             LEFT JOIN usage_events e
               ON e.source=f.source AND e.source_epoch=f.ledger_epoch AND e.event_id=f.event_id
             WHERE f.source='codex' AND f.ledger_epoch=?1
               AND (e.event_id IS NULL OR e.thread_id<>f.owning_thread_id
                    OR e.root_session_id<>?2)",
            params![active_epoch, expected_root],
            |row| row.get(0),
        )
        .expect("count fact and canonical owner mismatches");
    assert_eq!(
        fact_owner_mismatches, 0,
        "{} fact ownership agrees with canonical thread and fixture root",
        entry.fixture_file
    );
    let marker_binding_mismatches: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM codex_compaction_markers m
             LEFT JOIN codex_usage_event_facts f
               ON f.source=m.source AND f.ledger_epoch=m.ledger_epoch
              AND f.event_id=m.resolved_event_id
             LEFT JOIN usage_events e
               ON e.source=m.source AND e.source_epoch=m.ledger_epoch
              AND e.event_id=m.resolved_event_id
             WHERE m.source='codex' AND m.ledger_epoch=?1
               AND (m.owning_thread_id<>?2 OR m.root_session_id<>?3
                    OR (m.resolved_event_id IS NOT NULL AND
                        (f.event_id IS NULL OR f.owning_thread_id<>m.owning_thread_id
                         OR f.response_id<>m.response_id OR f.operation<>'compaction'
                         OR e.thread_id<>m.owning_thread_id
                         OR e.root_session_id<>m.root_session_id)))",
            params![active_epoch, entry.owning_thread_id_fixture, expected_root],
            |row| row.get(0),
        )
        .expect("count marker and canonical ownership mismatches");
    assert_eq!(
        marker_binding_mismatches, 0,
        "{} marker binding agrees with its owning thread, root, response, and canonical event",
        entry.fixture_file
    );
    let owning_actual: i64 = connection
        .query_row(
            "SELECT COALESCE(SUM(total_tokens),0) FROM usage_events
             WHERE source='codex' AND source_epoch=?1 AND thread_id=?2",
            params![active_epoch, entry.owning_thread_id_fixture],
            |row| row.get(0),
        )
        .expect("sum canonical usage for the owning thread");
    assert_eq!(
        owning_actual, actual.raw_total,
        "{} owning-thread actual",
        entry.fixture_file
    );

    assert_eq!(
        response_model_effort_counts(&connection, active_epoch, &entry.owning_thread_id_fixture),
        manifest_model_effort_counts(entry),
        "{} response model and effort remain the manifest values",
        entry.fixture_file
    );

    let incompatible_source_states: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM codex_usage_source_states
             WHERE ledger_epoch=?1 AND
               (usage_parser_version<>?2 OR canonical_algorithm_version<>?3)",
            params![
                active_epoch,
                CODEX_USAGE_PARSER_VERSION,
                CODEX_CANONICAL_ALGORITHM_VERSION
            ],
            |row| row.get(0),
        )
        .expect("validate active source parser and algorithm versions");
    assert_eq!(
        incompatible_source_states, 0,
        "{} source state uses parser 12 / algorithm 6",
        entry.fixture_file
    );
    let incomplete_checkpoints: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM codex_source_checkpoints
             WHERE consumer_kind='usage' AND processing_status<>'ready'",
            [],
            |row| row.get(0),
        )
        .expect("count non-ready usage checkpoints");
    assert_eq!(
        incomplete_checkpoints, 0,
        "{} scanner checkpoints are complete",
        entry.fixture_file
    );

    let orphan_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM usage_events e
             WHERE e.source='codex' AND e.source_epoch=?1
               AND NOT EXISTS (
                   SELECT 1 FROM codex_usage_event_occurrences o
                   WHERE o.source=e.source AND o.ledger_epoch=e.source_epoch
                     AND o.event_id=e.event_id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM codex_compaction_markers m
                   WHERE m.source=e.source AND m.ledger_epoch=e.source_epoch
                     AND m.resolved_event_id=e.event_id
               )",
            [active_epoch],
            |row| row.get(0),
        )
        .expect("count active canonical events without a source reference");
    assert_eq!(
        orphan_events, 0,
        "{} active epoch contains no orphan events",
        entry.fixture_file
    );
    let active_holds: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM codex_usage_event_holds
             WHERE source='codex' AND ledger_epoch=?1",
            [active_epoch],
            |row| row.get(0),
        )
        .expect("count active replay or carry holds");
    assert_eq!(
        active_holds, 0,
        "{} completed epoch has no temporary holds",
        entry.fixture_file
    );
    let remaining_build_members: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM codex_usage_build_sources",
            [],
            |row| row.get(0),
        )
        .expect("count build members after activation");
    assert_eq!(
        remaining_build_members, 0,
        "{} completed activation has no pending carry members or cursors",
        entry.fixture_file
    );
    let inconsistent_carry_cursors: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM codex_usage_build_sources
             WHERE (carry_phase='none' AND
                    (carry_from_epoch IS NOT NULL OR carry_after_start_offset IS NOT NULL
                     OR carry_after_turn_key IS NOT NULL OR carry_after_anomaly_id IS NOT NULL
                     OR carry_after_fact_event_id IS NOT NULL
                     OR carry_after_marker_start_offset IS NOT NULL
                     OR carry_after_window_start_offset IS NOT NULL))
                OR (carry_phase<>'none' AND carry_from_epoch IS NULL)",
            [],
            |row| row.get(0),
        )
        .expect("check carry phase and cursor consistency");
    assert_eq!(
        inconsistent_carry_cursors, 0,
        "{} carry cursor fields match their phase",
        entry.fixture_file
    );
    assert_foreign_keys_clean(&connection, &entry.fixture_file);
}

fn assert_schema14(connection: &Connection, label: &str) {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read SQLite schema version");
    assert_eq!(version, 14, "{label} database uses schema 14");

    for table in [
        "codex_usage_event_facts",
        "codex_compaction_markers",
        "codex_usage_reconciliation_windows",
        "codex_usage_event_holds",
    ] {
        assert_schema_object(connection, "table", table, label);
    }
    for index in [
        "codex_usage_response_identity_idx",
        "codex_compaction_marker_scope_idx",
        "codex_compaction_marker_response_idx",
        "codex_usage_window_thread_idx",
        "codex_usage_event_hold_event_idx",
    ] {
        assert_schema_object(connection, "index", index, label);
    }
    for trigger in [
        "codex_usage_fact_owner_insert",
        "codex_usage_fact_owner_update",
        "codex_compaction_marker_binding_insert",
        "codex_compaction_marker_binding_update",
        "codex_usage_bound_fact_update",
        "codex_usage_bound_fact_delete",
    ] {
        assert_schema_object(connection, "trigger", trigger, label);
    }
}

fn assert_schema_object(connection: &Connection, kind: &str, name: &str, label: &str) {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type=?1 AND name=?2",
            params![kind, name],
            |row| row.get(0),
        )
        .expect("query schema object");
    assert_eq!(count, 1, "{label} schema has {kind} {name}");
}

fn assert_active_epoch_stays_visible(
    harness: &CompactionHarness,
    entry: &ManifestFixture,
    expected_epoch: i64,
) {
    let expected = &entry.actual_expected;
    let summary = harness.usage_summary();
    assert_eq!(summary.totals.input_tokens, expected.actual.input);
    assert_eq!(summary.totals.cached_tokens, expected.actual.cached_input);
    assert_eq!(
        summary.totals.cache_write_tokens,
        Some(expected.actual.cache_write)
    );
    assert_eq!(summary.totals.output_tokens, expected.actual.output);
    assert_eq!(summary.totals.reasoning_tokens, expected.actual.reasoning);
    assert_eq!(summary.totals.total_tokens, expected.actual.raw_total);
    assert_eq!(
        harness.active_epoch().0,
        expected_epoch,
        "shadow build does not replace the seeded parser-11 active epoch"
    );

    let connection = Connection::open(&harness.database)
        .expect("open isolated ledger while the shadow epoch is building");
    let active_private_rows: i64 = connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=?1)
              + (SELECT COUNT(*) FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=?1)
              + (SELECT COUNT(*) FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=?1)
              + (SELECT COUNT(*) FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=?1)",
            [expected_epoch],
            |row| row.get(0),
        )
        .expect("verify schema-14 rows remain absent from old active epoch");
    assert_eq!(
        active_private_rows, 0,
        "new private evidence remains isolated while the old active epoch is visible"
    );
    let root = harness.fixture_root(entry);
    let detail = harness
        .session_detail_snapshot(root, None)
        .expect("read detail while parser 11 remains active")
        .value;
    let compaction_values = detail_compaction_values(&detail);
    assert!(!compaction_values.is_empty());
    assert!(
        compaction_values.iter().all(Option::is_none),
        "parser 11 detail stays unknown while parser 12 is building"
    );
}

fn detail_compaction_values(detail: &SessionDetail) -> Vec<Option<i64>> {
    detail
        .main
        .model_usage
        .iter()
        .map(|model| model.compaction_tokens)
        .chain(detail.subagents.iter().flat_map(|subagent| {
            subagent
                .model_usage
                .iter()
                .map(|model| model.compaction_tokens)
        }))
        .collect()
}

fn distinct_roots(connection: &Connection, epoch: i64, owner: &str) -> Vec<String> {
    let mut statement = connection
        .prepare(
            "SELECT DISTINCT root_session_id FROM usage_events
             WHERE source='codex' AND source_epoch=?1 AND thread_id=?2
             ORDER BY root_session_id",
        )
        .expect("prepare owning root query");
    statement
        .query_map(params![epoch, owner], |row| row.get(0))
        .expect("query owning roots")
        .map(Result::unwrap)
        .collect()
}

fn manifest_model_effort_counts(
    entry: &ManifestFixture,
) -> BTreeMap<(String, Option<String>), i64> {
    entry
        .responses_by_model_effort
        .iter()
        .map(|(key, count)| {
            let inner = key
                .strip_prefix("('")
                .and_then(|value| value.strip_suffix("')"))
                .unwrap_or_else(|| panic!("invalid model/effort manifest key {key:?}"));
            let (model, effort) = inner
                .split_once("', '")
                .unwrap_or_else(|| panic!("invalid model/effort manifest key {key:?}"));
            ((model.to_owned(), Some(effort.to_owned())), *count)
        })
        .collect()
}

fn response_model_effort_counts(
    connection: &Connection,
    epoch: i64,
    owner: &str,
) -> BTreeMap<(String, Option<String>), i64> {
    let mut statement = connection
        .prepare(
            "SELECT e.model,e.reasoning_effort,COUNT(DISTINCT f.response_id)
             FROM codex_usage_event_facts f JOIN usage_events e
               ON e.source=f.source AND e.source_epoch=f.ledger_epoch AND e.event_id=f.event_id
             WHERE f.source='codex' AND f.ledger_epoch=?1 AND f.owning_thread_id=?2
               AND f.evidence_kind='explicit' AND f.response_id IS NOT NULL
             GROUP BY e.model,e.reasoning_effort ORDER BY e.model,e.reasoning_effort",
        )
        .expect("prepare response model and effort query");
    statement
        .query_map(params![epoch, owner], |row| {
            Ok(((row.get(0)?, row.get(1)?), row.get(2)?))
        })
        .expect("query response model and effort counts")
        .map(Result::unwrap)
        .collect()
}

fn assert_foreign_keys_clean(connection: &Connection, label: &str) {
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .expect("prepare foreign key check");
    let violations = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .expect("run foreign key check")
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "{label} foreign key violations: {violations:?}"
    );
}

fn assert_fixture(file_name: &str) {
    let entry = manifest_fixture(file_name);
    verify_manifest_fixture(&entry);
    let mut harness = CompactionHarness::new(&[file_name]);
    harness.start_and_wait();
    assert_manifest_actual(&harness, &entry);
    harness.shutdown();
}

#[test]
fn compaction_normal_only() {
    assert_fixture("normal_only.jsonl");
}

#[test]
fn compaction_modern_one() {
    assert_fixture("modern_one.jsonl");
}

#[test]
fn compaction_modern_two() {
    assert_fixture("modern_two.jsonl");
}

#[test]
fn compaction_legacy_only_unknown_marker() {
    assert_fixture("legacy_only.jsonl");
}

#[test]
fn compaction_reset_preserves_actual_across_counter_reset() {
    assert_fixture("reset.jsonl");
}

#[test]
fn compaction_resume_excludes_inherited_baseline() {
    let entry = manifest_fixture("resume.jsonl");
    let first_response = std::str::from_utf8(verify_manifest_fixture(&entry))
        .expect("resume fixture must be UTF-8")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| record.get("type").and_then(Value::as_str) == Some("token_usage_record"))
        .expect("resume fixture contains explicit usage");
    let cumulative_tokens = first_response["payload"]["thread_token_usage"]["total_tokens"]
        .as_i64()
        .expect("resume fixture has cumulative thread usage");
    let response_tokens = first_response["payload"]["usage"]["total_tokens"]
        .as_i64()
        .expect("resume fixture has response usage");
    assert_eq!(
        cumulative_tokens - response_tokens,
        2_038_283,
        "resume fixture keeps its inherited cumulative baseline"
    );
    let mut harness = CompactionHarness::new(&["resume.jsonl"]);
    harness.start_and_wait();
    assert_manifest_actual(&harness, &entry);
    harness.shutdown();
}

#[test]
fn compaction_fork_excludes_ancestor_history() {
    let entry = manifest_fixture("fork.jsonl");
    assert!(
        fixture_parent_thread_id(&entry).is_some(),
        "fork fixture has a parent metadata stub"
    );
    let mut harness = CompactionHarness::new(&["fork.jsonl"]);
    harness.start_and_wait();
    assert_manifest_actual(&harness, &entry);
    harness.shutdown();
}

#[test]
fn compaction_detail_scope_real_subagent_usage_belongs_to_owning_child() {
    let entry = manifest_fixture("subagent.jsonl");
    assert!(
        fixture_parent_thread_id(&entry).is_some(),
        "subagent fixture has a parent metadata stub"
    );
    let mut harness = CompactionHarness::new(&["subagent.jsonl"]);
    harness.start_and_wait();
    assert_manifest_actual(&harness, &entry);

    let root = harness.fixture_root(&entry);
    let revision = harness
        .ledger
        .app_state()
        .expect("read revision for stale detail request")
        .data_revision;
    let detail = harness
        .session_detail_snapshot(root.clone(), Some(revision))
        .expect("read detail with active parser 12")
        .value;
    assert_eq!(
        detail.main.inclusive_usage.total_tokens, entry.actual_expected.actual.raw_total,
        "Compaction enrichment leaves root inclusive usage unchanged"
    );
    assert!(
        detail
            .main
            .model_usage
            .iter()
            .all(|model| model.compaction_tokens.is_none())
    );
    let child = detail
        .subagents
        .iter()
        .find(|subagent| subagent.thread_id == entry.owning_thread_id_fixture)
        .expect("real subagent fixture remains attached to its owning child");
    let child_compaction = child
        .model_usage
        .iter()
        .find(|model| {
            model.model == "gpt-6.1-sol" && model.reasoning_effort.as_deref() == Some("medium")
        })
        .expect("fixture child has the classified model and effort");
    assert_eq!(child_compaction.compaction_tokens, Some(240_412));
    assert_eq!(
        child_compaction.compaction_tokens, entry.actual_expected.compaction_total,
        "the preserved subagent fixture records 240412 compaction tokens"
    );
    let calls = DetailSidecarCallCount(AtomicU64::new(0));
    let detail_sidecars: [&dyn SessionDetailSidecar; 1] = [&calls];
    let error_sidecar = CodexSessionErrorSidecar;
    let error_sidecars: [&dyn SessionErrorSidecar; 1] = [&error_sidecar];
    let stale = UsageLedger::new(&harness.ledger, &error_sidecars).session_detail_snapshot(
        TimeRange::new(0, i64::MAX).unwrap(),
        UsageFilter::default(),
        Some(revision + 1),
        root,
        &detail_sidecars,
    );
    assert!(matches!(stale, Err(UsageLedgerError::StaleDataRevision)));
    assert_eq!(calls.0.load(Ordering::Relaxed), 0);
    harness.shutdown();
}

async fn api_detail(
    client: &reqwest::Client,
    base_url: &str,
    root: &str,
    query: &str,
) -> (u16, Value) {
    let response = client
        .get(format!(
            "{base_url}/api/usage/sessions/{root}/detail?{query}"
        ))
        .send()
        .await
        .expect("request session detail through loopback HTTP");
    let status = response.status().as_u16();
    let body = response
        .json::<Value>()
        .await
        .expect("decode session detail API response");
    (status, body)
}

#[tokio::test]
async fn compaction_system_acceptance_api_smoke() {
    let entry = manifest_fixture("subagent.jsonl");
    let root =
        fixture_parent_thread_id(&entry).expect("subagent fixture records its owning root thread");
    let mut harness = CompactionHarness::new(&["subagent.jsonl"]);
    harness.start_and_wait();
    assert_manifest_actual(&harness, &entry);

    let root_rollout = harness.home.join("sessions/rollout-phase5-root.jsonl");
    let root_timestamp = "2026-10-02T12:00:00.000Z";
    let root_records = [
        json!({
            "timestamp": root_timestamp,
            "ordinal": 0,
            "type": "session_meta",
            "payload": {
                "id": root,
                "session_id": root,
                "timestamp": root_timestamp,
                "cwd": "/temporary/fixture",
                "agent_role": "main",
                "originator": "Codex Desktop"
            }
        }),
        json!({
            "timestamp": root_timestamp,
            "ordinal": 1,
            "type": "turn_context",
            "payload": {
                "turn_id": "phase5-root-turn",
                "model": "phase5-root-model",
                "effort": "high"
            }
        }),
        json!({
            "timestamp": root_timestamp,
            "ordinal": 2,
            "type": "token_usage_record",
            "payload": {
                "thread_id": root,
                "turn_id": "phase5-root-turn",
                "session_id": root,
                "root_turn_id": "phase5-root-turn",
                "response_id": "resp_phase5_root_smoke",
                "usage": {
                    "input_tokens": 7,
                    "cached_input_tokens": 2,
                    "cache_write_input_tokens": 0,
                    "output_tokens": 3,
                    "reasoning_output_tokens": 1,
                    "total_tokens": 10
                }
            }
        }),
        json!({
            "timestamp": root_timestamp,
            "ordinal": 3,
            "type": "compacted",
            "payload": {
                "window_number": 1,
                "first_window_id": "phase5-root-window",
                "previous_window_id": "phase5-root-window",
                "window_id": "phase5-root-window-next",
                "compaction_response_id": null,
                "latest_token_usage_record": null
            }
        }),
    ];
    let root_bytes = root_records
        .iter()
        .flat_map(|record| {
            let mut line = serde_json::to_vec(record).expect("serialize temporary root evidence");
            line.push(b'\n');
            line
        })
        .collect::<Vec<_>>();
    fs::write(&root_rollout, root_bytes).expect("write temporary root rollout");
    let state = Connection::open(harness.home.join("state_5.sqlite"))
        .expect("open temporary Codex thread metadata");
    let updated = state
        .execute(
            "UPDATE threads SET rollout_path=?1 WHERE id=?2",
            params![
                root_rollout.to_str().expect("temporary path is UTF-8"),
                root
            ],
        )
        .expect("bind temporary parent thread to its rollout");
    assert_eq!(
        updated, 1,
        "parent stub now owns the temporary root rollout"
    );
    drop(state);
    harness.scan_now();

    let (unknown_marker_count, unknown_reason, resolved_event_id): (
        i64,
        Option<String>,
        Option<String>,
    ) = Connection::open(&harness.database)
        .expect("open isolated ledger for Main unknown marker proof")
        .query_row(
            "SELECT COUNT(*),MAX(unknown_reason),MAX(resolved_event_id)
             FROM codex_compaction_markers
             WHERE source='codex' AND ledger_epoch=?1
               AND owning_thread_id=?2 AND root_session_id=?2 AND model='phase5-root-model'",
            params![harness.active_epoch().0, root],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read Main model's unresolved Compaction marker");
    assert_eq!(unknown_marker_count, 1);
    assert!(
        unknown_reason.is_some(),
        "Main has a recorded unknown reason"
    );
    assert_eq!(resolved_event_id, None, "Main marker has no resolved event");

    let static_dir = harness._root.0.join("static");
    fs::create_dir_all(&static_dir).expect("create isolated API static directory");
    fs::write(
        static_dir.join("index.html"),
        "<html>compaction smoke</html>",
    )
    .expect("write isolated API fallback page");
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind API smoke server to an ephemeral loopback port");
    let address = listener.local_addr().expect("read API smoke address");
    let (process_shutdown, _process_shutdown_receiver) = ProcessShutdown::channel();
    let app = QueryApi::router_with_shutdown_on_port(
        AppContext {
            ledger: Arc::clone(&harness.ledger),
            scanner: harness
                .scanner
                .as_ref()
                .expect("fixture scanner remains active")
                .clone(),
            source_registry: SourceRegistry::new(),
            codex_quota_service: CodexQuotaService::unavailable(&harness.home),
            antigravity_quota_service: AntigravityQuotaService::unavailable(),
            codex_session_error_sidecar: Arc::new(CodexSessionErrorSidecar),
            update_service: UpdateService::unavailable(),
            browser_opener: Arc::new(SystemBrowser),
        },
        static_dir,
        process_shutdown,
        address.port(),
    )
    .expect("build existing QueryApi router");
    let (server_shutdown_tx, server_shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = server_shutdown_rx.await;
            })
            .await
    });
    let client = reqwest::Client::new();
    let base_url = format!("http://{address}");

    let (status, detail) = api_detail(
        &client,
        &base_url,
        &root,
        "range=custom&from=2026-10-02&to=2026-10-05",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(detail["root_session_id"], root);
    let main_models = detail["main"]["model_usage"]
        .as_array()
        .expect("Main model usage is an array");
    assert_eq!(main_models.len(), 1);
    assert_eq!(main_models[0]["model"], "phase5-root-model");
    assert!(
        main_models[0]
            .as_object()
            .unwrap()
            .contains_key("compaction_tokens")
    );
    assert!(
        main_models[0]["compaction_tokens"].is_null(),
        "the recorded unresolved Main marker makes its sibling nullable"
    );
    assert!(
        !main_models[0]["usage"]
            .as_object()
            .unwrap()
            .contains_key("compaction_tokens")
    );
    assert_eq!(detail["main"]["self_usage"]["total_tokens"], 10);
    assert_eq!(
        detail["main"]["inclusive_usage"]["total_tokens"],
        entry.actual_expected.actual.raw_total + 10
    );
    let children = detail["subagents"]
        .as_array()
        .expect("subagents is an array");
    assert_eq!(children.len(), 1);
    let child = &children[0];
    assert_eq!(child["thread_id"], entry.owning_thread_id_fixture);
    assert_eq!(child["parent_thread_id"], root);
    assert_eq!(child["root_session_id"], root);
    assert_eq!(child["model_usage"].as_array().unwrap().len(), 1);
    assert!(
        child["model_usage"][0]
            .as_object()
            .unwrap()
            .contains_key("compaction_tokens")
    );
    assert_eq!(
        child["model_usage"][0]["compaction_tokens"],
        entry.actual_expected.compaction_total.unwrap()
    );
    assert!(
        !child["model_usage"][0]["usage"]
            .as_object()
            .unwrap()
            .contains_key("compaction_tokens")
    );
    assert_eq!(
        child["model_usage"][0]["usage"]["total_tokens"],
        entry.actual_expected.actual.raw_total
    );

    let (status, root_day) = api_detail(
        &client,
        &base_url,
        &root,
        "range=custom&from=2026-10-02&to=2026-10-02",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(root_day["main"]["self_usage"]["total_tokens"], 10);
    assert_eq!(root_day["main"]["inclusive_usage"]["total_tokens"], 10);
    assert_eq!(root_day["subagents"].as_array().unwrap().len(), 0);
    assert!(
        root_day["main"]["model_usage"][0]
            .as_object()
            .unwrap()
            .contains_key("compaction_tokens")
    );
    assert!(root_day["main"]["model_usage"][0]["compaction_tokens"].is_null());

    let (status, child_day) = api_detail(
        &client,
        &base_url,
        &root,
        "range=custom&from=2026-10-03&to=2026-10-05",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(child_day["main"]["self_usage"]["total_tokens"], 0);
    assert!(
        child_day["main"]["model_usage"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(child_day["subagents"].as_array().unwrap().len(), 1);
    assert_eq!(
        child_day["subagents"][0]["model_usage"][0]["compaction_tokens"],
        entry.actual_expected.compaction_total.unwrap()
    );
    let start_ms = child_day["range"]["start_ms"]
        .as_i64()
        .expect("child-day response has a range start");
    let end_ms = child_day["range"]["end_ms"]
        .as_i64()
        .expect("child-day response has a range end");
    let expected_child_day_total: i64 = Connection::open(&harness.database)
        .expect("open isolated ledger for exact child-day range total")
        .query_row(
            "SELECT COALESCE(SUM(total_tokens),0) FROM usage_events
             WHERE source='codex' AND source_epoch=?1 AND root_session_id=?2
               AND occurred_at_ms>=?3 AND occurred_at_ms<?4",
            params![harness.active_epoch().0, root, start_ms, end_ms],
            |row| row.get(0),
        )
        .expect("sum canonical usage inside the requested day");
    assert_eq!(
        child_day["main"]["inclusive_usage"]["total_tokens"], expected_child_day_total,
        "HTTP detail total equals canonical usage inside the requested date range"
    );

    let stale_revision = detail["data_revision"]
        .as_i64()
        .expect("detail response has a data revision")
        + 1;
    let (status, stale) = api_detail(
        &client,
        &base_url,
        &root,
        &format!(
            "range=custom&from=2026-10-02&to=2026-10-05&expected_data_revision={stale_revision}"
        ),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(stale["error"]["code"], "STALE_DATA_REVISION");

    server_shutdown_tx
        .send(())
        .expect("signal API smoke server shutdown");
    server
        .await
        .expect("join API smoke server task")
        .expect("API smoke server exits cleanly");
    harness.shutdown();
}

#[test]
fn compaction_upgrade_readiness() {
    for file_name in ["normal_only.jsonl", "modern_one.jsonl"] {
        let entry = manifest_fixture(file_name);
        let mut harness = CompactionHarness::new(&[file_name]);
        harness.start_and_wait();

        let root = harness.fixture_root(&entry);
        let initially_ready = harness
            .session_detail_snapshot(root.clone(), None)
            .expect("read detail from ready parser 12 epoch")
            .value;
        let initial_values = detail_compaction_values(&initially_ready);
        assert!(!initial_values.is_empty());
        assert!(initial_values.iter().all(Option::is_some));
        assert_eq!(
            initial_values
                .iter()
                .map(|value| value.unwrap())
                .sum::<i64>(),
            entry.actual_expected.compaction_total.unwrap_or(0),
            "{} active detail total",
            file_name
        );

        let old_epoch = harness.active_epoch().0;
        let revision_before = harness
            .ledger
            .app_state()
            .expect("read revision before parser upgrade")
            .data_revision;
        harness.rebuild_seeded_v5_history_through_scanner(old_epoch, revision_before, &entry);

        let activated = harness
            .session_detail_snapshot(root, None)
            .expect("read detail after parser 12 activation")
            .value;
        let activated_values = detail_compaction_values(&activated);
        assert!(!activated_values.is_empty());
        assert!(activated_values.iter().all(Option::is_some));
        assert_eq!(
            activated_values
                .iter()
                .map(|value| value.unwrap())
                .sum::<i64>(),
            entry.actual_expected.compaction_total.unwrap_or(0),
            "{} activated detail total",
            file_name
        );
        assert_eq!(
            activated.main.inclusive_usage.total_tokens, entry.actual_expected.actual.raw_total,
            "{} root inclusive usage remains unchanged",
            file_name
        );
        assert!(
            harness
                .ledger
                .app_state()
                .expect("read revision after parser activation")
                .data_revision
                > revision_before,
            "{} activation publishes the detail projection revision",
            file_name
        );
        harness.shutdown();
    }
}

#[test]
fn compaction_lifecycle_replay_snapshot_and_shadow_rebuild() {
    let entry = manifest_fixture("reset.jsonl");
    let mut harness = CompactionHarness::new(&["reset.jsonl"]);
    harness.start_and_wait();
    assert_manifest_actual(&harness, &entry);

    let initial_epoch = harness.active_epoch().0;
    let initial_responses = entry.actual_expected.unique_responses;
    let first_source = harness
        .session_paths
        .get(&entry.fixture_file)
        .expect("primary source path is retained")
        .clone();
    let first_source_id = harness.source_file_id(&first_source);

    harness.scan_now();
    assert_eq!(
        harness.active_epoch().0,
        initial_epoch,
        "unchanged second scan stays in the active epoch"
    );
    assert_manifest_actual(&harness, &entry);

    let archive_source = harness.install_archived_snapshot(&entry);
    harness.scan_now();
    let archive_source_id = harness.source_file_id(&archive_source);
    assert_ne!(
        first_source_id, archive_source_id,
        "snapshot copies have distinct source identities"
    );
    assert_manifest_actual_with_marker_count(
        &harness,
        &entry,
        entry.actual_expected.compaction_marker_count * 2,
    );

    let connection = Connection::open(&harness.database).expect("open snapshot occurrence ledger");
    let source_counts: (i64, i64, i64) = connection
        .query_row(
            "SELECT COUNT(*),MIN(snapshot_count),MAX(snapshot_count) FROM (
                SELECT f.response_id,COUNT(DISTINCT o.source_file_id) AS snapshot_count
                FROM codex_usage_event_facts f JOIN codex_usage_event_occurrences o
                  ON o.source='codex' AND o.ledger_epoch=f.ledger_epoch AND o.event_id=f.event_id
                WHERE f.source='codex' AND f.ledger_epoch=?1
                  AND f.owning_thread_id=?2 AND f.response_id IS NOT NULL
                GROUP BY f.response_id
             )",
            params![harness.active_epoch().0, entry.owning_thread_id_fixture],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("count response identities represented by physical snapshots");
    assert_eq!(
        source_counts,
        (initial_responses, 2, 2),
        "each response binds across both snapshots"
    );
    drop(connection);

    let replay_offset = harness.request_checkpoint_local_replay(first_source_id);
    harness.scan_now();
    harness.assert_checkpoint_ready_at(first_source_id, replay_offset);
    assert_eq!(
        harness.active_epoch().0,
        initial_epoch,
        "local replay does not switch epochs"
    );
    assert_manifest_actual_with_marker_count(
        &harness,
        &entry,
        entry.actual_expected.compaction_marker_count * 2,
    );
    let replayed_source_counts: (i64, i64, i64) = Connection::open(&harness.database)
        .expect("open replay verification ledger")
        .query_row(
            "SELECT COUNT(*),MIN(snapshot_count),MAX(snapshot_count) FROM (
                SELECT f.response_id,COUNT(DISTINCT o.source_file_id) AS snapshot_count
                FROM codex_usage_event_facts f JOIN codex_usage_event_occurrences o
                  ON o.source='codex' AND o.ledger_epoch=f.ledger_epoch AND o.event_id=f.event_id
                WHERE f.source='codex' AND f.ledger_epoch=?1
                  AND f.owning_thread_id=?2 AND f.response_id IS NOT NULL
                GROUP BY f.response_id
             )",
            params![harness.active_epoch().0, entry.owning_thread_id_fixture],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("count response snapshots after source-local replay");
    assert_eq!(replayed_source_counts, (initial_responses, 2, 2));

    let revision_before_v5_rebuild = harness
        .ledger
        .app_state()
        .expect("read revision before parser upgrade")
        .data_revision;
    harness.rebuild_seeded_v5_history_through_scanner(
        initial_epoch,
        revision_before_v5_rebuild,
        &entry,
    );
    assert_manifest_actual_with_marker_count(
        &harness,
        &entry,
        entry.actual_expected.compaction_marker_count * 2,
    );
    assert_eq!(harness.active_epoch().0, initial_epoch + 1);
    harness.shutdown();
}
