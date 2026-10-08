package ingestion

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"sync"
	"sync/atomic"
	"time"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	"github.com/google/uuid"
)

const (
	commandCapacity = 32
	retryBase       = 25 * time.Millisecond
	retryMax        = time.Second
)

var scanIDSequence atomic.Uint64

type CoordinatorAvailability uint8

const (
	CoordinatorRecovering CoordinatorAvailability = iota
	CoordinatorReady
	CoordinatorShuttingDown
	CoordinatorStopped
)

type CommitFailureKind uint8

const (
	CommitFailureBusy CommitFailureKind = iota
	CommitFailureInternal
)

type RequestDispositionKind uint8

const (
	RequestStarted RequestDispositionKind = iota
	RequestCoalesced
)

type RequestDisposition struct {
	Kind           RequestDispositionKind
	ScanID         string
	StatusRevision int64
}

type RequestErrorKind uint8

const (
	RequestErrorRecovering RequestErrorKind = iota
	RequestErrorShuttingDown
	RequestErrorStartCommitFailed
	RequestErrorEnqueueCommitFailed
)

type RequestError struct {
	Kind          RequestErrorKind
	CommitFailure *CommitFailureKind
}

func (e RequestError) Error() string {
	switch e.Kind {
	case RequestErrorRecovering:
		return "scan coordinator is recovering"
	case RequestErrorShuttingDown:
		return "scan coordinator is shutting down"
	case RequestErrorStartCommitFailed:
		return fmt.Sprintf("scan start commit failed: %s", commitFailureName(e.CommitFailure))
	case RequestErrorEnqueueCommitFailed:
		return fmt.Sprintf("scan follow-up commit failed: %s", commitFailureName(e.CommitFailure))
	default:
		return "scan request failed"
	}
}

func newRequestError(kind RequestErrorKind, failure CommitFailureKind) RequestError {
	switch kind {
	case RequestErrorStartCommitFailed, RequestErrorEnqueueCommitFailed:
		if failure != CommitFailureBusy && failure != CommitFailureInternal {
			failure = CommitFailureInternal
		}
		return RequestError{Kind: kind, CommitFailure: &failure}
	default:
		return RequestError{Kind: kind}
	}
}

func commitFailureName(failure *CommitFailureKind) string {
	if failure == nil {
		return "internal"
	}
	if *failure == CommitFailureBusy {
		return "busy"
	}
	return "internal"
}

var ErrCoordinatorUnavailable = errors.New("scan coordinator is unavailable")

type lifecycleStore interface {
	ScanStatusSnapshot(ctx context.Context, targetScanID *string) (domain.ScanStatusSnapshot, error)
	MarkScanStartedWithSources(ctx context.Context, event domain.ScanStartEvent, sources []domain.SourceID) (domain.ScanState, error)
	ReserveScanFollowup(ctx context.Context, event domain.ReserveScanFollowupEvent) (domain.ScanState, error)
	MarkFollowupStartedWithSources(ctx context.Context, event domain.FollowupStartedEvent, sources []domain.SourceID) (domain.ScanState, error)
	MarkFollowupStartFailed(ctx context.Context, event domain.FollowupStartFailedEvent) (domain.ScanState, error)
	MarkScanCompleted(ctx context.Context, event domain.ScanCompletedEvent) (domain.ScanState, error)
	MarkScanFailed(ctx context.Context, event domain.ScanFailedEvent) (domain.ScanState, error)
	MarkSourceScanStarted(ctx context.Context, scanID string, source domain.SourceID, startedAtMS int64) error
	MarkSourceScanCompleted(ctx context.Context, scanID string, source domain.SourceID, finishedAtMS int64) error
	MarkSourceScanSkipped(ctx context.Context, scanID string, source domain.SourceID, finishedAtMS int64) error
	MarkSourceScanFailed(ctx context.Context, scanID string, source domain.SourceID, failedAtMS int64, errorCode string) error
}

