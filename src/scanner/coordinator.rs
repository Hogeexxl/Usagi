//! Serialized scan scheduling and durable lifecycle coordination.
//!
//! File discovery and parsing stay behind `ScanWorker`; this module only
//! decides when one worker may run and persists every externally observable
//! lifecycle transition before acknowledging it.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use uuid::Uuid;

use crate::{
    domain::{
        FollowupStartFailedEvent, FollowupStartedEvent, FollowupState, ReserveScanFollowupEvent,
        ScanCompletedEvent, ScanFailedEvent, ScanLifecycleState, ScanStartEvent, ScanState,
        ScanTrigger,
    },
    storage::{Ledger, StorageError, StorageErrorKind},
};

const DEFAULT_INTERVAL: Duration = Duration::from_secs(300);
const MIN_INTERVAL: Duration = Duration::from_secs(60);
const MAX_INTERVAL: Duration = Duration::from_secs(3_600);
const COMMAND_CAPACITY: usize = 32;
const RETRY_BASE: Duration = Duration::from_millis(25);
const RETRY_MAX: Duration = Duration::from_secs(1);

const RECOVERING: u8 = 0;
const READY: u8 = 1;
const SOURCE_CHANGED: u8 = 2;
const SHUTTING_DOWN: u8 = 3;
const STOPPED: u8 = 4;

static NEXT_SCAN_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanConfig {
    pub codex_home: PathBuf,
    pub interval: Duration,
}