type Handle struct {
	commands     chan command
	loopDone     chan struct{}
	availability atomic.Uint32
	reportsMu    sync.RWMutex
	reports      []source.RunReport
}

func Start(cfg Config, db *storage.DB, registry *source.Registry) (*Handle, error) {
	if err := cfg.Validate(); err != nil {
		return nil, err
	}
	if db == nil || registry == nil {
		return nil, ErrCoordinatorUnavailable
	}
	return startWithStore(cfg, db, registry, db)
}

func startWithStore(cfg Config, db *storage.DB, registry *source.Registry, store lifecycleStore) (*Handle, error) {
	return startCoordinator(cfg.Interval, db, registry, store)
}

func startWithIntervalForTest(interval time.Duration, db *storage.DB, registry *source.Registry, store lifecycleStore) (*Handle, error) {
	if interval <= 0 {
		return nil, fmt.Errorf("test scan interval must be positive")
	}
	return startCoordinator(interval, db, registry, store)
}

func startCoordinator(interval time.Duration, db *storage.DB, registry *source.Registry, store lifecycleStore) (*Handle, error) {
	if db == nil || registry == nil || store == nil {
		return nil, ErrCoordinatorUnavailable
	}
	h := &Handle{
		commands: make(chan command, commandCapacity),
		loopDone: make(chan struct{}),
	}
	h.availability.Store(uint32(CoordinatorRecovering))
	loop := coordinatorLoop{
		interval: interval,
		db:       db,
		registry: registry,
		store:    store,
		handle:   h,
	}
	go loop.run()
	return h, nil
}

func (h *Handle) Availability() CoordinatorAvailability {
	return CoordinatorAvailability(h.availability.Load())
}

func (h *Handle) Request(ctx context.Context, trigger domain.ScanTrigger) (RequestDisposition, error) {
	switch h.Availability() {
	case CoordinatorRecovering:
		return RequestDisposition{}, newRequestError(RequestErrorRecovering, 0)
	case CoordinatorShuttingDown, CoordinatorStopped:
		return RequestDisposition{}, newRequestError(RequestErrorShuttingDown, 0)
	case CoordinatorReady:
	}
	reply := make(chan requestResult, 1)
	cmd := requestCommand{trigger: trigger, reply: reply}
	select {
	case h.commands <- cmd:
	case <-ctx.Done():
		return RequestDisposition{}, ctx.Err()
	case <-h.loopDone:
		return RequestDisposition{}, newRequestError(RequestErrorShuttingDown, 0)
	}
	select {
	case result := <-reply:
		return result.disposition, result.err
	case <-ctx.Done():
		return RequestDisposition{}, ctx.Err()
	case <-h.loopDone:
		select {
		case result := <-reply:
			return result.disposition, result.err
		default:
			return RequestDisposition{}, newRequestError(RequestErrorShuttingDown, 0)
		}
	}
}

func (h *Handle) Shutdown(ctx context.Context) error {
	for {
		state := h.Availability()
		switch state {
		case CoordinatorStopped:
			return nil
		case CoordinatorShuttingDown:
			return ErrCoordinatorUnavailable
		case CoordinatorRecovering, CoordinatorReady:
			if h.availability.CompareAndSwap(uint32(state), uint32(CoordinatorShuttingDown)) {
				goto owned
			}
		default:
			return ErrCoordinatorUnavailable
		}
	}

owned:
	reply := make(chan error, 1)
	select {
	case h.commands <- shutdownCommand{reply: reply}:
	case <-h.loopDone:
		return ErrCoordinatorUnavailable
	}
	select {
	case err := <-reply:
		return err
	case <-ctx.Done():
		return ctx.Err()
	case <-h.loopDone:
		select {
		case err := <-reply:
			return err
		default:
			return ErrCoordinatorUnavailable
		}
	}
}

func (h *Handle) SourceReports() []source.RunReport {
	h.reportsMu.RLock()
	defer h.reportsMu.RUnlock()
	out := make([]source.RunReport, len(h.reports))
	for i, report := range h.reports {
		out[i] = report
		if report.ErrorCode != nil {
			code := *report.ErrorCode
			out[i].ErrorCode = &code
		}
	}
	return out
}

func (h *Handle) publishReports(reports []source.RunReport) {
	h.reportsMu.Lock()
	h.reports = append(h.reports[:0], reports...)
	for i := range h.reports {
		if reports[i].ErrorCode != nil {
			code := *reports[i].ErrorCode
			h.reports[i].ErrorCode = &code
		}
	}
	h.reportsMu.Unlock()
}

type command interface{ isCoordinatorCommand() }

type requestCommand struct {
	trigger domain.ScanTrigger
	reply   chan requestResult
}

func (requestCommand) isCoordinatorCommand() {}

type requestResult struct {
	disposition RequestDisposition
	err         error
}

type workerFinishedCommand struct {
	scanID string
	result workerResult
}

func (workerFinishedCommand) isCoordinatorCommand() {}

type shutdownCommand struct{ reply chan error }

func (shutdownCommand) isCoordinatorCommand() {}

type workerResult struct {
	failed    bool
	errorCode string
}

type activeWorker struct {
	scanID string
	cancel context.CancelFunc
}

type pendingTerminal struct {
	scanID string
	result workerResult
}

type pendingFollowupFailure struct {
	scanID    string
	errorCode string
}

type coordinatorLoop struct {
	interval time.Duration
	db       *storage.DB
	registry *source.Registry
	store    lifecycleStore
	handle   *Handle

	activeWorker          *activeWorker
	pendingTerminal       *pendingTerminal
	pendingFollowup       *pendingFollowupFailure
	pendingShutdownActive *string
	retryAttempt          uint32
	retryAt               time.Time
	shutdownReply         chan error
	shutdownSnapshotKnown bool
}

func (loop *coordinatorLoop) run() {
	defer close(loop.handle.loopDone)
	nextTick := time.Now().Add(loop.interval)
	loop.recover()
	if !loop.handle.availability.CompareAndSwap(uint32(CoordinatorRecovering), uint32(CoordinatorReady)) {
		if loop.handle.Availability() == CoordinatorShuttingDown && loop.driveShutdown() {
			return
		}
	}

	for {
		if !loop.retryAt.IsZero() && !loop.retryAt.After(time.Now()) {
			loop.retryAt = time.Time{}
			loop.retryPending()
			if loop.handle.Availability() == CoordinatorShuttingDown {
				if loop.driveShutdown() {
					return
				}
			} else {
				loop.tryStartQueued()
			}
		}
		if loop.handle.Availability() == CoordinatorShuttingDown && loop.driveShutdown() {
			return
		}
		nextEvent := nextTick
		if !loop.retryAt.IsZero() && loop.retryAt.Before(nextEvent) {
			nextEvent = loop.retryAt
		}
		delay := time.Until(nextEvent)
		if delay < 0 {
			delay = 0
		}
		timer := time.NewTimer(delay)
		select {
		case cmd := <-loop.handle.commands:
			timer.Stop()
			if loop.handleCommand(cmd) {
				return
			}
		case <-timer.C:
			now := time.Now()
			if !nextTick.After(now) {
				if loop.handle.Availability() == CoordinatorReady {
					loop.processRequest(domain.ScanTriggerScheduled, nil)
				}
				nextTick = time.Now().Add(loop.interval)
			}
		}
	}
}