impl ScanConfig {
    pub fn new(codex_home: impl Into<PathBuf>) -> Self {
        Self {
            codex_home: codex_home.into(),
            interval: DEFAULT_INTERVAL,
        }
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn validate(&self) -> Result<(), ScanConfigError> {
        if !self.codex_home.is_absolute() {
            return Err(ScanConfigError::CodexHomeNotAbsolute);
        }
        if !(MIN_INTERVAL..=MAX_INTERVAL).contains(&self.interval) {
            return Err(ScanConfigError::IntervalOutOfRange);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanConfigError {
    CodexHomeNotAbsolute,
    IntervalOutOfRange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitFailureKind {
    Busy,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanRequestError {
    SourceChanged,
    Recovering,
    ShuttingDown,
    StartCommitFailed { kind: CommitFailureKind },
    EnqueueCommitFailed { kind: CommitFailureKind },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestDisposition {
    Started {
        scan_id: String,
        started_status_revision: i64,
    },
    Coalesced {
        followup_scan_id: String,
        enqueued_status_revision: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanShutdownError {
    CoordinatorUnavailable,
    Persistence(CommitFailureKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanStartError {
    InvalidConfig(ScanConfigError),
    CoordinatorUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerResult {
    Completed,
    Failed(&'static str),
}

pub(crate) trait ScanWorker: Send + Sync + 'static {
    fn run(&self, scan_id: &str, cancellation: &AtomicBool) -> WorkerResult;
}

trait LifecycleStore: Send + Sync + 'static {
    fn scan_state(&self) -> Result<ScanState, StorageError>;
    fn mark_started(&self, event: ScanStartEvent) -> Result<ScanState, StorageError>;
    fn reserve_followup(&self, event: ReserveScanFollowupEvent) -> Result<ScanState, StorageError>;
    fn mark_followup_started(&self, event: FollowupStartedEvent)
    -> Result<ScanState, StorageError>;
    fn mark_followup_start_failed(
        &self,
        event: FollowupStartFailedEvent,
    ) -> Result<ScanState, StorageError>;
    fn mark_completed(&self, event: ScanCompletedEvent) -> Result<ScanState, StorageError>;
    fn mark_failed(&self, event: ScanFailedEvent) -> Result<ScanState, StorageError>;
}

impl LifecycleStore for Ledger {
    fn scan_state(&self) -> Result<ScanState, StorageError> {
        Ok(self.scan_status_snapshot(None)?.app_state.scan)
    }

    fn mark_started(&self, event: ScanStartEvent) -> Result<ScanState, StorageError> {
        self.mark_scan_started(event)
    }

    fn reserve_followup(&self, event: ReserveScanFollowupEvent) -> Result<ScanState, StorageError> {
        self.reserve_scan_followup(event)
    }

    fn mark_followup_started(
        &self,
        event: FollowupStartedEvent,
    ) -> Result<ScanState, StorageError> {
        self.mark_followup_started(event)
    }

    fn mark_followup_start_failed(
        &self,
        event: FollowupStartFailedEvent,
    ) -> Result<ScanState, StorageError> {
        self.mark_followup_start_failed(event)
    }

    fn mark_completed(&self, event: ScanCompletedEvent) -> Result<ScanState, StorageError> {
        self.mark_scan_completed(event)
    }

    fn mark_failed(&self, event: ScanFailedEvent) -> Result<ScanState, StorageError> {
        self.mark_scan_failed(event)
    }
}

enum Command {
    Request {
        trigger: ScanTrigger,
        reply: SyncSender<Result<RequestDisposition, ScanRequestError>>,
    },
    WorkerFinished {
        scan_id: String,
        result: WorkerResult,
    },
    Shutdown {
        reply: SyncSender<Result<(), ScanShutdownError>>,
    },
}

#[derive(Clone)]
pub struct ScanHandle {
    commands: SyncSender<Command>,
    availability: Arc<AtomicU8>,
}

impl ScanHandle {
    pub fn request(&self, trigger: ScanTrigger) -> Result<RequestDisposition, ScanRequestError> {
        match self.availability.load(Ordering::Acquire) {
            RECOVERING => return Err(ScanRequestError::Recovering),
            SOURCE_CHANGED => return Err(ScanRequestError::SourceChanged),
            SHUTTING_DOWN | STOPPED => return Err(ScanRequestError::ShuttingDown),
            READY => {}
            _ => return Err(ScanRequestError::Recovering),
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Request {
                trigger,
                reply: reply_tx,
            })
            .map_err(|_| ScanRequestError::ShuttingDown)?;
        reply_rx
            .recv()
            .unwrap_or(Err(ScanRequestError::ShuttingDown))
    }

    pub fn shutdown(&self) -> Result<(), ScanShutdownError> {
        let previous = self.availability.swap(SHUTTING_DOWN, Ordering::AcqRel);
        if previous == STOPPED {
            return Ok(());
        }
        if previous == SHUTTING_DOWN {
            return Err(ScanShutdownError::CoordinatorUnavailable);
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Shutdown { reply: reply_tx })
            .map_err(|_| ScanShutdownError::CoordinatorUnavailable)?;
        reply_rx
            .recv()
            .unwrap_or(Err(ScanShutdownError::CoordinatorUnavailable))
    }
}

pub struct ScanCoordinator;

impl ScanCoordinator {
    pub(crate) fn start(
        config: ScanConfig,
        ledger: Arc<Ledger>,
        worker: Arc<dyn ScanWorker>,
    ) -> Result<ScanHandle, ScanStartError> {
        config.validate().map_err(ScanStartError::InvalidConfig)?;
        Self::start_with_store(config, ledger, worker)
    }

    fn start_with_store(
        config: ScanConfig,
        store: Arc<dyn LifecycleStore>,
        worker: Arc<dyn ScanWorker>,
    ) -> Result<ScanHandle, ScanStartError> {
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let availability = Arc::new(AtomicU8::new(RECOVERING));
        let loop_commands = commands.clone();
        let loop_availability = Arc::clone(&availability);
        thread::Builder::new()
            .name("usagi-scan-coordinator".to_owned())
            .spawn(move || {
                EventLoop::new(
                    config,
                    store,
                    worker,
                    receiver,
                    loop_commands,
                    loop_availability,
                )
                .run();
            })
            .map_err(|_| ScanStartError::CoordinatorUnavailable)?;
        Ok(ScanHandle {
            commands,
            availability,
        })
    }
}

struct ActiveWorker {
    scan_id: String,
    cancellation: Arc<AtomicBool>,
}

struct EventLoop {
    interval: Duration,
    store: Arc<dyn LifecycleStore>,
    worker: Arc<dyn ScanWorker>,
    receiver: Receiver<Command>,
    commands: SyncSender<Command>,
    availability: Arc<AtomicU8>,
    active: Option<ActiveWorker>,
    retry_at: Option<Instant>,
    pending_terminal: Option<(String, WorkerResult)>,
    pending_followup_failure: Option<(String, &'static str)>,
    retry_attempt: u32,
    shutdown_reply: Option<SyncSender<Result<(), ScanShutdownError>>>,
}

impl EventLoop {
    fn new(
        config: ScanConfig,
        store: Arc<dyn LifecycleStore>,
        worker: Arc<dyn ScanWorker>,
        receiver: Receiver<Command>,
        commands: SyncSender<Command>,
        availability: Arc<AtomicU8>,
    ) -> Self {
        Self {
            interval: config.interval,
            store,
            worker,
            receiver,
            commands,
            availability,
            active: None,
            retry_at: None,
            pending_terminal: None,
            pending_followup_failure: None,
            retry_attempt: 0,
            shutdown_reply: None,
        }
    }

    fn run(mut self) {
        let mut next_tick = Instant::now() + self.interval;
        if !self.recover() {
            self.availability.store(STOPPED, Ordering::Release);
            return;
        }
        if self.availability.load(Ordering::Acquire) == RECOVERING {
            self.availability.store(READY, Ordering::Release);
        }

        loop {
            let now = Instant::now();
            let next_event = self.retry_at.into_iter().chain([next_tick]).min().unwrap();
            let timeout = next_event.saturating_duration_since(now);
            match self.receiver.recv_timeout(timeout) {
                Ok(command) => {
                    if self.handle_command(command) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    let now = Instant::now();
                    if self.retry_at.is_some_and(|deadline| deadline <= now) {
                        self.retry_at = None;
                        if let Some((scan_id, result)) = self.pending_terminal.take() {
                            self.handle_terminal(scan_id, result);
                        } else if let Some((scan_id, error_code)) =
                            self.pending_followup_failure.take()
                        {
                            self.persist_followup_failure(&scan_id, error_code);
                        } else {
                            self.try_start_queued();
                        }
                    }
                    if next_tick <= now {
                        if self.availability.load(Ordering::Acquire) == READY {
                            self.process_request(ScanTrigger::Scheduled, None);
                        }
                        // Skip missed periods instead of replaying each elapsed tick.
                        next_tick = Instant::now() + self.interval;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.cancel_active();
                    break;
                }
            }
        }
        self.availability.store(STOPPED, Ordering::Release);
    }

    fn recover(&mut self) -> bool {
        loop {
            let state = match self.store.scan_state() {
                Ok(state) => state,
                Err(error) if is_busy(&error) => {
                    thread::sleep(self.next_retry_delay());
                    continue;
                }
                Err(error) if is_source_changed(&error) => {
                    self.availability.store(SOURCE_CHANGED, Ordering::Release);
                    return true;
                }
                Err(_) => return false,
            };

            if let Some(active_scan_id) = state.active_scan_id {
                let event = ScanFailedEvent::new(active_scan_id, now_ms(), "SCAN_INTERRUPTED")
                    .expect("fixed recovery event is valid");
                match self.store.mark_failed(event) {
                    Ok(_) => {
                        self.retry_attempt = 0;
                        continue;
                    }
                    Err(error) if is_busy(&error) => {
                        thread::sleep(self.next_retry_delay());
                        continue;
                    }
                    Err(_) => return false,
                }
            }

            if self.availability.load(Ordering::Acquire) == SHUTTING_DOWN {
                return true;
            }

            if state.followup_state == Some(FollowupState::Queued) {
                let scan_id = state
                    .followup_scan_id
                    .expect("validated queued state has an id");
                let event = FollowupStartedEvent::new(scan_id.clone(), now_ms())
                    .expect("fixed follow-up event is valid");
                match self.store.mark_followup_started(event) {
                    Ok(_) => {
                        self.retry_attempt = 0;
                        self.spawn_worker(scan_id);
                        return true;
                    }
                    Err(error) if is_busy(&error) => {
                        thread::sleep(self.next_retry_delay());
                        continue;
                    }
                    Err(error) => {
                        let code = if is_source_changed(&error) {
                            "SOURCE_CHANGED"
                        } else {
                            "SCAN_START_FAILED"
                        };
                        if !self.persist_followup_failure_during_recovery(&scan_id, code) {
                            return false;
                        }
                        if is_source_changed(&error) {
                            self.availability.store(SOURCE_CHANGED, Ordering::Release);
                            return true;
                        }
                        continue;
                    }
                }
            }

            let scan_id = next_scan_id();
            let event = ScanStartEvent::new(scan_id.clone(), ScanTrigger::Startup, now_ms())
                .expect("generated startup event is valid");
            match self.store.mark_started(event) {
                Ok(_) => {
                    self.retry_attempt = 0;
                    self.spawn_worker(scan_id);
                    return true;
                }
                Err(error) if is_busy(&error) => {
                    thread::sleep(self.next_retry_delay());
                }
                Err(error) if is_source_changed(&error) => {
                    self.availability.store(SOURCE_CHANGED, Ordering::Release);
                    return true;
                }
                Err(_) => {
                    // Recovery is complete even if an unacknowledged Startup could not
                    // commit; a later timer or request may start a fresh scan.
                    return true;
                }
            }
        }
    }

    fn handle_command(&mut self, command: Command) -> bool {
        match command {
            Command::Request { trigger, reply } => {
                self.process_request(trigger, Some(reply));
                false
            }
            Command::WorkerFinished { scan_id, result } => {
                self.handle_terminal(scan_id, result);
                self.shutdown_reply.is_none()
                    && self.availability.load(Ordering::Acquire) == STOPPED
            }
            Command::Shutdown { reply } => self.begin_shutdown(reply),
        }
    }

    fn process_request(
        &mut self,
        trigger: ScanTrigger,
        reply: Option<SyncSender<Result<RequestDisposition, ScanRequestError>>>,
    ) {
        let result = self.request_inner(trigger);
        if let Some(reply) = reply {
            let _ = reply.send(result);
        }
    }

    fn request_inner(
        &mut self,
        trigger: ScanTrigger,
    ) -> Result<RequestDisposition, ScanRequestError> {
        match self.availability.load(Ordering::Acquire) {
            READY => {}
            RECOVERING => return Err(ScanRequestError::Recovering),
            SOURCE_CHANGED => return Err(ScanRequestError::SourceChanged),
            SHUTTING_DOWN | STOPPED => return Err(ScanRequestError::ShuttingDown),
            _ => return Err(ScanRequestError::Recovering),
        }
        let state = self.store.scan_state().map_err(map_start_error)?;
        if state.followup_state == Some(FollowupState::Queued) {
            return disposition_from_followup(&state).ok_or(
                ScanRequestError::EnqueueCommitFailed {
                    kind: CommitFailureKind::Internal,
                },
            );
        }
        let requested_at_ms = now_ms();
        if state.scan_state == ScanLifecycleState::Running {
            let event = ReserveScanFollowupEvent::new(next_scan_id(), trigger, requested_at_ms)
                .expect("generated follow-up event is valid");
            let state = self
                .store
                .reserve_followup(event)
                .map_err(|error| map_commit_error(error, false))?;
            return disposition_from_followup(&state).ok_or(
                ScanRequestError::EnqueueCommitFailed {
                    kind: CommitFailureKind::Internal,
                },
            );
        }

        let scan_id = next_scan_id();
        let event = ScanStartEvent::new(scan_id.clone(), trigger, requested_at_ms)
            .expect("generated scan event is valid");
        let state = self
            .store
            .mark_started(event)
            .map_err(|error| map_commit_error(error, true))?;
        let revision = state.status_revision;
        self.spawn_worker(scan_id.clone());
        Ok(RequestDisposition::Started {
            scan_id,
            started_status_revision: revision,
        })
    }

    fn spawn_worker(&mut self, scan_id: String) {
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancellation);
        let worker = Arc::clone(&self.worker);
        let commands = self.commands.clone();
        let worker_scan_id = scan_id.clone();
        thread::Builder::new()
            .name("usagi-scan-worker".to_owned())
            .spawn(move || {
                let result = worker.run(&worker_scan_id, &worker_cancel);
                let _ = commands.send(Command::WorkerFinished {
                    scan_id: worker_scan_id,
                    result,
                });
            })
            .expect("scan worker thread creation failed");
        self.active = Some(ActiveWorker {
            scan_id,
            cancellation,
        });
    }

    fn handle_terminal(&mut self, scan_id: String, result: WorkerResult) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        if active.scan_id != scan_id {
            return;
        }
        let shutting_down = self.availability.load(Ordering::Acquire) == SHUTTING_DOWN;
        let persisted = if shutting_down {
            self.store.mark_failed(
                ScanFailedEvent::new(&scan_id, now_ms(), "SCAN_CANCELLED")
                    .expect("fixed cancellation event is valid"),
            )
        } else {
            match result {
                WorkerResult::Completed => self.store.mark_completed(
                    ScanCompletedEvent::new(&scan_id, now_ms())
                        .expect("generated completion event is valid"),
                ),
                WorkerResult::Failed(error_code) => self.store.mark_failed(
                    ScanFailedEvent::new(&scan_id, now_ms(), error_code)
                        .expect("worker error code must be structured and safe"),
                ),
            }
        };
        match persisted {
            Ok(_) => {
                self.active = None;
                self.retry_attempt = 0;
            }
            Err(error) if is_busy(&error) => {
                self.pending_terminal = Some((scan_id, result));
                self.retry_at = Some(Instant::now() + self.next_retry_delay());
                return;
            }
            Err(error) => {
                if shutting_down && let Some(reply) = self.shutdown_reply.take() {
                    let _ = reply.send(Err(ScanShutdownError::Persistence(commit_kind(&error))));
                }
                self.availability.store(STOPPED, Ordering::Release);
                return;
            }
        }

        if shutting_down {
            if let Some(reply) = self.shutdown_reply.take() {
                let _ = reply.send(Ok(()));
                self.availability.store(STOPPED, Ordering::Release);
            }
        } else {
            self.try_start_queued();
        }
    }

    fn try_start_queued(&mut self) {
        if self.active.is_some() || self.availability.load(Ordering::Acquire) != READY {
            return;
        }
        let state = match self.store.scan_state() {
            Ok(state) => state,
            Err(error) if is_busy(&error) => {
                self.retry_at = Some(Instant::now() + self.next_retry_delay());
                return;
            }
            Err(error) if is_source_changed(&error) => {
                self.availability.store(SOURCE_CHANGED, Ordering::Release);
                return;
            }
            Err(_) => return,
        };
        if state.followup_state != Some(FollowupState::Queued) {
            self.retry_at = None;
            return;
        }
        let scan_id = state
            .followup_scan_id
            .expect("validated queued state has an id");
        let event = FollowupStartedEvent::new(scan_id.clone(), now_ms())
            .expect("generated follow-up event is valid");
        match self.store.mark_followup_started(event) {
            Ok(_) => {
                self.retry_at = None;
                self.retry_attempt = 0;
                self.spawn_worker(scan_id);
            }
            Err(error) if is_busy(&error) => {
                self.retry_at = Some(Instant::now() + self.next_retry_delay());
            }
            Err(error) => {
                let code = if is_source_changed(&error) {
                    "SOURCE_CHANGED"
                } else {
                    "SCAN_START_FAILED"
                };
                if self.persist_followup_failure(&scan_id, code) && is_source_changed(&error) {
                    self.availability.store(SOURCE_CHANGED, Ordering::Release);
                }
            }
        }
    }

    fn begin_shutdown(&mut self, reply: SyncSender<Result<(), ScanShutdownError>>) -> bool {
        self.availability.store(SHUTTING_DOWN, Ordering::Release);
        let state = match self.store.scan_state() {
            Ok(state) => state,
            Err(error) => {
                let _ = reply.send(Err(ScanShutdownError::Persistence(commit_kind(&error))));
                return true;
            }
        };
        if state.followup_state == Some(FollowupState::Queued) {
            let scan_id = state
                .followup_scan_id
                .expect("validated queued state has an id");
            if !self.persist_followup_failure_during_recovery(&scan_id, "SCANNER_UNAVAILABLE") {
                let _ = reply.send(Err(ScanShutdownError::Persistence(
                    CommitFailureKind::Internal,
                )));
                return true;
            }
        }
        if self.active.is_some() {
            self.shutdown_reply = Some(reply);
            self.cancel_active();
            false
        } else {
            let _ = reply.send(Ok(()));
            true
        }
    }

    fn cancel_active(&self) {
        if let Some(active) = &self.active {
            active.cancellation.store(true, Ordering::Release);
        }
    }

    fn persist_followup_failure(&mut self, scan_id: &str, error_code: &'static str) -> bool {
        let event = FollowupStartFailedEvent::new(scan_id, now_ms(), error_code)
            .expect("fixed follow-up failure event is valid");
        match self.store.mark_followup_start_failed(event) {
            Ok(_) => {
                if error_code == "SOURCE_CHANGED" {
                    self.availability.store(SOURCE_CHANGED, Ordering::Release);
                }
                true
            }
            Err(error) if is_busy(&error) => {
                self.pending_followup_failure = Some((scan_id.to_owned(), error_code));
                self.retry_at = Some(Instant::now() + self.next_retry_delay());
                false
            }
            Err(_) => false,
        }
    }

    fn persist_followup_failure_during_recovery(
        &mut self,
        scan_id: &str,
        error_code: &'static str,
    ) -> bool {
        loop {
            let event = FollowupStartFailedEvent::new(scan_id, now_ms(), error_code)
                .expect("fixed follow-up failure event is valid");
            match self.store.mark_followup_start_failed(event) {
                Ok(_) => {
                    if error_code == "SOURCE_CHANGED" {
                        self.availability.store(SOURCE_CHANGED, Ordering::Release);
                    }
                    self.retry_attempt = 0;
                    return true;
                }
                Err(error) if is_busy(&error) => {
                    thread::sleep(self.next_retry_delay());
                }
                Err(_) => return false,
            }
        }
    }

    fn next_retry_delay(&mut self) -> Duration {
        let shift = self.retry_attempt.min(5);
        self.retry_attempt = self.retry_attempt.saturating_add(1);
        RETRY_BASE
            .checked_mul(1_u32 << shift)
            .unwrap_or(RETRY_MAX)
            .min(RETRY_MAX)
    }
}

fn disposition_from_followup(state: &ScanState) -> Option<RequestDisposition> {
    Some(RequestDisposition::Coalesced {
        followup_scan_id: state.followup_scan_id.clone()?,
        enqueued_status_revision: state.followup_enqueued_status_revision?,
    })
}

fn map_start_error(error: StorageError) -> ScanRequestError {
    if is_source_changed(&error) {
        ScanRequestError::SourceChanged
    } else {
        ScanRequestError::StartCommitFailed {
            kind: commit_kind(&error),
        }
    }
}

fn map_commit_error(error: StorageError, starting: bool) -> ScanRequestError {
    if is_source_changed(&error) {
        return ScanRequestError::SourceChanged;
    }
    if starting {
        ScanRequestError::StartCommitFailed {
            kind: commit_kind(&error),
        }
    } else {
        ScanRequestError::EnqueueCommitFailed {
            kind: commit_kind(&error),
        }
    }
}

fn commit_kind(error: &StorageError) -> CommitFailureKind {
    if is_busy(error) {
        CommitFailureKind::Busy
    } else {
        CommitFailureKind::Internal
    }
}

fn is_busy(error: &StorageError) -> bool {
    error.kind() == StorageErrorKind::DatabaseBusy
}

fn is_source_changed(error: &StorageError) -> bool {
    matches!(
        error.kind(),
        StorageErrorKind::SourceChanged | StorageErrorKind::SourceUnbound
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn next_scan_id() -> String {
    let sequence = NEXT_SCAN_ID.fetch_add(1, Ordering::Relaxed);
    let mut bytes = [0_u8; 16];
    if crate::random::fill_os_random(&mut bytes).is_err() {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&now_ms().to_be_bytes());
        hasher.update(&sequence.to_be_bytes());
        hasher.update(&std::process::id().to_be_bytes());
        bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    }
    // RFC 4122 variant + version 4 representation. The fallback is still an
    // opaque, never-reused process/time/counter-derived UUID.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        fs::OpenOptions,
        io::Write,
        path::{Path, PathBuf},
        sync::{Mutex, mpsc},
        thread,
    };

    use super::*;
    use crate::{
        domain::{ReserveScanFollowupEvent, ScanRunState},
        storage::LedgerOptions,
    };

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "usagi-coordinator-{label}-{}-{}",
                now_ms(),
                NEXT_SCAN_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create temporary directory");
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

    struct ControlledWorker {
        started: mpsc::Sender<String>,
        releases: Mutex<mpsc::Receiver<()>>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum StoreOperation {
        ScanState,
        Start,
        Reserve,
        FollowupStart,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum InjectedFailure {
        Busy,
        Internal,
        SourceChanged,
        BlockAfter,
    }

    struct BlockGate {
        entered: mpsc::SyncSender<()>,
        release: mpsc::Receiver<()>,
        returned: mpsc::SyncSender<()>,
    }

    struct BlockControl {
        entered: mpsc::Receiver<()>,
        release: mpsc::SyncSender<()>,
        returned: mpsc::Receiver<()>,
    }

    struct FaultStore {
        ledger: Arc<Ledger>,
        failures: Mutex<VecDeque<(StoreOperation, InjectedFailure)>>,
        operations: Mutex<Vec<StoreOperation>>,
        block_gate: Mutex<Option<BlockGate>>,
    }

    impl FaultStore {
        fn new(ledger: Arc<Ledger>) -> Self {
            Self {
                ledger,
                failures: Mutex::new(VecDeque::new()),
                operations: Mutex::new(Vec::new()),
                block_gate: Mutex::new(None),
            }
        }

        fn install_block_gate(&self) -> BlockControl {
            let (entered_tx, entered_rx) = mpsc::sync_channel(0);
            let (release_tx, release_rx) = mpsc::sync_channel(0);
            let (returned_tx, returned_rx) = mpsc::sync_channel(0);
            *self.block_gate.lock().unwrap() = Some(BlockGate {
                entered: entered_tx,
                release: release_rx,
                returned: returned_tx,
            });
            BlockControl {
                entered: entered_rx,
                release: release_tx,
                returned: returned_rx,
            }
        }

        fn inject(&self, operation: StoreOperation, failure: InjectedFailure) {
            self.failures
                .lock()
                .unwrap()
                .push_back((operation, failure));
        }

        fn operation_count(&self, operation: StoreOperation) -> usize {
            self.operations
                .lock()
                .unwrap()
                .iter()
                .filter(|candidate| **candidate == operation)
                .count()
        }

        fn before(&self, operation: StoreOperation) -> Result<bool, StorageError> {
            self.operations.lock().unwrap().push(operation);
            let mut failures = self.failures.lock().unwrap();
            let Some(index) = failures
                .iter()
                .position(|(candidate, _)| *candidate == operation)
            else {
                return Ok(false);
            };
            let (_, failure) = failures.remove(index).unwrap();
            drop(failures);
            if failure == InjectedFailure::BlockAfter {
                Ok(true)
            } else {
                Err(injected_storage_error(failure))
            }
        }

        fn wait_on_block_gate(&self) {
            let gate = self
                .block_gate
                .lock()
                .unwrap()
                .take()
                .expect("block gate should be installed before injection");
            gate.entered.send(()).unwrap();
            gate.release.recv().unwrap();
            gate.returned.send(()).unwrap();
        }
    }

    impl LifecycleStore for FaultStore {
        fn scan_state(&self) -> Result<ScanState, StorageError> {
            self.before(StoreOperation::ScanState)?;
            Ok(self.ledger.scan_status_snapshot(None)?.app_state.scan)
        }

        fn mark_started(&self, event: ScanStartEvent) -> Result<ScanState, StorageError> {
            self.before(StoreOperation::Start)?;
            self.ledger.mark_scan_started(event)
        }

        fn reserve_followup(
            &self,
            event: ReserveScanFollowupEvent,
        ) -> Result<ScanState, StorageError> {
            let block_after = self.before(StoreOperation::Reserve)?;
            let state = self.ledger.reserve_scan_followup(event)?;
            if block_after {
                self.wait_on_block_gate();
            }
            Ok(state)
        }

        fn mark_followup_started(
            &self,
            event: FollowupStartedEvent,
        ) -> Result<ScanState, StorageError> {
            self.before(StoreOperation::FollowupStart)?;
            self.ledger.mark_followup_started(event)
        }

        fn mark_followup_start_failed(
            &self,
            event: FollowupStartFailedEvent,
        ) -> Result<ScanState, StorageError> {
            self.ledger.mark_followup_start_failed(event)
        }

        fn mark_completed(&self, event: ScanCompletedEvent) -> Result<ScanState, StorageError> {
            self.ledger.mark_scan_completed(event)
        }

        fn mark_failed(&self, event: ScanFailedEvent) -> Result<ScanState, StorageError> {
            self.ledger.mark_scan_failed(event)
        }
    }

    fn injected_storage_error(failure: InjectedFailure) -> StorageError {
        match failure {
            InjectedFailure::Busy => StorageError::sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            )),
            InjectedFailure::Internal => StorageError::invalid_state("injected lifecycle failure"),
            InjectedFailure::SourceChanged => StorageError::source_changed("old", "new"),
            InjectedFailure::BlockAfter => {
                unreachable!("test-only timing controls are not storage errors")
            }
        }
    }

    fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + timeout;
        while !predicate() {
            assert!(Instant::now() < deadline, "condition timed out");
            thread::sleep(Duration::from_millis(2));
        }
    }

    impl ScanWorker for ControlledWorker {
        fn run(&self, scan_id: &str, cancellation: &AtomicBool) -> WorkerResult {
            self.started.send(scan_id.to_owned()).unwrap();
            loop {
                if cancellation.load(Ordering::Acquire) {
                    return WorkerResult::Completed;
                }
                match self
                    .releases
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_millis(5))
                {
                    Ok(()) => return WorkerResult::Completed,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return WorkerResult::Failed("STORAGE_COMMIT_FAILED");
                    }
                }
            }
        }
    }

    fn setup(label: &str) -> (TempDir, Arc<Ledger>, ScanConfig) {
        let temp = TempDir::new(label);
        let codex_home = temp.path().join("codex");
        fs::create_dir_all(&codex_home).unwrap();
        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(
                temp.path().join("mu.sqlite3"),
                &codex_home,
            ))
            .unwrap(),
        );
        (temp, ledger, ScanConfig::new(codex_home))
    }

    fn worker() -> (
        Arc<ControlledWorker>,
        mpsc::Receiver<String>,
        mpsc::Sender<()>,
    ) {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        (
            Arc::new(ControlledWorker {
                started: started_tx,
                releases: Mutex::new(release_rx),
            }),
            started_rx,
            release_tx,
        )
    }

    #[test]
    fn startup_is_immediate_timer_is_not_and_interval_is_validated() {
        let (_temp, ledger, config) = setup("startup");
        assert_eq!(config.interval, Duration::from_secs(300));
        assert_eq!(
            config
                .clone()
                .with_interval(Duration::from_secs(59))
                .validate(),
            Err(ScanConfigError::IntervalOutOfRange)
        );
        assert_eq!(
            config
                .clone()
                .with_interval(Duration::from_secs(3_601))
                .validate(),
            Err(ScanConfigError::IntervalOutOfRange)
        );
        let (worker, started, _release) = worker();
        let handle = ScanCoordinator::start(config, ledger, worker).unwrap();
        let first = started.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(Uuid::parse_str(&first).is_ok());
        assert!(started.recv_timeout(Duration::from_millis(40)).is_err());
        handle.shutdown().unwrap();
    }

    #[test]
    fn concurrent_requests_share_one_durable_followup_and_ack() {
        let (_temp, ledger, config) = setup("coalesce");
        let (worker_impl, started, release) = worker();
        let handle = ScanCoordinator::start(config, Arc::clone(&ledger), worker_impl).unwrap();
        let startup_id = started.recv_timeout(Duration::from_secs(2)).unwrap();

        let first = handle.request(ScanTrigger::Manual).unwrap();
        let second = handle.request(ScanTrigger::Scheduled).unwrap();
        let (
            RequestDisposition::Coalesced {
                followup_scan_id: first_id,
                enqueued_status_revision: first_revision,
            },
            RequestDisposition::Coalesced {
                followup_scan_id: second_id,
                enqueued_status_revision: second_revision,
            },
        ) = (first, second)
        else {
            panic!("both requests must coalesce");
        };
        assert_eq!(first_id, second_id);
        assert_eq!(first_revision, second_revision);
        let persisted = ledger.scan_status_snapshot(Some(&first_id)).unwrap();
        assert_eq!(persisted.target_scan.unwrap().state, ScanRunState::Queued);

        release.send(()).unwrap();
        assert_eq!(
            started.recv_timeout(Duration::from_secs(2)).unwrap(),
            first_id
        );
        assert_ne!(startup_id, first_id);
        handle.shutdown().unwrap();
    }

    #[test]
    fn recovery_prefers_queued_and_shutdown_persists_both_terminal_states() {
        let (_temp, ledger, config) = setup("recovery");
        ledger
            .mark_scan_started(ScanStartEvent::new("old-active", ScanTrigger::Startup, 1).unwrap())
            .unwrap();
        ledger
            .reserve_scan_followup(
                ReserveScanFollowupEvent::new("recovered-followup", ScanTrigger::Manual, 2)
                    .unwrap(),
            )
            .unwrap();

        let (worker, started, _release) = worker();
        let handle = ScanCoordinator::start(config, Arc::clone(&ledger), worker).unwrap();
        assert_eq!(
            started.recv_timeout(Duration::from_secs(2)).unwrap(),
            "recovered-followup"
        );
        let queued = handle.request(ScanTrigger::Manual).unwrap();
        let RequestDisposition::Coalesced {
            followup_scan_id, ..
        } = queued
        else {
            panic!("active recovered worker must coalesce");
        };

        handle.shutdown().unwrap();
        let interrupted = ledger
            .scan_status_snapshot(Some("old-active"))
            .unwrap()
            .target_scan
            .unwrap();
        assert_eq!(interrupted.state, ScanRunState::Failed);
        assert_eq!(interrupted.error_code.as_deref(), Some("SCAN_INTERRUPTED"));
        let cancelled = ledger
            .scan_status_snapshot(Some("recovered-followup"))
            .unwrap()
            .target_scan
            .unwrap();
        assert_eq!(cancelled.state, ScanRunState::Failed);
        assert_eq!(cancelled.error_code.as_deref(), Some("SCAN_CANCELLED"));
        let unavailable = ledger
            .scan_status_snapshot(Some(&followup_scan_id))
            .unwrap()
            .target_scan
            .unwrap();
        assert_eq!(unavailable.state, ScanRunState::StartFailed);
        assert_eq!(
            unavailable.error_code.as_deref(),
            Some("SCANNER_UNAVAILABLE")
        );
    }

    #[test]
    fn real_timer_skips_missed_ticks_growth_does_not_self_enqueue_and_empty_completion_is_status_only()
     {
        let (temp, ledger, config) = setup("timer-status");
        let (worker_impl, started, release) = worker();
        let handle = ScanCoordinator::start_with_store(
            config.with_interval(Duration::from_millis(200)),
            Arc::clone(&ledger) as Arc<dyn LifecycleStore>,
            worker_impl,
        )
        .unwrap();
        let startup_id = started.recv_timeout(Duration::from_secs(2)).unwrap();
        let started_revision = ledger.app_state().unwrap().status_revision;

        let growing_file = temp.path().join("rollout-growing.jsonl");
        fs::write(&growing_file, b"first\n").unwrap();
        OpenOptions::new()
            .append(true)
            .open(&growing_file)
            .unwrap()
            .write_all(b"second\n")
            .unwrap();
        // Actual file growth emits no coordinator command. Completing the
        // round therefore cannot create an immediate follow-up.
        release.send(()).unwrap();
        wait_until(Duration::from_secs(2), || {
            ledger.app_state().unwrap().scan_state == ScanLifecycleState::Idle
        });
        let completed = ledger.app_state().unwrap();
        assert_eq!(completed.data_revision, 1);
        assert_eq!(completed.status_revision, started_revision + 1);
        assert_eq!(
            ledger
                .scan_status_snapshot(Some(&startup_id))
                .unwrap()
                .target_scan
                .unwrap()
                .state,
            ScanRunState::Completed
        );
        assert!(started.recv_timeout(Duration::from_millis(40)).is_err());
        handle.shutdown().unwrap();

        let (_temp, ledger, config) = setup("missed-tick");
        let store = Arc::new(FaultStore::new(Arc::clone(&ledger)));
        let (worker_impl, started, release) = worker();
        let handle = ScanCoordinator::start_with_store(
            config.with_interval(Duration::from_millis(20)),
            Arc::clone(&store) as Arc<dyn LifecycleStore>,
            worker_impl,
        )
        .unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let baseline_reads = store.operation_count(StoreOperation::ScanState);
        let block = store.install_block_gate();
        store.inject(StoreOperation::Reserve, InjectedFailure::BlockAfter);
        block
            .entered
            .recv_timeout(Duration::from_secs(2))
            .expect("scheduled request did not reach the deterministic gate");
        let queued = ledger.app_state().unwrap();
        assert_eq!(queued.followup_state, Some(FollowupState::Queued));
        let queued_id = queued.followup_scan_id.clone().unwrap();
        let reads_after_delayed_tick = store.operation_count(StoreOperation::ScanState);
        assert_eq!(reads_after_delayed_tick, baseline_reads + 1);
        block.release.send(()).unwrap();
        block
            .returned
            .recv_timeout(Duration::from_secs(2))
            .expect("scheduled request did not leave the deterministic gate");
        assert_eq!(
            store.operation_count(StoreOperation::ScanState),
            reads_after_delayed_tick,
            "missed intervals must not replay immediately"
        );

        release.send(()).unwrap();
        assert_eq!(
            started.recv_timeout(Duration::from_secs(2)).unwrap(),
            queued_id
        );
        release.send(()).unwrap();
        handle.shutdown().unwrap();
    }

    #[test]
    fn request_commit_error_matrix_and_failed_state_manual_start_are_linearized() {
        let (_temp, ledger, config) = setup("request-errors");
        let store = Arc::new(FaultStore::new(Arc::clone(&ledger)));
        let (worker_impl, started, release) = worker();
        let handle = ScanCoordinator::start_with_store(
            config,
            Arc::clone(&store) as Arc<dyn LifecycleStore>,
            worker_impl,
        )
        .unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        release.send(()).unwrap();
        wait_until(Duration::from_secs(2), || {
            ledger.app_state().unwrap().scan_state == ScanLifecycleState::Idle
        });
        ledger
            .mark_scan_started(
                ScanStartEvent::new("previous-failed", ScanTrigger::Manual, now_ms()).unwrap(),
            )
            .unwrap();
        ledger
            .mark_scan_failed(
                ScanFailedEvent::new("previous-failed", now_ms(), "STORAGE_COMMIT_FAILED").unwrap(),
            )
            .unwrap();
        assert_eq!(
            ledger.app_state().unwrap().scan_state,
            ScanLifecycleState::Failed
        );

        for (failure, expected) in [
            (
                InjectedFailure::Busy,
                ScanRequestError::StartCommitFailed {
                    kind: CommitFailureKind::Busy,
                },
            ),
            (
                InjectedFailure::Internal,
                ScanRequestError::StartCommitFailed {
                    kind: CommitFailureKind::Internal,
                },
            ),
            (
                InjectedFailure::SourceChanged,
                ScanRequestError::SourceChanged,
            ),
        ] {
            store.inject(StoreOperation::Start, failure);
            assert_eq!(handle.request(ScanTrigger::Manual), Err(expected));
        }

        let RequestDisposition::Started {
            scan_id,
            started_status_revision,
        } = handle.request(ScanTrigger::Manual).unwrap()
        else {
            panic!("idle/failed state must start a direct scan");
        };
        assert_eq!(
            started.recv_timeout(Duration::from_secs(2)).unwrap(),
            scan_id
        );
        let target = ledger
            .scan_status_snapshot(Some(&scan_id))
            .unwrap()
            .target_scan
            .unwrap();
        assert_eq!(target.state, ScanRunState::Running);
        assert_eq!(
            target.started_status_revision,
            Some(started_status_revision)
        );

        for (failure, expected_kind) in [
            (InjectedFailure::Busy, CommitFailureKind::Busy),
            (InjectedFailure::Internal, CommitFailureKind::Internal),
        ] {
            store.inject(StoreOperation::Reserve, failure);
            assert_eq!(
                handle.request(ScanTrigger::Manual),
                Err(ScanRequestError::EnqueueCommitFailed {
                    kind: expected_kind
                })
            );
        }
        let first = handle.request(ScanTrigger::Manual).unwrap();
        let second = handle.request(ScanTrigger::Scheduled).unwrap();
        assert_eq!(first, second);
        handle.shutdown().unwrap();
    }

    #[test]
    fn followup_start_busy_and_terminal_failure_matrix_preserve_the_durable_slot() {
        let (_temp, ledger, config) = setup("followup-busy");
        let store = Arc::new(FaultStore::new(Arc::clone(&ledger)));
        let (worker_impl, started, release) = worker();
        let handle = ScanCoordinator::start_with_store(
            config,
            Arc::clone(&store) as Arc<dyn LifecycleStore>,
            worker_impl,
        )
        .unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let RequestDisposition::Coalesced {
            followup_scan_id, ..
        } = handle.request(ScanTrigger::Manual).unwrap()
        else {
            panic!("running request must reserve a follow-up");
        };
        for _ in 0..4 {
            store.inject(StoreOperation::FollowupStart, InjectedFailure::Busy);
        }
        release.send(()).unwrap();
        wait_until(Duration::from_secs(2), || {
            store.operation_count(StoreOperation::FollowupStart) >= 1
        });
        assert_eq!(
            ledger
                .scan_status_snapshot(Some(&followup_scan_id))
                .unwrap()
                .target_scan
                .unwrap()
                .state,
            ScanRunState::Queued
        );
        assert_eq!(
            started.recv_timeout(Duration::from_secs(3)).unwrap(),
            followup_scan_id
        );
        handle.shutdown().unwrap();

        for (label, failure, expected_code) in [
            (
                "followup-internal",
                InjectedFailure::Internal,
                "SCAN_START_FAILED",
            ),
            (
                "followup-source-change",
                InjectedFailure::SourceChanged,
                "SOURCE_CHANGED",
            ),
        ] {
            let (_temp, ledger, config) = setup(label);
            let store = Arc::new(FaultStore::new(Arc::clone(&ledger)));
            let (worker_impl, started, release) = worker();
            let handle = ScanCoordinator::start_with_store(
                config,
                Arc::clone(&store) as Arc<dyn LifecycleStore>,
                worker_impl,
            )
            .unwrap();
            started.recv_timeout(Duration::from_secs(2)).unwrap();
            let RequestDisposition::Coalesced {
                followup_scan_id, ..
            } = handle.request(ScanTrigger::Manual).unwrap()
            else {
                panic!("running request must reserve a follow-up");
            };
            store.inject(StoreOperation::FollowupStart, failure);
            release.send(()).unwrap();
            wait_until(Duration::from_secs(2), || {
                ledger
                    .scan_status_snapshot(Some(&followup_scan_id))
                    .unwrap()
                    .target_scan
                    .is_some_and(|run| run.state == ScanRunState::StartFailed)
            });
            let target = ledger
                .scan_status_snapshot(Some(&followup_scan_id))
                .unwrap()
                .target_scan
                .unwrap();
            assert_eq!(target.error_code.as_deref(), Some(expected_code));
            if failure == InjectedFailure::SourceChanged {
                assert_eq!(
                    handle.request(ScanTrigger::Manual),
                    Err(ScanRequestError::SourceChanged)
                );
            }
            handle.shutdown().unwrap();
        }
    }

    #[test]
    fn startup_recovery_crash_window_matrix_keeps_queued_priority() {
        #[derive(Clone, Copy)]
        enum CrashWindow {
            ActiveOnly,
            ActiveAndQueued,
            IdleAndQueued,
            FailedAndQueued,
            FollowupStartedBeforeIo,
            StartFailureBeforeCommit,
            BusyRetryRestart,
        }

        for (index, window) in [
            CrashWindow::ActiveOnly,
            CrashWindow::ActiveAndQueued,
            CrashWindow::IdleAndQueued,
            CrashWindow::FailedAndQueued,
            CrashWindow::FollowupStartedBeforeIo,
            CrashWindow::StartFailureBeforeCommit,
            CrashWindow::BusyRetryRestart,
        ]
        .into_iter()
        .enumerate()
        {
            let label = format!("crash-{index}");
            let (_temp, ledger, config) = setup(&label);
            ledger
                .mark_scan_started(
                    ScanStartEvent::new("old-active", ScanTrigger::Startup, 1).unwrap(),
                )
                .unwrap();
            let has_queued = !matches!(window, CrashWindow::ActiveOnly);
            if has_queued {
                ledger
                    .reserve_scan_followup(
                        ReserveScanFollowupEvent::new("durable-followup", ScanTrigger::Manual, 2)
                            .unwrap(),
                    )
                    .unwrap();
            }
            match window {
                CrashWindow::IdleAndQueued
                | CrashWindow::StartFailureBeforeCommit
                | CrashWindow::BusyRetryRestart => {
                    ledger
                        .mark_scan_completed(ScanCompletedEvent::new("old-active", 3).unwrap())
                        .unwrap();
                }
                CrashWindow::FailedAndQueued => {
                    ledger
                        .mark_scan_failed(
                            ScanFailedEvent::new("old-active", 3, "STORAGE_COMMIT_FAILED").unwrap(),
                        )
                        .unwrap();
                }
                CrashWindow::FollowupStartedBeforeIo => {
                    ledger
                        .mark_scan_completed(ScanCompletedEvent::new("old-active", 3).unwrap())
                        .unwrap();
                    ledger
                        .mark_followup_started(
                            FollowupStartedEvent::new("durable-followup", 4).unwrap(),
                        )
                        .unwrap();
                }
                CrashWindow::ActiveOnly | CrashWindow::ActiveAndQueued => {}
            }

            let store = Arc::new(FaultStore::new(Arc::clone(&ledger)));
            if matches!(window, CrashWindow::BusyRetryRestart) {
                store.inject(StoreOperation::ScanState, InjectedFailure::Busy);
                store.inject(StoreOperation::ScanState, InjectedFailure::Busy);
            }
            let (worker, started, _release) = worker();
            let handle = ScanCoordinator::start_with_store(
                config,
                Arc::clone(&store) as Arc<dyn LifecycleStore>,
                worker,
            )
            .unwrap();
            if matches!(window, CrashWindow::BusyRetryRestart) {
                assert_eq!(
                    handle.request(ScanTrigger::Manual),
                    Err(ScanRequestError::Recovering)
                );
            }
            let first_worker = started.recv_timeout(Duration::from_secs(3)).unwrap();
            match window {
                CrashWindow::ActiveAndQueued
                | CrashWindow::IdleAndQueued
                | CrashWindow::FailedAndQueued
                | CrashWindow::StartFailureBeforeCommit
                | CrashWindow::BusyRetryRestart => {
                    assert_eq!(first_worker, "durable-followup");
                }
                CrashWindow::ActiveOnly | CrashWindow::FollowupStartedBeforeIo => {
                    assert_ne!(first_worker, "durable-followup");
                }
            }
            if matches!(
                window,
                CrashWindow::ActiveOnly | CrashWindow::ActiveAndQueued
            ) {
                let old = ledger
                    .scan_status_snapshot(Some("old-active"))
                    .unwrap()
                    .target_scan
                    .unwrap();
                assert_eq!(old.state, ScanRunState::Failed);
                assert_eq!(old.error_code.as_deref(), Some("SCAN_INTERRUPTED"));
            }
            if matches!(window, CrashWindow::FollowupStartedBeforeIo) {
                let interrupted = ledger
                    .scan_status_snapshot(Some("durable-followup"))
                    .unwrap()
                    .target_scan
                    .unwrap();
                assert_eq!(interrupted.state, ScanRunState::Failed);
                assert_eq!(interrupted.error_code.as_deref(), Some("SCAN_INTERRUPTED"));
            }
            handle.shutdown().unwrap();
        }
    }
}