func (loop *coordinatorLoop) recover() {
	ctx := context.Background()
	for {
		snapshot, err := loop.store.ScanStatusSnapshot(ctx, nil)
		if err != nil {
			loop.waitForRetry()
			continue
		}
		state := snapshot.AppState.Scan
		if state.ActiveScanID != nil {
			event := domain.ScanFailedEvent{ScanID: *state.ActiveScanID, FailedAtMS: nowMS(), ErrorCode: "SCAN_INTERRUPTED"}
			if _, err := loop.store.MarkScanFailed(ctx, event); err != nil {
				loop.waitForRetry()
				continue
			}
			loop.retryAttempt = 0
			continue
		}
		if loop.handle.Availability() == CoordinatorShuttingDown {
			return
		}
		if state.FollowupState != nil && *state.FollowupState == domain.FollowupQueued {
			if loop.handle.Availability() == CoordinatorShuttingDown {
				return
			}
			scanID := *state.FollowupScanID
			event := domain.FollowupStartedEvent{ScanID: scanID, StartedAtMS: nowMS()}
			if loop.handle.Availability() == CoordinatorShuttingDown {
				return
			}
			if _, err := loop.store.MarkFollowupStartedWithSources(ctx, event, loop.registry.SourceIDs()); err == nil {
				loop.retryAttempt = 0
				loop.spawnWorker(scanID)
				return
			} else if isBusy(err) {
				loop.waitForRetry()
				continue
			} else {
				loop.persistRecoveryFollowupFailure(scanID)
				continue
			}
		}
		if loop.handle.Availability() == CoordinatorShuttingDown {
			return
		}
		scanID := newScanID()
		atMS := nowMS()
		event := domain.ScanStartEvent{ScanID: scanID, Trigger: domain.ScanTriggerStartup, RequestedAtMS: atMS, StartedAtMS: atMS}
		if loop.handle.Availability() == CoordinatorShuttingDown {
			return
		}
		if _, err := loop.store.MarkScanStartedWithSources(ctx, event, loop.registry.SourceIDs()); err == nil {
			loop.retryAttempt = 0
			loop.spawnWorker(scanID)
			return
		} else if isBusy(err) {
			loop.waitForRetry()
			continue
		} else {
			return
		}
	}
}

func (loop *coordinatorLoop) persistRecoveryFollowupFailure(scanID string) {
	for {
		event := domain.FollowupStartFailedEvent{ScanID: scanID, FailedAtMS: nowMS(), ErrorCode: "SCAN_START_FAILED"}
		if _, err := loop.store.MarkFollowupStartFailed(context.Background(), event); err == nil {
			loop.retryAttempt = 0
			return
		}
		loop.waitForRetry()
	}
}

func (loop *coordinatorLoop) waitForRetry() {
	delay := retryDelay(loop.retryAttempt)
	if loop.retryAttempt < ^uint32(0) {
		loop.retryAttempt++
	}
	time.Sleep(delay)
}

func (loop *coordinatorLoop) handleCommand(cmd command) bool {
	switch cmd := cmd.(type) {
	case requestCommand:
		result := loop.requestInner(cmd.trigger)
		cmd.reply <- result
	case workerFinishedCommand:
		loop.handleWorkerFinished(cmd)
		if loop.handle.Availability() == CoordinatorShuttingDown && loop.driveShutdown() {
			return true
		}
	case shutdownCommand:
		if loop.shutdownReply == nil {
			loop.shutdownReply = cmd.reply
		}
		loop.cancelActiveWorker()
		if loop.driveShutdown() {
			return true
		}
	}
	return false
}

func (loop *coordinatorLoop) processRequest(trigger domain.ScanTrigger, reply chan requestResult) {
	result := loop.requestInner(trigger)
	if reply != nil {
		reply <- result
	}
}

func (loop *coordinatorLoop) requestInner(trigger domain.ScanTrigger) requestResult {
	switch loop.handle.Availability() {
	case CoordinatorRecovering:
		return requestResult{err: newRequestError(RequestErrorRecovering, 0)}
	case CoordinatorShuttingDown, CoordinatorStopped:
		return requestResult{err: newRequestError(RequestErrorShuttingDown, 0)}
	case CoordinatorReady:
	}
	ctx := context.Background()
	snapshot, err := loop.store.ScanStatusSnapshot(ctx, nil)
	if err != nil {
		return requestResult{err: newRequestError(RequestErrorStartCommitFailed, commitFailure(err))}
	}
	if loop.handle.Availability() != CoordinatorReady {
		return requestResult{err: newRequestError(RequestErrorShuttingDown, 0)}
	}
	state := snapshot.AppState.Scan
	if state.FollowupState != nil && *state.FollowupState == domain.FollowupQueued {
		return loop.coalescedDisposition(state)
	}
	if state.State == domain.ScanLifecycleRunning {
		event := domain.ReserveScanFollowupEvent{FollowupScanID: newScanID(), Trigger: trigger, RequestedAtMS: nowMS()}
		if loop.handle.Availability() != CoordinatorReady {
			return requestResult{err: newRequestError(RequestErrorShuttingDown, 0)}
		}
		reserved, err := loop.store.ReserveScanFollowup(ctx, event)
		if err != nil {
			return requestResult{err: newRequestError(RequestErrorEnqueueCommitFailed, commitFailure(err))}
		}
		return loop.coalescedDisposition(reserved)
	}
	scanID := newScanID()
	atMS := nowMS()
	event := domain.ScanStartEvent{ScanID: scanID, Trigger: trigger, RequestedAtMS: atMS, StartedAtMS: atMS}
	if loop.handle.Availability() != CoordinatorReady {
		return requestResult{err: newRequestError(RequestErrorShuttingDown, 0)}
	}
	started, err := loop.store.MarkScanStartedWithSources(ctx, event, loop.registry.SourceIDs())
	if err != nil {
		return requestResult{err: newRequestError(RequestErrorStartCommitFailed, commitFailure(err))}
	}
	loop.spawnWorker(scanID)
	return requestResult{disposition: RequestDisposition{Kind: RequestStarted, ScanID: scanID, StatusRevision: started.StatusRevision}}
}

func (loop *coordinatorLoop) coalescedDisposition(state domain.ScanState) requestResult {
	if state.FollowupScanID == nil || state.FollowupEnqueuedStatusRevision == nil {
		return requestResult{err: newRequestError(RequestErrorEnqueueCommitFailed, CommitFailureInternal)}
	}
	return requestResult{disposition: RequestDisposition{
		Kind:           RequestCoalesced,
		ScanID:         *state.FollowupScanID,
		StatusRevision: *state.FollowupEnqueuedStatusRevision,
	}}
}

func (loop *coordinatorLoop) spawnWorker(scanID string) {
	workerCtx, cancel := context.WithCancel(context.Background())
	active := &activeWorker{scanID: scanID, cancel: cancel}
	loop.activeWorker = active
	commands := loop.handle.commands
	loopDone := loop.handle.loopDone
	go func() {
		defer cancel()
		result := loop.runWorker(workerCtx, scanID)
		cmd := workerFinishedCommand{scanID: scanID, result: result}
		select {
		case commands <- cmd:
		case <-loopDone:
		}
	}()
}

type availabilityDecision uint8

const (
	availabilityDecisionAvailable availabilityDecision = iota
	availabilityDecisionSkip
	availabilityDecisionFail
)

func classifyAvailabilityKind(kind source.AvailabilityKind) availabilityDecision {
	switch kind {
	case source.AvailabilityAvailable:
		return availabilityDecisionAvailable
	case source.AvailabilityUnavailable, source.AvailabilityNotInstalled:
		return availabilityDecisionSkip
	default:
		return availabilityDecisionFail
	}
}

func (loop *coordinatorLoop) runWorker(workerCtx context.Context, scanID string) (result workerResult) {
	sourceIDs := loop.registry.SourceIDs()
	reports := make([]source.RunReport, 0, len(sourceIDs))
	defer func() { loop.handle.publishReports(reports) }()
	availabilityCtx := context.WithoutCancel(workerCtx)
	persistenceCtx := context.WithoutCancel(workerCtx)
	factory := source.NewStorageFactory(loop.db)
	failed := false
	for _, sourceID := range sourceIDs {
		if workerCtx.Err() != nil {
			code := "SCAN_CANCELLED"
			reports = append(reports, source.RunReport{ScanID: scanID, Source: sourceID, State: source.RunFailed, ErrorCode: &code, Detail: "source run cancelled before execution"})
			failed = true
			break
		}
		adapter, _ := loop.registry.Get(sourceID)
		descriptor, _ := loop.registry.Descriptor(sourceID)
		availability, err := adapter.Availability(availabilityCtx)
		if err != nil {
			code := source.ErrorCode(err)
			_ = loop.store.MarkSourceScanFailed(persistenceCtx, scanID, sourceID, nowMS(), code)
			reports = append(reports, source.RunReport{ScanID: scanID, Source: sourceID, State: source.RunFailed, ErrorCode: stringPointer(code), Detail: err.Error()})
			failed = true
			continue
		}
		switch classifyAvailabilityKind(availability.Kind()) {
		case availabilityDecisionSkip:
			detail := availability.Reason()
			if availability.Kind() == source.AvailabilityNotInstalled {
				detail = "source is not installed"
			}
			if err := loop.store.MarkSourceScanSkipped(persistenceCtx, scanID, sourceID, nowMS()); err != nil {
				failed = true
			}
			reports = append(reports, source.RunReport{ScanID: scanID, Source: sourceID, State: source.RunSkipped, Detail: detail})
			continue
		case availabilityDecisionFail:
			code := "SOURCE_RUN_FAILED"
			_ = loop.store.MarkSourceScanFailed(persistenceCtx, scanID, sourceID, nowMS(), code)
			reports = append(reports, source.RunReport{ScanID: scanID, Source: sourceID, State: source.RunFailed, ErrorCode: &code, Detail: "invalid or unknown source availability"})
			failed = true
			continue
		}
		if err := loop.store.MarkSourceScanStarted(persistenceCtx, scanID, sourceID, nowMS()); err != nil {
			code := "SOURCE_RUN_FAILED"
			reports = append(reports, source.RunReport{ScanID: scanID, Source: sourceID, State: source.RunFailed, ErrorCode: &code, Detail: "source child could not be started"})
			failed = true
			continue
		}
		runContext, err := factory.Context(workerCtx, scanID, descriptor)
		if err != nil {
			code := "SOURCE_CONTEXT_FAILED"
			_ = loop.store.MarkSourceScanFailed(persistenceCtx, scanID, sourceID, nowMS(), code)
			reports = append(reports, source.RunReport{ScanID: scanID, Source: sourceID, State: source.RunFailed, ErrorCode: &code, Detail: err.Error()})
			failed = true
			continue
		}
		runErr := adapter.RunScan(workerCtx, runContext)
		if runErr != nil {
			code := source.ErrorCode(runErr)
			_ = loop.store.MarkSourceScanFailed(persistenceCtx, scanID, sourceID, nowMS(), code)
			reports = append(reports, source.RunReport{ScanID: scanID, Source: sourceID, State: source.RunFailed, ErrorCode: stringPointer(code), Detail: runErr.Error()})
			failed = true
			continue
		}
		if workerCtx.Err() != nil {
			code := "SCAN_CANCELLED"
			_ = loop.store.MarkSourceScanFailed(persistenceCtx, scanID, sourceID, nowMS(), code)
			reports = append(reports, source.RunReport{ScanID: scanID, Source: sourceID, State: source.RunFailed, ErrorCode: &code, Detail: "source run cancelled during execution"})
			failed = true
			continue
		}
		if err := loop.store.MarkSourceScanCompleted(persistenceCtx, scanID, sourceID, nowMS()); err != nil {
			failed = true
		}
		reports = append(reports, source.RunReport{ScanID: scanID, Source: sourceID, State: source.RunCompleted})
	}
	if failed {
		return workerResult{failed: true, errorCode: "SOURCE_RUN_FAILED"}
	}
	return workerResult{}
}

func (loop *coordinatorLoop) handleWorkerFinished(cmd workerFinishedCommand) {
	active := loop.activeWorker
	if active == nil || active.scanID != cmd.scanID {
		return
	}
	if loop.pendingTerminal != nil && loop.pendingTerminal.scanID == cmd.scanID {
		loop.pendingTerminal = nil
	}
	if loop.persistLocalTerminal(cmd.result) && loop.handle.Availability() == CoordinatorReady {
		loop.tryStartQueued()
	}
}

func (loop *coordinatorLoop) persistLocalTerminal(result workerResult) bool {
	active := loop.activeWorker
	if active == nil {
		return false
	}
	ctx := context.Background()
	var err error
	if loop.handle.Availability() == CoordinatorShuttingDown {
		_, err = loop.store.MarkScanFailed(ctx, domain.ScanFailedEvent{ScanID: active.scanID, FailedAtMS: nowMS(), ErrorCode: "SCAN_CANCELLED"})
	} else if result.failed {
		code := result.errorCode
		if code == "" {
			code = "SOURCE_RUN_FAILED"
		}
		_, err = loop.store.MarkScanFailed(ctx, domain.ScanFailedEvent{ScanID: active.scanID, FailedAtMS: nowMS(), ErrorCode: code})
	} else {
		_, err = loop.store.MarkScanCompleted(ctx, domain.ScanCompletedEvent{ScanID: active.scanID, CompletedAtMS: nowMS()})
	}
	if err != nil {
		loop.pendingTerminal = &pendingTerminal{scanID: active.scanID, result: result}
		loop.scheduleRetry()
		return false
	}
	active.cancel()
	loop.activeWorker = nil
	loop.pendingTerminal = nil
	loop.retryAttempt = 0
	return true
}

func (loop *coordinatorLoop) tryStartQueued() {
	if loop.activeWorker != nil || loop.handle.Availability() != CoordinatorReady {
		return
	}
	ctx := context.Background()
	snapshot, err := loop.store.ScanStatusSnapshot(ctx, nil)
	if err != nil {
		if isBusy(err) {
			loop.scheduleRetry()
		}
		return
	}
	state := snapshot.AppState.Scan
	if state.FollowupState == nil || *state.FollowupState != domain.FollowupQueued {
		return
	}
	if loop.handle.Availability() != CoordinatorReady {
		return
	}
	scanID := *state.FollowupScanID
	event := domain.FollowupStartedEvent{ScanID: scanID, StartedAtMS: nowMS()}
	if loop.handle.Availability() != CoordinatorReady {
		return
	}
	_, err = loop.store.MarkFollowupStartedWithSources(ctx, event, loop.registry.SourceIDs())
	if err == nil {
		loop.retryAttempt = 0
		loop.retryAt = time.Time{}
		loop.spawnWorker(scanID)
		return
	}
	if isBusy(err) {
		loop.scheduleRetry()
		return
	}
	loop.persistFollowupFailure(scanID, "SCAN_START_FAILED")
}

func (loop *coordinatorLoop) retryPending() {
	if loop.pendingTerminal != nil && loop.activeWorker != nil && loop.pendingTerminal.scanID == loop.activeWorker.scanID {
		loop.persistLocalTerminal(loop.pendingTerminal.result)
	}
	if loop.pendingShutdownActive != nil {
		id := *loop.pendingShutdownActive
		_, err := loop.store.MarkScanFailed(context.Background(), domain.ScanFailedEvent{ScanID: id, FailedAtMS: nowMS(), ErrorCode: "SCAN_CANCELLED"})
		if err == nil {
			loop.pendingShutdownActive = nil
			loop.retryAttempt = 0
		} else {
			loop.scheduleRetry()
		}
	}
	if loop.pendingFollowup != nil {
		pending := *loop.pendingFollowup
		loop.persistFollowupFailure(pending.scanID, pending.errorCode)
	}
}

func (loop *coordinatorLoop) persistFollowupFailure(scanID, code string) bool {
	_, err := loop.store.MarkFollowupStartFailed(context.Background(), domain.FollowupStartFailedEvent{ScanID: scanID, FailedAtMS: nowMS(), ErrorCode: code})
	if err != nil {
		loop.pendingFollowup = &pendingFollowupFailure{scanID: scanID, errorCode: code}
		loop.scheduleRetry()
		return false
	}
	loop.pendingFollowup = nil
	loop.retryAttempt = 0
	return true
}

func (loop *coordinatorLoop) scheduleRetry() {
	if loop.retryAt.IsZero() {
		delay := retryDelay(loop.retryAttempt)
		if loop.retryAttempt < ^uint32(0) {
			loop.retryAttempt++
		}
		loop.retryAt = time.Now().Add(delay)
	}
}

func (loop *coordinatorLoop) cancelActiveWorker() {
	if loop.activeWorker == nil {
		return
	}
	loop.activeWorker.cancel()
	if loop.pendingTerminal == nil {
		loop.pendingTerminal = &pendingTerminal{
			scanID: loop.activeWorker.scanID,
			result: workerResult{failed: true, errorCode: "SCAN_CANCELLED"},
		}
	}
}

func (loop *coordinatorLoop) driveShutdown() bool {
	if loop.handle.Availability() != CoordinatorShuttingDown {
		return false
	}
	loop.cancelActiveWorker()
	if !loop.shutdownSnapshotKnown {
		snapshot, err := loop.store.ScanStatusSnapshot(context.Background(), nil)
		if err != nil {
			loop.scheduleRetry()
			return false
		}
		loop.shutdownSnapshotKnown = true
		activeID := snapshot.AppState.Scan.ActiveScanID
		if activeID != nil {
			localActive := loop.activeWorker != nil && loop.activeWorker.scanID == *activeID
			if localActive {
				loop.pendingShutdownActive = nil
			} else {
				if loop.pendingTerminal != nil && loop.pendingTerminal.scanID == *activeID {
					panic("scan has both local and orphan terminal retry ownership")
				}
				id := *activeID
				loop.pendingShutdownActive = &id
			}
		} else {
			loop.pendingShutdownActive = nil
		}
		state := snapshot.AppState.Scan
		if state.FollowupState != nil && *state.FollowupState == domain.FollowupQueued {
			loop.pendingFollowup = &pendingFollowupFailure{scanID: *state.FollowupScanID, errorCode: "SCANNER_UNAVAILABLE"}
		} else {
			loop.pendingFollowup = nil
		}
	}
	loop.cancelActiveWorker()
	if loop.activeWorker != nil || loop.pendingTerminal != nil || loop.pendingShutdownActive != nil || loop.pendingFollowup != nil {
		loop.scheduleRetry()
		return false
	}
	if loop.shutdownReply == nil {
		return false
	}
	reply := loop.shutdownReply
	loop.shutdownReply = nil
	reply <- nil
	loop.handle.availability.Store(uint32(CoordinatorStopped))
	return true
}

func isBusy(err error) bool {
	var storageErr *storage.Error
	return errors.As(err, &storageErr) && storageErr.Kind == storage.ErrorDatabaseBusy
}

func commitFailure(err error) CommitFailureKind {
	if isBusy(err) {
		return CommitFailureBusy
	}
	return CommitFailureInternal
}

func retryDelay(attempt uint32) time.Duration {
	if attempt >= 6 {
		return retryMax
	}
	delay := retryBase * time.Duration(uint64(1)<<attempt)
	if delay > retryMax {
		return retryMax
	}
	return delay
}

func nowMS() int64 {
	value := time.Now().UnixMilli()
	if value < 0 {
		return 0
	}
	return value
}

func newScanID() string {
	sequence := scanIDSequence.Add(1)
	if id, err := uuid.NewRandom(); err == nil {
		return id.String()
	}
	var input [20]byte
	binary.BigEndian.PutUint32(input[0:4], uint32(os.Getpid()))
	binary.BigEndian.PutUint64(input[4:12], uint64(time.Now().UnixNano()))
	binary.BigEndian.PutUint64(input[12:20], sequence)
	digest := sha256.Sum256(input[:])
	var id uuid.UUID
	copy(id[:], digest[:16])
	id[6] = id[6]&0x0f | 0x40
	id[8] = id[8]&0x3f | 0x80
	return id.String()
}

func stringPointer(value string) *string { return &value }
