package ingestion

import (
	"context"
	"database/sql"
	"errors"
	"path/filepath"
	"runtime/pprof"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	"github.com/Hogeexxl/Usagi/internal/usage"
)

type fakeAdapter struct {
	descriptor      atomic.Value
	availabilityFn  func(context.Context) (source.Availability, error)
	runFn           func(context.Context, source.RunContext) error
	availabilityNum atomic.Int32
	runNum          atomic.Int32
}

func newFakeAdapter(id domain.SourceID) *fakeAdapter {
	adapter := &fakeAdapter{}
	descriptor, err := source.NewDescriptor(id, string(id))
	if err != nil {
		panic(err)
	}
	adapter.descriptor.Store(descriptor)
	return adapter
}

func (a *fakeAdapter) Descriptor() source.Descriptor {
	return a.descriptor.Load().(source.Descriptor)
}

func (a *fakeAdapter) Availability(ctx context.Context) (source.Availability, error) {
	a.availabilityNum.Add(1)
	if a.availabilityFn != nil {
		return a.availabilityFn(ctx)
	}
	return source.Available(), nil
}

func (a *fakeAdapter) RunScan(ctx context.Context, run source.RunContext) error {
	a.runNum.Add(1)
	if a.runFn != nil {
		return a.runFn(ctx, run)
	}
	return nil
}

func makeRegistry(t *testing.T, adapters ...*fakeAdapter) *source.Registry {
	t.Helper()
	values := make([]source.Adapter, len(adapters))
	for i, adapter := range adapters {
		values[i] = adapter
	}
	registry, err := source.NewRegistry(values...)
	if err != nil {
		t.Fatal(err)
	}
	return registry
}

func openCoordinatorTestDB(t *testing.T) *storage.DB {
	t.Helper()
	db, err := storage.Open(context.Background(), storage.Config{Path: filepath.Join(t.TempDir(), "coordinator.sqlite3")})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := db.Close(); err != nil {
			t.Error(err)
		}
	})
	return db
}

type storeOperation string

const (
	opSnapshot        storeOperation = "snapshot"
	opStart           storeOperation = "start"
	opReserve         storeOperation = "reserve"
	opFollowupStart   storeOperation = "followup_start"
	opFollowupFailure storeOperation = "followup_failure"
	opGlobalComplete  storeOperation = "global_complete"
	opGlobalFailed    storeOperation = "global_failed"
	opChildStart      storeOperation = "child_start"
	opChildComplete   storeOperation = "child_complete"
	opChildSkip       storeOperation = "child_skip"
	opChildFailed     storeOperation = "child_failed"
)

type storeCall struct {
	operation   storeOperation
	scanID      string
	source      domain.SourceID
	errorCode   string
	ctxCanceled bool
}

type operationGate struct {
	entered chan struct{}
	release chan struct{}
}

type scriptedStore struct {
	base lifecycleStore

	mu     sync.Mutex
	faults map[storeOperation][]error
	gates  map[storeOperation][]*operationGate
	calls  []storeCall
}

func newScriptedStore(base lifecycleStore) *scriptedStore {
	return &scriptedStore{
		base:   base,
		faults: make(map[storeOperation][]error),
		gates:  make(map[storeOperation][]*operationGate),
	}
}

func (s *scriptedStore) inject(operation storeOperation, err error) {
	s.mu.Lock()
	s.faults[operation] = append(s.faults[operation], err)
	s.mu.Unlock()
}

func (s *scriptedStore) blockNext(operation storeOperation) *operationGate {
	gate := &operationGate{entered: make(chan struct{}, 1), release: make(chan struct{})}
	s.mu.Lock()
	s.gates[operation] = append(s.gates[operation], gate)
	s.mu.Unlock()
	return gate
}

func (s *scriptedStore) before(operation storeOperation, ctx context.Context, scanID string, sourceID domain.SourceID, code string) error {
	call := storeCall{operation: operation, scanID: scanID, source: sourceID, errorCode: code}
	if ctx != nil {
		call.ctxCanceled = ctx.Err() != nil
	}
	s.mu.Lock()
	s.calls = append(s.calls, call)
	var gate *operationGate
	if pending := s.gates[operation]; len(pending) != 0 {
		gate = pending[0]
		s.gates[operation] = pending[1:]
	}
	var fault error
	if pending := s.faults[operation]; len(pending) != 0 {
		fault = pending[0]
		s.faults[operation] = pending[1:]
	}
	s.mu.Unlock()
	if gate != nil {
		gate.entered <- struct{}{}
		<-gate.release
	}
	return fault
}

func (s *scriptedStore) callsSnapshot() []storeCall {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]storeCall(nil), s.calls...)
}

func (s *scriptedStore) count(operation storeOperation) int {
	n := 0
	for _, call := range s.callsSnapshot() {
		if call.operation == operation {
			n++
		}
	}
	return n
}

func (s *scriptedStore) callsFor(operation storeOperation) []storeCall {
	var calls []storeCall
	for _, call := range s.callsSnapshot() {
		if call.operation == operation {
			calls = append(calls, call)
		}
	}
	return calls
}

func (s *scriptedStore) ScanStatusSnapshot(ctx context.Context, id *string) (domain.ScanStatusSnapshot, error) {
	if err := s.before(opSnapshot, ctx, valueOrEmpty(id), "", ""); err != nil {
		return domain.ScanStatusSnapshot{}, err
	}
	return s.base.ScanStatusSnapshot(ctx, id)
}

func (s *scriptedStore) MarkScanStartedWithSources(ctx context.Context, event domain.ScanStartEvent, sources []domain.SourceID) (domain.ScanState, error) {
	if err := s.before(opStart, ctx, event.ScanID, "", ""); err != nil {
		return domain.ScanState{}, err
	}
	return s.base.MarkScanStartedWithSources(ctx, event, sources)
}

func (s *scriptedStore) ReserveScanFollowup(ctx context.Context, event domain.ReserveScanFollowupEvent) (domain.ScanState, error) {
	if err := s.before(opReserve, ctx, event.FollowupScanID, "", ""); err != nil {
		return domain.ScanState{}, err
	}
	return s.base.ReserveScanFollowup(ctx, event)
}

func (s *scriptedStore) MarkFollowupStartedWithSources(ctx context.Context, event domain.FollowupStartedEvent, sources []domain.SourceID) (domain.ScanState, error) {
	if err := s.before(opFollowupStart, ctx, event.ScanID, "", ""); err != nil {
		return domain.ScanState{}, err
	}
	return s.base.MarkFollowupStartedWithSources(ctx, event, sources)
}

func (s *scriptedStore) MarkFollowupStartFailed(ctx context.Context, event domain.FollowupStartFailedEvent) (domain.ScanState, error) {
	if err := s.before(opFollowupFailure, ctx, event.ScanID, "", event.ErrorCode); err != nil {
		return domain.ScanState{}, err
	}
	return s.base.MarkFollowupStartFailed(ctx, event)
}

func (s *scriptedStore) MarkScanCompleted(ctx context.Context, event domain.ScanCompletedEvent) (domain.ScanState, error) {
	if err := s.before(opGlobalComplete, ctx, event.ScanID, "", ""); err != nil {
		return domain.ScanState{}, err
	}
	return s.base.MarkScanCompleted(ctx, event)
}

func (s *scriptedStore) MarkScanFailed(ctx context.Context, event domain.ScanFailedEvent) (domain.ScanState, error) {
	if err := s.before(opGlobalFailed, ctx, event.ScanID, "", event.ErrorCode); err != nil {
		return domain.ScanState{}, err
	}
	return s.base.MarkScanFailed(ctx, event)
}

func (s *scriptedStore) MarkSourceScanStarted(ctx context.Context, scanID string, sourceID domain.SourceID, atMS int64) error {
	if err := s.before(opChildStart, ctx, scanID, sourceID, ""); err != nil {
		return err
	}
	return s.base.MarkSourceScanStarted(ctx, scanID, sourceID, atMS)
}

func (s *scriptedStore) MarkSourceScanCompleted(ctx context.Context, scanID string, sourceID domain.SourceID, atMS int64) error {
	if err := s.before(opChildComplete, ctx, scanID, sourceID, ""); err != nil {
		return err
	}
	return s.base.MarkSourceScanCompleted(ctx, scanID, sourceID, atMS)
}

func (s *scriptedStore) MarkSourceScanSkipped(ctx context.Context, scanID string, sourceID domain.SourceID, atMS int64) error {
	if err := s.before(opChildSkip, ctx, scanID, sourceID, ""); err != nil {
		return err
	}
	return s.base.MarkSourceScanSkipped(ctx, scanID, sourceID, atMS)
}

func (s *scriptedStore) MarkSourceScanFailed(ctx context.Context, scanID string, sourceID domain.SourceID, atMS int64, code string) error {
	if err := s.before(opChildFailed, ctx, scanID, sourceID, code); err != nil {
		return err
	}
	return s.base.MarkSourceScanFailed(ctx, scanID, sourceID, atMS, code)
}

func valueOrEmpty(value *string) string {
	if value == nil {
		return ""
	}
	return *value
}

func injectedError(kind storage.ErrorKind) error {
	return &storage.Error{Kind: kind, Err: errors.New("scripted lifecycle failure")}
}

func waitUntil(t *testing.T, description string, condition func() bool) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for !condition() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", description)
		}
		time.Sleep(time.Millisecond)
	}
}

func startTestHandle(t *testing.T, db *storage.DB, registry *source.Registry, store lifecycleStore, interval time.Duration) *Handle {
	t.Helper()
	h, err := startWithIntervalForTest(interval, db, registry, store)
	if err != nil {
		t.Fatal(err)
	}
	return h
}

func waitReady(t *testing.T, h *Handle) {
	t.Helper()
	waitUntil(t, "coordinator Ready", func() bool { return h.Availability() == CoordinatorReady })
}

func waitIdle(t *testing.T, db *storage.DB) domain.ScanStatusSnapshot {
	t.Helper()
	var snapshot domain.ScanStatusSnapshot
	waitUntil(t, "no active scan", func() bool {
		var err error
		snapshot, err = db.ScanStatusSnapshot(context.Background(), nil)
		return err == nil && snapshot.AppState.Scan.ActiveScanID == nil &&
			(snapshot.AppState.Scan.FollowupState == nil || *snapshot.AppState.Scan.FollowupState != domain.FollowupQueued)
	})
	return snapshot
}

func readTarget(t *testing.T, db *storage.DB, scanID string) (domain.ScanStatusSnapshot, domain.ScanRun) {
	t.Helper()
	snapshot, err := db.ScanStatusSnapshot(context.Background(), &scanID)
	if err != nil {
		t.Fatal(err)
	}
	if snapshot.TargetScan == nil {
		t.Fatalf("scan %q has no row", scanID)
	}
	return snapshot, *snapshot.TargetScan
}

func findRows(t *testing.T, db *storage.DB, scanID string) []domain.ScanRun {
	t.Helper()
	var runs []domain.ScanRun
	err := db.Read(context.Background(), func(conn *sql.Conn) error {
		query := `SELECT scan_id,trigger,request_kind,state,requested_at_ms,enqueued_status_revision,
started_at_ms,started_status_revision,finished_at_ms,terminal_status_revision,error_code FROM scan_runs`
		args := []any(nil)
		if scanID != "" {
			query += " WHERE scan_id=?"
			args = append(args, scanID)
		}
		query += " ORDER BY requested_at_ms,scan_id"
		rows, err := conn.QueryContext(context.Background(), query, args...)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var run domain.ScanRun
			var trigger, requestKind, state string
			var enqueued, started, startedRevision, finished, terminalRevision sql.NullInt64
			var errorCode sql.NullString
			if err := rows.Scan(&run.ScanID, &trigger, &requestKind, &state, &run.RequestedAtMS,
				&enqueued, &started, &startedRevision, &finished, &terminalRevision, &errorCode); err != nil {
				return err
			}
			if run.Trigger, err = domain.ParseScanTrigger(trigger); err != nil {
				return err
			}
			if run.RequestKind, err = domain.ParseScanRequestKind(requestKind); err != nil {
				return err
			}
			if run.State, err = domain.ParseScanRunState(state); err != nil {
				return err
			}
			run.EnqueuedStatusRevision = nullableInt64(enqueued)
			run.StartedAtMS = nullableInt64(started)
			run.StartedStatusRevision = nullableInt64(startedRevision)
			run.FinishedAtMS = nullableInt64(finished)
			run.TerminalStatusRevision = nullableInt64(terminalRevision)
			if errorCode.Valid {
				run.ErrorCode = &errorCode.String
			}
			runs = append(runs, run)
		}
		return rows.Err()
	})
	if err != nil {
		t.Fatal(err)
	}
	return runs
}

func nullableInt64(value sql.NullInt64) *int64 {
	if !value.Valid {
		return nil
	}
	return &value.Int64
}

func writeCanonicalData(run source.RunContext, eventID string) error {
	identity, err := domain.NewSessionIdentity("thread-"+eventID, run.Source(), "native-"+eventID)
	if err != nil {
		return err
	}
	patch, err := domain.NewResolvedThreadPatch(identity, 1)
	if err != nil {
		return err
	}
	turnKey := "turn-" + eventID
	reasoning := "high"
	cost := int64(100)
	cacheWrite := int64(1)
	event := usage.CanonicalUsageEventWrite{
		EventID: eventID, Kind: usage.EventKindNormal, OccurredAtMS: 10,
		ThreadID: identity.ThreadID, RootSessionID: identity.ThreadID,
		TurnKey: &turnKey, Model: "model", ReasoningEffort: &reasoning,
		EstimatedCostNanosUSD: &cost,
		Usage: usage.NormalizedTokenUsage{
			InputTokens: 10, CachedTokens: 2, CacheWriteTokens: &cacheWrite,
			OutputTokens: 4, ReasoningTokens: 1, TotalTokens: 14,
		},
		CreatedAtMS: 11,
	}
	return run.Storage().Write(func(tx *source.WriteTx) error {
		if err := tx.EnsureUsageEpoch(); err != nil {
			return err
		}
		epoch, err := tx.BeginOrResumeUsageBuild(1)
		if err != nil {
			return err
		}
		if err := tx.UpsertThread(identity, patch); err != nil {
			return err
		}
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetBuild, event); err != nil {
			return err
		}
		_, err = tx.ActivateUsageBuild(epoch, 1)
		return err
	})
}

func shutdownHandle(t *testing.T, h *Handle) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := h.Shutdown(ctx); err != nil {
		t.Fatal(err)
	}
}

func assertNoDurableRunning(t *testing.T, db *storage.DB) {
	t.Helper()
	snapshot, err := db.ScanStatusSnapshot(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	if snapshot.AppState.Scan.ActiveScanID != nil {
		t.Fatalf("durable active scan remains: %q", *snapshot.AppState.Scan.ActiveScanID)
	}
	if snapshot.AppState.Scan.FollowupState != nil && *snapshot.AppState.Scan.FollowupState == domain.FollowupQueued {
		t.Fatalf("durable queued follow-up remains: %v", snapshot.AppState.Scan.FollowupScanID)
	}
	for _, child := range snapshot.Sources {
		if child.State == domain.SourceScanQueued || child.State == domain.SourceScanRunning {
			t.Fatalf("durable non-terminal child remains: %+v", child)
		}
	}
}

func TestStartRejectsNilDependencies(t *testing.T) {
	db := openCoordinatorTestDB(t)
	registry := makeRegistry(t)
	if _, err := Start(DefaultConfig(), nil, registry); !errors.Is(err, ErrCoordinatorUnavailable) {
		t.Fatalf("Start(nil DB) error = %v", err)
	}
	if _, err := Start(DefaultConfig(), db, nil); !errors.Is(err, ErrCoordinatorUnavailable) {
		t.Fatalf("Start(nil Registry) error = %v", err)
	}
	if _, err := Start(Config{Interval: MinInterval - time.Nanosecond}, nil, nil); err == nil || errors.Is(err, ErrCoordinatorUnavailable) {
		t.Fatalf("invalid config must be checked before nil dependencies, got %v", err)
	}
}

func TestRequestRejectedWhileRecovering(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	gate := store.blockNext(opSnapshot)
	h := startTestHandle(t, db, makeRegistry(t), store, time.Hour)
	select {
	case <-gate.entered:
	case <-time.After(3 * time.Second):
		t.Fatal("recovery did not enter scripted snapshot")
	}
	if _, err := h.Request(context.Background(), domain.ScanTriggerManual); err == nil {
		t.Fatal("Request during recovery succeeded")
	} else if requestErr, ok := err.(RequestError); !ok || requestErr.Kind != RequestErrorRecovering || requestErr.CommitFailure != nil {
		t.Fatalf("Request during recovery error = %#v", err)
	}
	close(gate.release)
	waitReady(t, h)
	waitIdle(t, db)
	shutdownHandle(t, h)
	if _, err := h.Request(context.Background(), domain.ScanTriggerManual); err == nil {
		t.Fatal("Request after shutdown succeeded")
	} else if requestErr, ok := err.(RequestError); !ok || requestErr.Kind != RequestErrorShuttingDown || requestErr.CommitFailure != nil {
		t.Fatalf("Request after shutdown error = %#v", err)
	}
}

func TestRequestAckAfterDurableCommit(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	h := startTestHandle(t, db, makeRegistry(t), store, time.Hour)
	waitReady(t, h)
	waitIdle(t, db)
	for _, test := range []struct {
		kind     storage.ErrorKind
		wantKind CommitFailureKind
	}{
		{storage.ErrorDatabaseBusy, CommitFailureBusy},
		{storage.ErrorInvalidState, CommitFailureInternal},
	} {
		store.inject(opStart, injectedError(test.kind))
		_, err := h.Request(context.Background(), domain.ScanTriggerManual)
		requestErr, ok := err.(RequestError)
		if !ok || requestErr.Kind != RequestErrorStartCommitFailed || requestErr.CommitFailure == nil || *requestErr.CommitFailure != test.wantKind {
			t.Fatalf("Request start failure = %#v, want failure kind %d", err, test.wantKind)
		}
		if snapshot, err := db.ScanStatusSnapshot(context.Background(), nil); err != nil || snapshot.AppState.Scan.ActiveScanID != nil {
			t.Fatalf("failed start changed durable active state: snapshot=%+v err=%v", snapshot, err)
		}
	}
	disposition, err := h.Request(context.Background(), domain.ScanTriggerManual)
	if err != nil || disposition.Kind != RequestStarted || disposition.ScanID == "" || disposition.StatusRevision <= 0 {
		t.Fatalf("successful Request = %+v, %v", disposition, err)
	}
	shutdownHandle(t, h)
	assertNoDurableRunning(t, db)
}

func TestLifecycleStoreFaultInjectionSeam(t *testing.T) {
	db := openCoordinatorTestDB(t)
	if _, err := db.MarkScanStartedWithSources(context.Background(), domain.ScanStartEvent{ScanID: "interrupted", Trigger: domain.ScanTriggerStartup, RequestedAtMS: 1, StartedAtMS: 1}, nil); err != nil {
		t.Fatal(err)
	}
	store := newScriptedStore(db)
	store.inject(opSnapshot, injectedError(storage.ErrorDatabaseBusy))
	store.inject(opSnapshot, injectedError(storage.ErrorInvalidState))
	store.inject(opGlobalFailed, injectedError(storage.ErrorInvalidState))
	store.inject(opStart, injectedError(storage.ErrorDatabaseBusy))
	h := startTestHandle(t, db, makeRegistry(t), store, time.Hour)
	waitReady(t, h)
	waitIdle(t, db)
	if store.count(opSnapshot) < 3 || store.count(opStart) < 2 {
		t.Fatalf("recovery did not retry injected snapshot/start faults: calls=%+v", store.callsSnapshot())
	}
	if store.count(opGlobalFailed) < 2 {
		t.Fatalf("recovery did not retry interrupted terminal failure: %+v", store.callsSnapshot())
	}
	_, interrupted := readTarget(t, db, "interrupted")
	if interrupted.State != domain.ScanRunFailed || interrupted.ErrorCode == nil || *interrupted.ErrorCode != "SCAN_INTERRUPTED" {
		t.Fatalf("recovered interrupted row = %+v", interrupted)
	}
	for _, call := range store.callsSnapshot() {
		if call.ctxCanceled {
			t.Fatalf("lifecycle store received canceled Context: %+v", call)
		}
	}
	shutdownHandle(t, h)

	db = openCoordinatorTestDB(t)
	store = newScriptedStore(db)
	adapter := newFakeAdapter("codex")
	started := make(chan struct{}, 2)
	releaseFirst := make(chan struct{})
	var runs atomic.Int32
	adapter.runFn = func(ctx context.Context, _ source.RunContext) error {
		if runs.Add(1) == 1 {
			started <- struct{}{}
			select {
			case <-releaseFirst:
			case <-ctx.Done():
			}
			return nil
		}
		started <- struct{}{}
		<-ctx.Done()
		return nil
	}
	h = startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
	<-started
	waitReady(t, h)
	for _, test := range []struct {
		kind     storage.ErrorKind
		wantKind CommitFailureKind
	}{
		{storage.ErrorDatabaseBusy, CommitFailureBusy},
		{storage.ErrorInvalidState, CommitFailureInternal},
	} {
		store.inject(opReserve, injectedError(test.kind))
		_, err := h.Request(context.Background(), domain.ScanTriggerManual)
		requestErr, ok := err.(RequestError)
		if !ok || requestErr.Kind != RequestErrorEnqueueCommitFailed || requestErr.CommitFailure == nil || *requestErr.CommitFailure != test.wantKind {
			t.Fatalf("follow-up reserve failure = %#v, want failure kind %d", err, test.wantKind)
		}
	}
	disposition, err := h.Request(context.Background(), domain.ScanTriggerManual)
	if err != nil || disposition.Kind != RequestCoalesced {
		t.Fatalf("durable follow-up after injected Reserve failures = %+v, %v", disposition, err)
	}
	store.inject(opFollowupStart, injectedError(storage.ErrorDatabaseBusy))
	close(releaseFirst)
	select {
	case <-started:
	case <-time.After(3 * time.Second):
		t.Fatal("Busy follow-up start was not retried")
	}
	secondFollowup, err := h.Request(context.Background(), domain.ScanTriggerManual)
	if err != nil || secondFollowup.Kind != RequestCoalesced {
		t.Fatalf("queued follow-up during second Worker = %+v, %v", secondFollowup, err)
	}
	store.inject(opFollowupFailure, injectedError(storage.ErrorInvalidState))
	shutdownHandle(t, h)
	if store.count(opFollowupFailure) < 2 {
		t.Fatalf("failed shutdown follow-up persistence was not retried: %+v", store.callsSnapshot())
	}
	assertNoDurableRunning(t, db)
}

func TestRequestCallerTimeoutAfterEnqueueDoesNotBlockCoordinator(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	h := startTestHandle(t, db, makeRegistry(t), store, time.Hour)
	waitReady(t, h)
	waitIdle(t, db)
	gate := store.blockNext(opSnapshot)
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Millisecond)
	defer cancel()
	result := make(chan error, 1)
	go func() {
		_, err := h.Request(ctx, domain.ScanTriggerManual)
		result <- err
	}()
	select {
	case <-gate.entered:
	case <-time.After(3 * time.Second):
		t.Fatal("Request was not processed after enqueue")
	}
	if err := <-result; !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("timed out Request error = %v", err)
	}
	close(gate.release)
	waitUntil(t, "timed-out Request durable start", func() bool { return store.count(opStart) >= 2 })
	shutdownHandle(t, h)
	assertNoDurableRunning(t, db)
}

func TestOnlyOneActiveWorker(t *testing.T) {
	db := openCoordinatorTestDB(t)
	adapter := newFakeAdapter("alpha")
	started := make(chan string, 4)
	release := make(chan struct{}, 4)
	var active atomic.Int32
	var maximum atomic.Int32
	adapter.runFn = func(_ context.Context, run source.RunContext) error {
		current := active.Add(1)
		for {
			old := maximum.Load()
			if old >= current || maximum.CompareAndSwap(old, current) {
				break
			}
		}
		started <- run.ScanID()
		<-release
		active.Add(-1)
		return nil
	}
	h := startTestHandle(t, db, makeRegistry(t, adapter), db, time.Hour)
	firstID := <-started
	for i := 0; i < 12; i++ {
		disposition, err := h.Request(context.Background(), domain.ScanTriggerManual)
		if err != nil || disposition.Kind != RequestCoalesced {
			t.Fatalf("active request %d = %+v, %v", i, disposition, err)
		}
		if i > 0 {
			previous, _ := readTarget(t, db, disposition.ScanID)
			if previous.TargetScan == nil || previous.TargetScan.State != domain.ScanRunQueued {
				t.Fatalf("coalesced follow-up is not durable: %+v", previous)
			}
		}
	}
	release <- struct{}{}
	secondID := <-started
	if secondID == firstID {
		t.Fatal("follow-up reused active Scan ID")
	}
	release <- struct{}{}
	waitIdle(t, db)
	if maximum.Load() != 1 {
		t.Fatalf("maximum concurrent workers = %d", maximum.Load())
	}
	shutdownHandle(t, h)
}

func TestWorkerRunsSourcesSequentially(t *testing.T) {
	db := openCoordinatorTestDB(t)
	first := newFakeAdapter("alpha")
	second := newFakeAdapter("beta")
	started := make(chan string, 2)
	releaseFirst := make(chan struct{})
	releaseSecond := make(chan struct{})
	var active atomic.Int32
	var maximum atomic.Int32
	first.runFn = func(_ context.Context, run source.RunContext) error {
		updateMaximum(&maximum, active.Add(1))
		started <- string(run.Source())
		<-releaseFirst
		active.Add(-1)
		return nil
	}
	second.runFn = func(_ context.Context, run source.RunContext) error {
		updateMaximum(&maximum, active.Add(1))
		started <- string(run.Source())
		<-releaseSecond
		active.Add(-1)
		return nil
	}
	h := startTestHandle(t, db, makeRegistry(t, second, first), db, time.Hour)
	if got := <-started; got != "alpha" {
		t.Fatalf("first Source = %q", got)
	}
	select {
	case got := <-started:
		t.Fatalf("second Source %q started before first completed", got)
	case <-time.After(20 * time.Millisecond):
	}
	close(releaseFirst)
	if got := <-started; got != "beta" {
		t.Fatalf("second Source = %q", got)
	}
	close(releaseSecond)
	waitIdle(t, db)
	if maximum.Load() != 1 {
		t.Fatalf("maximum concurrent Source adapters = %d", maximum.Load())
	}
	shutdownHandle(t, h)
}

func updateMaximum(maximum *atomic.Int32, value int32) {
	for {
		old := maximum.Load()
		if old >= value || maximum.CompareAndSwap(old, value) {
			return
		}
	}
}

func TestRecoveryMarksInterruptedBeforeStartup(t *testing.T) {
	db := openCoordinatorTestDB(t)
	adapter := newFakeAdapter("codex")
	started := make(chan struct{}, 1)
	release := make(chan struct{})
	adapter.runFn = func(context.Context, source.RunContext) error {
		started <- struct{}{}
		<-release
		return nil
	}
	registry := makeRegistry(t, adapter)
	if _, err := db.MarkScanStartedWithSources(context.Background(), domain.ScanStartEvent{ScanID: "old-active", Trigger: domain.ScanTriggerStartup, RequestedAtMS: 1, StartedAtMS: 1}, registry.SourceIDs()); err != nil {
		t.Fatal(err)
	}
	store := newScriptedStore(db)
	h := startTestHandle(t, db, registry, store, time.Hour)
	select {
	case <-started:
	case <-time.After(3 * time.Second):
		t.Fatal("recovery Startup worker did not start")
	}
	_, old := readTarget(t, db, "old-active")
	if old.State != domain.ScanRunFailed || old.ErrorCode == nil || *old.ErrorCode != "SCAN_INTERRUPTED" {
		t.Fatalf("interrupted Scan = %+v", old)
	}
	calls := store.callsSnapshot()
	var interruptedAt, startupAt int = -1, -1
	for i, call := range calls {
		if call.operation == opGlobalFailed && call.errorCode == "SCAN_INTERRUPTED" {
			interruptedAt = i
		}
		if call.operation == opStart && call.scanID != "old-active" {
			startupAt = i
		}
	}
	if interruptedAt < 0 || startupAt <= interruptedAt {
		t.Fatalf("Startup started before interrupted terminal: %+v", calls)
	}
	close(release)
	shutdownHandle(t, h)
}

func TestRecoveryStartsQueuedFollowupFirst(t *testing.T) {
	db := openCoordinatorTestDB(t)
	adapter := newFakeAdapter("codex")
	started := make(chan string, 2)
	release := make(chan struct{})
	adapter.runFn = func(_ context.Context, run source.RunContext) error {
		started <- run.ScanID()
		<-release
		return nil
	}
	registry := makeRegistry(t, adapter)
	if _, err := db.MarkScanStartedWithSources(context.Background(), domain.ScanStartEvent{ScanID: "old-active", Trigger: domain.ScanTriggerStartup, RequestedAtMS: 1, StartedAtMS: 1}, registry.SourceIDs()); err != nil {
		t.Fatal(err)
	}
	if _, err := db.ReserveScanFollowup(context.Background(), domain.ReserveScanFollowupEvent{FollowupScanID: "recovery-followup", Trigger: domain.ScanTriggerManual, RequestedAtMS: 2}); err != nil {
		t.Fatal(err)
	}
	if _, err := db.MarkScanFailed(context.Background(), domain.ScanFailedEvent{ScanID: "old-active", FailedAtMS: 3, ErrorCode: "SCAN_INTERRUPTED"}); err != nil {
		t.Fatal(err)
	}
	store := newScriptedStore(db)
	h := startTestHandle(t, db, registry, store, time.Hour)
	if got := <-started; got != "recovery-followup" {
		t.Fatalf("first recovered worker = %q", got)
	}
	waitReady(t, h)
	if store.count(opStart) != 0 || store.count(opFollowupStart) != 1 {
		t.Fatalf("recovery did not prioritize queued follow-up: %+v", store.callsSnapshot())
	}
	close(release)
	shutdownHandle(t, h)
}

func TestRecoveryPersistsFollowupStartFailure(t *testing.T) {
	db := openCoordinatorTestDB(t)
	adapter := newFakeAdapter("codex")
	started := make(chan string, 2)
	release := make(chan struct{})
	adapter.runFn = func(_ context.Context, run source.RunContext) error {
		started <- run.ScanID()
		<-release
		return nil
	}
	registry := makeRegistry(t, adapter)
	if _, err := db.MarkScanStartedWithSources(context.Background(), domain.ScanStartEvent{ScanID: "old-active", Trigger: domain.ScanTriggerStartup, RequestedAtMS: 1, StartedAtMS: 1}, registry.SourceIDs()); err != nil {
		t.Fatal(err)
	}
	if _, err := db.ReserveScanFollowup(context.Background(), domain.ReserveScanFollowupEvent{FollowupScanID: "failed-followup", Trigger: domain.ScanTriggerManual, RequestedAtMS: 2}); err != nil {
		t.Fatal(err)
	}
	if _, err := db.MarkScanFailed(context.Background(), domain.ScanFailedEvent{ScanID: "old-active", FailedAtMS: 3, ErrorCode: "SCAN_INTERRUPTED"}); err != nil {
		t.Fatal(err)
	}
	store := newScriptedStore(db)
	store.inject(opFollowupStart, injectedError(storage.ErrorInvalidState))
	h := startTestHandle(t, db, registry, store, time.Hour)
	startupID := <-started
	if startupID == "failed-followup" {
		t.Fatal("recovery ran a follow-up whose start failed")
	}
	_, failed := readTarget(t, db, "failed-followup")
	if failed.State != domain.ScanRunStartFailed || failed.ErrorCode == nil || *failed.ErrorCode != "SCAN_START_FAILED" {
		t.Fatalf("recovery follow-up failure = %+v", failed)
	}
	if calls := store.callsFor(opFollowupFailure); len(calls) != 1 || calls[0].errorCode != "SCAN_START_FAILED" {
		t.Fatalf("recovery did not persist the specified start failure: %+v", calls)
	}
	close(release)
	shutdownHandle(t, h)
}

func TestRecoveryNonBusyStartupStartFailureBecomesReady(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	store.inject(opStart, injectedError(storage.ErrorInvalidState))
	h := startTestHandle(t, db, makeRegistry(t), store, time.Hour)
	waitReady(t, h)
	if snapshot, err := db.ScanStatusSnapshot(context.Background(), nil); err != nil || snapshot.AppState.Scan.ActiveScanID != nil {
		t.Fatalf("failed Startup unexpectedly persisted a running Scan: %+v %v", snapshot, err)
	}
	disposition, err := h.Request(context.Background(), domain.ScanTriggerManual)
	if err != nil || disposition.Kind != RequestStarted {
		t.Fatalf("manual request after Startup failure = %+v, %v", disposition, err)
	}
	shutdownHandle(t, h)
	assertNoDurableRunning(t, db)
}

func TestTryStartQueuedRetriesSnapshotBusyOnly(t *testing.T) {
	for _, test := range []struct {
		name      string
		kind      storage.ErrorKind
		wantRetry bool
	}{
		{"busy", storage.ErrorDatabaseBusy, true},
		{"internal", storage.ErrorInvalidState, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			db := openCoordinatorTestDB(t)
			store := newScriptedStore(db)
			adapter := newFakeAdapter("codex")
			var calls atomic.Int32
			firstStarted := make(chan struct{}, 1)
			firstRelease := make(chan struct{})
			followupStarted := make(chan struct{}, 1)
			followupRelease := make(chan struct{})
			adapter.runFn = func(ctx context.Context, _ source.RunContext) error {
				switch calls.Add(1) {
				case 1:
					firstStarted <- struct{}{}
					select {
					case <-firstRelease:
						return nil
					case <-ctx.Done():
						return nil
					}
				default:
					followupStarted <- struct{}{}
					select {
					case <-followupRelease:
						return nil
					case <-ctx.Done():
						return nil
					}
				}
			}
			h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
			<-firstStarted
			waitReady(t, h)
			disposition, err := h.Request(context.Background(), domain.ScanTriggerManual)
			if err != nil || disposition.Kind != RequestCoalesced {
				t.Fatalf("follow-up request = %+v, %v", disposition, err)
			}
			store.inject(opSnapshot, injectedError(test.kind))
			close(firstRelease)
			waitUntil(t, "normal queued-start snapshot", func() bool { return store.count(opSnapshot) >= 3 })
			if test.wantRetry {
				select {
				case <-followupStarted:
				case <-time.After(2 * time.Second):
					t.Fatal("Busy snapshot did not schedule the queued-start retry")
				}
				close(followupRelease)
				waitIdle(t, db)
			} else {
				time.Sleep(80 * time.Millisecond)
				if got := store.count(opSnapshot); got != 3 {
					t.Fatalf("non-Busy snapshot scheduled an extra retry: %d calls", got)
				}
				snapshot, err := db.ScanStatusSnapshot(context.Background(), nil)
				if err != nil || snapshot.AppState.Scan.FollowupState == nil || *snapshot.AppState.Scan.FollowupState != domain.FollowupQueued {
					t.Fatalf("non-Busy snapshot changed queued follow-up: %+v, %v", snapshot.AppState.Scan, err)
				}
			}
			shutdownHandle(t, h)
			assertNoDurableRunning(t, db)
		})
	}
}

func TestQueuedFollowupStartFailureBecomesTerminal(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	adapter := newFakeAdapter("codex")
	firstStarted := make(chan struct{}, 1)
	firstRelease := make(chan struct{})
	var calls atomic.Int32
	adapter.runFn = func(ctx context.Context, _ source.RunContext) error {
		if calls.Add(1) == 1 {
			firstStarted <- struct{}{}
			select {
			case <-firstRelease:
			case <-ctx.Done():
			}
		}
		return nil
	}
	h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
	<-firstStarted
	waitReady(t, h)
	disposition, err := h.Request(context.Background(), domain.ScanTriggerManual)
	if err != nil || disposition.Kind != RequestCoalesced {
		t.Fatalf("follow-up request = %+v, %v", disposition, err)
	}
	store.inject(opFollowupStart, injectedError(storage.ErrorInvalidState))
	close(firstRelease)
	waitUntil(t, "durable queued follow-up start failure transition", func() bool {
		_, followup := readTarget(t, db, disposition.ScanID)
		return followup.State == domain.ScanRunStartFailed
	})
	if count := store.count(opFollowupFailure); count != 1 {
		t.Fatalf("follow-up start failure persistence calls = %d, want 1", count)
	}
	_, followup := readTarget(t, db, disposition.ScanID)
	if followup.State != domain.ScanRunStartFailed || followup.ErrorCode == nil || *followup.ErrorCode != "SCAN_START_FAILED" {
		t.Fatalf("queued follow-up start failure = %+v", followup)
	}
	if calls.Load() != 1 {
		t.Fatalf("failed follow-up start ran Adapter %d times", calls.Load()-1)
	}
	shutdownHandle(t, h)
	assertNoDurableRunning(t, db)
}

func TestTerminalPersistenceRetriesBusyAndInternal(t *testing.T) {
	for _, test := range []struct {
		name      string
		operation storeOperation
		kind      storage.ErrorKind
		failed    bool
	}{
		{"completed busy", opGlobalComplete, storage.ErrorDatabaseBusy, false},
		{"completed internal", opGlobalComplete, storage.ErrorInvalidState, false},
		{"failed busy", opGlobalFailed, storage.ErrorDatabaseBusy, true},
		{"failed internal", opGlobalFailed, storage.ErrorInvalidState, true},
	} {
		t.Run(test.name, func(t *testing.T) {
			db := openCoordinatorTestDB(t)
			store := newScriptedStore(db)
			store.inject(test.operation, injectedError(test.kind))
			adapter := newFakeAdapter("codex")
			if test.failed {
				adapter.runFn = func(context.Context, source.RunContext) error {
					return source.NewAdapterErrorWithCode("SOURCE_TEST_FAILED", "scripted adapter error", nil)
				}
			}
			h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
			waitReady(t, h)
			waitIdle(t, db)
			if store.count(test.operation) < 2 {
				t.Fatalf("terminal write was not retried: %+v", store.callsSnapshot())
			}
			starts := store.callsFor(opStart)
			if len(starts) != 1 {
				t.Fatalf("Startup calls = %+v", starts)
			}
			_, run := readTarget(t, db, starts[0].scanID)
			wantState, wantCode := domain.ScanRunCompleted, ""
			if test.failed {
				wantState, wantCode = domain.ScanRunFailed, "SOURCE_RUN_FAILED"
			}
			if run.State != wantState || (run.ErrorCode == nil) != (wantCode == "") || (run.ErrorCode != nil && *run.ErrorCode != wantCode) {
				t.Fatalf("terminal row = %+v, want state=%s code=%q", run, wantState, wantCode)
			}
			shutdownHandle(t, h)
		})
	}
	for i, want := range []time.Duration{25, 50, 100, 200, 400, 800, 1000, 1000} {
		if got := retryDelay(uint32(i)); got != want*time.Millisecond {
			t.Errorf("retryDelay(%d)=%s, want %s", i, got, want*time.Millisecond)
		}
	}
}

func TestScheduledTickDoesNotReplayMissedIntervals(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	reserveGate := store.blockNext(opReserve)
	adapter := newFakeAdapter("codex")
	release := make(chan struct{})
	started := make(chan struct{}, 2)
	adapter.runFn = func(context.Context, source.RunContext) error {
		started <- struct{}{}
		<-release
		return nil
	}
	h := startTestHandle(t, db, makeRegistry(t, adapter), store, 100*time.Millisecond)
	<-started
	select {
	case <-reserveGate.entered:
	case <-time.After(3 * time.Second):
		t.Fatal("scheduled request did not reach Reserve")
	}
	time.Sleep(350 * time.Millisecond)
	close(reserveGate.release)
	waitUntil(t, "scheduled follow-up commit", func() bool {
		snapshot, err := db.ScanStatusSnapshot(context.Background(), nil)
		return err == nil && snapshot.AppState.Scan.FollowupState != nil && *snapshot.AppState.Scan.FollowupState == domain.FollowupQueued
	})
	readsAfterTick := store.count(opSnapshot)
	time.Sleep(25 * time.Millisecond)
	if got := store.count(opSnapshot); got != readsAfterTick {
		t.Fatalf("missed scheduled ticks replayed immediately: snapshots %d -> %d", readsAfterTick, got)
	}
	close(release)
	waitIdle(t, db)
	shutdownHandle(t, h)
}

func TestUnavailableAndNotInstalledSkip(t *testing.T) {
	db := openCoordinatorTestDB(t)
	unavailable := newFakeAdapter("alpha")
	unavailable.availabilityFn = func(context.Context) (source.Availability, error) { return source.Unavailable(""), nil }
	notInstalled := newFakeAdapter("beta")
	notInstalled.availabilityFn = func(context.Context) (source.Availability, error) { return source.NotInstalled(), nil }
	h := startTestHandle(t, db, makeRegistry(t, notInstalled, unavailable), db, time.Hour)
	waitReady(t, h)
	waitIdle(t, db)
	starts := findRows(t, db, "")
	if len(starts) != 1 || starts[0].State != domain.ScanRunCompleted {
		t.Fatalf("global Scan after skips = %+v", starts)
	}
	snapshot, run := readTarget(t, db, starts[0].ScanID)
	if run.State != domain.ScanRunCompleted || len(snapshot.Sources) != 2 || snapshot.Sources[0].Source != "alpha" || snapshot.Sources[0].State != domain.SourceScanSkipped || snapshot.Sources[1].Source != "beta" || snapshot.Sources[1].State != domain.SourceScanSkipped {
		t.Fatalf("skip child states = %+v", snapshot.Sources)
	}
	reports := h.SourceReports()
	if len(reports) != 2 || reports[0].State != source.RunSkipped || reports[0].Detail != "" || reports[1].State != source.RunSkipped || reports[1].Detail != "source is not installed" {
		t.Fatalf("skip reports = %+v", reports)
	}
	shutdownHandle(t, h)
}

func TestAvailabilityErrorFailsChildAndContinues(t *testing.T) {
	db := openCoordinatorTestDB(t)
	first := newFakeAdapter("alpha")
	first.availabilityFn = func(context.Context) (source.Availability, error) {
		return source.Availability{}, source.NewAdapterErrorWithCode("AVAILABILITY_FAILED", "probe failed", nil)
	}
	second := newFakeAdapter("beta")
	h := startTestHandle(t, db, makeRegistry(t, second, first), db, time.Hour)
	waitReady(t, h)
	waitIdle(t, db)
	starts := findRows(t, db, "")
	if len(starts) != 1 || starts[0].State != domain.ScanRunFailed || starts[0].ErrorCode == nil || *starts[0].ErrorCode != "SOURCE_RUN_FAILED" {
		t.Fatalf("global Scan after Availability error = %+v", starts)
	}
	snapshot, _ := readTarget(t, db, starts[0].ScanID)
	if len(snapshot.Sources) != 2 || snapshot.Sources[0].State != domain.SourceScanFailed || snapshot.Sources[0].ErrorCode == nil || *snapshot.Sources[0].ErrorCode != "AVAILABILITY_FAILED" || snapshot.Sources[1].State != domain.SourceScanCompleted {
		t.Fatalf("Availability error isolation states = %+v", snapshot.Sources)
	}
	reports := h.SourceReports()
	if len(reports) != 2 || reports[0].State != source.RunFailed || reports[0].ErrorCode == nil || *reports[0].ErrorCode != "AVAILABILITY_FAILED" || reports[0].Detail != "probe failed" || reports[1].State != source.RunCompleted {
		t.Fatalf("Availability error reports = %+v", reports)
	}
	if second.runNum.Load() != 1 {
		t.Fatalf("next Source was not run after Availability error: %d", second.runNum.Load())
	}
	shutdownHandle(t, h)
}

func TestSourceFailurePersistenceFailureDoesNotChangeReport(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	store.inject(opChildFailed, injectedError(storage.ErrorInvalidState))
	first := newFakeAdapter("alpha")
	first.runFn = func(context.Context, source.RunContext) error {
		return source.NewAdapterErrorWithCode("ADAPTER_FAILED", "first Source failed", nil)
	}
	second := newFakeAdapter("beta")
	h := startTestHandle(t, db, makeRegistry(t, first, second), store, time.Hour)
	waitReady(t, h)
	waitIdle(t, db)
	if second.runNum.Load() != 1 {
		t.Fatalf("next Source did not run after child failure persistence error: %d", second.runNum.Load())
	}
	reports := h.SourceReports()
	if len(reports) != 2 || reports[0].State != source.RunFailed || reports[0].ErrorCode == nil || *reports[0].ErrorCode != "ADAPTER_FAILED" || reports[0].Detail != "first Source failed" || reports[1].State != source.RunCompleted {
		t.Fatalf("child failure persistence changed report outcomes: %+v", reports)
	}
	starts := store.callsFor(opStart)
	_, global := readTarget(t, db, starts[0].scanID)
	if global.State != domain.ScanRunFailed || global.ErrorCode == nil || *global.ErrorCode != "SOURCE_RUN_FAILED" || store.count(opChildFailed) != 1 {
		t.Fatalf("child persistence failure was not aggregated: global=%+v calls=%+v", global, store.callsFor(opChildFailed))
	}
	shutdownHandle(t, h)
}

func TestAvailabilityZeroValueIsInvalid(t *testing.T) {
	db := openCoordinatorTestDB(t)
	adapter := newFakeAdapter("codex")
	adapter.availabilityFn = func(context.Context) (source.Availability, error) { return source.Availability{}, nil }
	h := startTestHandle(t, db, makeRegistry(t, adapter), db, time.Hour)
	waitReady(t, h)
	waitIdle(t, db)
	if adapter.runNum.Load() != 0 {
		t.Fatalf("zero Availability entered RunScan %d times", adapter.runNum.Load())
	}
	starts := findRows(t, db, "")
	if len(starts) != 1 || starts[0].State != domain.ScanRunFailed {
		t.Fatalf("zero Availability global state = %+v", starts)
	}
	snapshot, _ := readTarget(t, db, starts[0].ScanID)
	if snapshot.Sources[0].State != domain.SourceScanFailed || snapshot.Sources[0].ErrorCode == nil || *snapshot.Sources[0].ErrorCode != "SOURCE_RUN_FAILED" {
		t.Fatalf("zero Availability child = %+v", snapshot.Sources)
	}
	shutdownHandle(t, h)
}

func TestCoordinatorRejectsUnknownAvailabilityKind(t *testing.T) {
	if got := classifyAvailabilityKind(source.AvailabilityInvalid); got != availabilityDecisionFail {
		t.Fatalf("Invalid decision = %d", got)
	}
	if got := classifyAvailabilityKind(source.AvailabilityKind(255)); got != availabilityDecisionFail {
		t.Fatalf("unknown decision = %d", got)
	}
}

func TestChildStartPersistenceFailureContinuesNextSource(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	store.inject(opChildStart, injectedError(storage.ErrorInvalidState))
	first, second := newFakeAdapter("alpha"), newFakeAdapter("beta")
	h := startTestHandle(t, db, makeRegistry(t, first, second), store, time.Hour)
	waitReady(t, h)
	waitIdle(t, db)
	if first.runNum.Load() != 0 || second.runNum.Load() != 1 {
		t.Fatalf("Source runs after child start failure = %d, %d", first.runNum.Load(), second.runNum.Load())
	}
	starts := store.callsFor(opStart)
	_, run := readTarget(t, db, starts[0].scanID)
	if run.State != domain.ScanRunFailed || run.ErrorCode == nil || *run.ErrorCode != "SOURCE_RUN_FAILED" {
		t.Fatalf("global row = %+v", run)
	}
	snapshot, _ := readTarget(t, db, starts[0].scanID)
	if snapshot.Sources[0].State != domain.SourceScanFailed || snapshot.Sources[0].ErrorCode == nil || *snapshot.Sources[0].ErrorCode != "SOURCE_RUN_FAILED" || snapshot.Sources[1].State != domain.SourceScanCompleted {
		t.Fatalf("child state after start failure = %+v", snapshot.Sources)
	}
	reports := h.SourceReports()
	if len(reports) != 2 || reports[0].State != source.RunFailed || reports[0].ErrorCode == nil || *reports[0].ErrorCode != "SOURCE_RUN_FAILED" || reports[1].State != source.RunCompleted {
		t.Fatalf("reports after child start failure = %+v", reports)
	}
	shutdownHandle(t, h)
}

func TestSkipAndCompletePersistenceFailureForceGlobalFailure(t *testing.T) {
	for _, test := range []struct {
		name      string
		operation storeOperation
		firstSkip bool
	}{
		{"skip", opChildSkip, true},
		{"complete", opChildComplete, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			db := openCoordinatorTestDB(t)
			store := newScriptedStore(db)
			store.inject(test.operation, injectedError(storage.ErrorInvalidState))
			first, second := newFakeAdapter("alpha"), newFakeAdapter("beta")
			if test.firstSkip {
				first.availabilityFn = func(context.Context) (source.Availability, error) { return source.Unavailable("maintenance"), nil }
			}
			h := startTestHandle(t, db, makeRegistry(t, first, second), store, time.Hour)
			waitReady(t, h)
			waitIdle(t, db)
			starts := store.callsFor(opStart)
			_, global := readTarget(t, db, starts[0].scanID)
			if global.State != domain.ScanRunFailed || global.ErrorCode == nil || *global.ErrorCode != "SOURCE_RUN_FAILED" {
				t.Fatalf("global terminal after child persistence failure = %+v", global)
			}
			snapshot, _ := readTarget(t, db, starts[0].scanID)
			if len(snapshot.Sources) != 2 {
				t.Fatalf("Source statuses = %+v", snapshot.Sources)
			}
			reports := h.SourceReports()
			if len(reports) != 2 {
				t.Fatalf("reports = %+v", reports)
			}
			if test.firstSkip {
				if snapshot.Sources[0].State != domain.SourceScanFailed || reports[0].State != source.RunSkipped || reports[0].Detail != "maintenance" || reports[1].State != source.RunCompleted {
					t.Fatalf("Skip persistence failure changed outcome: children=%+v reports=%+v", snapshot.Sources, reports)
				}
			} else if snapshot.Sources[0].State != domain.SourceScanFailed || snapshot.Sources[1].State != domain.SourceScanCompleted || reports[0].State != source.RunCompleted || reports[1].State != source.RunCompleted {
				t.Fatalf("Complete persistence failure changed outcome: children=%+v reports=%+v", snapshot.Sources, reports)
			}
			shutdownHandle(t, h)
		})
	}
}

func TestAdapterFailureDoesNotRollbackOtherSource(t *testing.T) {
	db := openCoordinatorTestDB(t)
	first := newFakeAdapter("alpha")
	first.runFn = func(_ context.Context, run source.RunContext) error { return writeCanonicalData(run, "event-alpha") }
	second := newFakeAdapter("beta")
	second.runFn = func(context.Context, source.RunContext) error {
		return source.NewAdapterErrorWithCode("ADAPTER_FAILED", "second Source failed", nil)
	}
	h := startTestHandle(t, db, makeRegistry(t, first, second), db, time.Hour)
	waitReady(t, h)
	waitIdle(t, db)
	starts := findRows(t, db, "")
	if len(starts) != 1 || starts[0].State != domain.ScanRunFailed || starts[0].ErrorCode == nil || *starts[0].ErrorCode != "SOURCE_RUN_FAILED" {
		t.Fatalf("global terminal = %+v", starts)
	}
	snapshot, _ := readTarget(t, db, starts[0].ScanID)
	if snapshot.Sources[0].State != domain.SourceScanCompleted || snapshot.Sources[1].State != domain.SourceScanFailed || snapshot.Sources[1].ErrorCode == nil || *snapshot.Sources[1].ErrorCode != "ADAPTER_FAILED" {
		t.Fatalf("child outcomes = %+v", snapshot.Sources)
	}
	revision := db.CurrentRevision()
	if revision.DataRevision != 1 || revision.StatusRevision != 6 {
		t.Fatalf("revisions after isolated Source commit = %+v, want data=1 status=6", revision)
	}
	var threads, events int
	if err := db.Read(context.Background(), func(conn *sql.Conn) error {
		if err := conn.QueryRowContext(context.Background(), "SELECT count(*) FROM threads WHERE source='alpha'").Scan(&threads); err != nil {
			return err
		}
		return conn.QueryRowContext(context.Background(), "SELECT count(*) FROM usage_events WHERE source='alpha'").Scan(&events)
	}); err != nil {
		t.Fatal(err)
	}
	if threads != 1 || events != 1 {
		t.Fatalf("first Source committed data was rolled back: threads=%d events=%d", threads, events)
	}
	shutdownHandle(t, h)
}

func TestSourceReportsAreSnapshotOnly(t *testing.T) {
	db := openCoordinatorTestDB(t)
	adapter := newFakeAdapter("codex")
	adapter.runFn = func(context.Context, source.RunContext) error {
		return source.NewAdapterErrorWithCode("SOURCE_TEST_FAILED", "diagnostic", nil)
	}
	h := startTestHandle(t, db, makeRegistry(t, adapter), db, time.Hour)
	waitReady(t, h)
	waitIdle(t, db)
	before := h.SourceReports()
	if len(before) != 1 || before[0].ErrorCode == nil {
		t.Fatalf("reports = %+v", before)
	}
	*before[0].ErrorCode = "MUTATED"
	before[0].Detail = "mutated"
	before[0].Source = "changed"
	before[0] = source.RunReport{}
	after := h.SourceReports()
	if len(after) != 1 || after[0].ErrorCode == nil || *after[0].ErrorCode != "SOURCE_TEST_FAILED" || after[0].Detail != "diagnostic" || after[0].Source != "codex" {
		t.Fatalf("caller mutated internal report snapshot: %+v", after)
	}
	shutdownHandle(t, h)
}

func TestSourceReportsReplacedAfterCancelledRun(t *testing.T) {
	db := openCoordinatorTestDB(t)
	adapter := newFakeAdapter("codex")
	var runs atomic.Int32
	secondStarted := make(chan struct{}, 1)
	adapter.runFn = func(ctx context.Context, _ source.RunContext) error {
		if runs.Add(1) == 1 {
			return nil
		}
		secondStarted <- struct{}{}
		<-ctx.Done()
		return nil
	}
	store := newScriptedStore(db)
	h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
	waitReady(t, h)
	waitIdle(t, db)
	firstReports := h.SourceReports()
	if len(firstReports) != 1 || firstReports[0].State != source.RunCompleted {
		t.Fatalf("first run reports = %+v", firstReports)
	}
	globalGate := store.blockNext(opGlobalFailed)
	disposition, err := h.Request(context.Background(), domain.ScanTriggerManual)
	if err != nil || disposition.Kind != RequestStarted {
		t.Fatalf("second run Request = %+v, %v", disposition, err)
	}
	<-secondStarted
	shutdownResult := make(chan error, 1)
	go func() { shutdownResult <- h.Shutdown(context.Background()) }()
	select {
	case <-globalGate.entered:
	case <-time.After(3 * time.Second):
		t.Fatal("shutdown did not reach terminal write")
	}
	waitUntil(t, "second Worker report publication", func() bool {
		reports := h.SourceReports()
		return len(reports) == 1 && reports[0].ScanID == disposition.ScanID && reports[0].State == source.RunFailed
	})
	close(globalGate.release)
	if err := <-shutdownResult; err != nil {
		t.Fatal(err)
	}
	reports := h.SourceReports()
	if len(reports) != 1 || reports[0].ScanID != disposition.ScanID || reports[0].ErrorCode == nil || *reports[0].ErrorCode != "SCAN_CANCELLED" {
		t.Fatalf("reports retained old Worker snapshot: %+v", reports)
	}
}

func TestRunScanCancellationRustPrecedence(t *testing.T) {
	for _, test := range []struct {
		name       string
		adapterErr error
		childCode  string
	}{
		{"adapter error wins", source.NewAdapterErrorWithCode("OPERATION_CANCELLED", "adapter checkpoint", nil), "OPERATION_CANCELLED"},
		{"nil error observes cancellation", nil, "SCAN_CANCELLED"},
	} {
		t.Run(test.name, func(t *testing.T) {
			db := openCoordinatorTestDB(t)
			store := newScriptedStore(db)
			globalGate := store.blockNext(opGlobalFailed)
			adapter := newFakeAdapter("codex")
			runStarted := make(chan context.Context, 1)
			adapter.runFn = func(ctx context.Context, _ source.RunContext) error {
				runStarted <- ctx
				<-ctx.Done()
				return test.adapterErr
			}
			h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
			waitReady(t, h)
			workerCtx := <-runStarted
			shutdownResult := make(chan error, 1)
			go func() { shutdownResult <- h.Shutdown(context.Background()) }()
			select {
			case <-globalGate.entered:
			case <-time.After(3 * time.Second):
				t.Fatal("Shutdown did not reach its global terminal attempt")
			}
			if workerCtx.Err() == nil {
				t.Fatal("Shutdown did not cancel RunScan context")
			}
			waitUntil(t, "child cancellation transition", func() bool {
				for _, call := range store.callsFor(opChildFailed) {
					if call.errorCode == test.childCode {
						return true
					}
				}
				return false
			})
			waitUntil(t, "worker report", func() bool { return len(h.SourceReports()) == 1 })
			close(globalGate.release)
			if err := <-shutdownResult; err != nil {
				t.Fatal(err)
			}
			starts := store.callsFor(opStart)
			_, global := readTarget(t, db, starts[0].scanID)
			if global.State != domain.ScanRunFailed || global.ErrorCode == nil || *global.ErrorCode != "SCAN_CANCELLED" {
				t.Fatalf("Shutdown global result = %+v", global)
			}
			snapshot, _ := readTarget(t, db, starts[0].scanID)
			if snapshot.Sources[0].State != domain.SourceScanFailed || snapshot.Sources[0].ErrorCode == nil || *snapshot.Sources[0].ErrorCode != test.childCode {
				t.Fatalf("child Rust precedence result = %+v", snapshot.Sources)
			}
			if snapshot.AppState.Scan.StatusRevision != 4 {
				t.Fatalf("status_revision = %d, want start=1 child-start=2 child-fail=3 global-fail=4", snapshot.AppState.Scan.StatusRevision)
			}
			childCalls := store.callsFor(opChildFailed)
			if len(childCalls) != 1 || childCalls[0].errorCode != test.childCode || childCalls[0].ctxCanceled {
				t.Fatalf("child persistence context/code = %+v", childCalls)
			}
			report := h.SourceReports()[0]
			if report.State != source.RunFailed || report.ErrorCode == nil || *report.ErrorCode != test.childCode {
				t.Fatalf("RunReport precedence = %+v", report)
			}
		})
	}
}

func TestCancellationDuringAvailabilityMatchesRust(t *testing.T) {
	for _, test := range []struct {
		name            string
		availability    source.Availability
		availabilityErr error
		childState      domain.SourceScanState
		childCode       string
		wantRun         bool
		wantStatusRev   int64
	}{
		{"available continues into RunScan", source.Available(), nil, domain.SourceScanFailed, "SCAN_CANCELLED", true, 4},
		{"unavailable persists skip", source.Unavailable("offline"), nil, domain.SourceScanSkipped, "", false, 3},
		{"availability error keeps adapter code", source.Availability{}, source.NewAdapterErrorWithCode("AVAILABILITY_FAILED", "probe failed", nil), domain.SourceScanFailed, "AVAILABILITY_FAILED", false, 3},
	} {
		t.Run(test.name, func(t *testing.T) {
			db := openCoordinatorTestDB(t)
			store := newScriptedStore(db)
			globalGate := store.blockNext(opGlobalFailed)
			first, second := newFakeAdapter("alpha"), newFakeAdapter("beta")
			availabilityEntered := make(chan error, 1)
			availabilityRelease := make(chan struct{})
			first.availabilityFn = func(ctx context.Context) (source.Availability, error) {
				availabilityEntered <- ctx.Err()
				<-availabilityRelease
				return test.availability, test.availabilityErr
			}
			runContextErr := make(chan error, 1)
			first.runFn = func(ctx context.Context, _ source.RunContext) error {
				runContextErr <- ctx.Err()
				return nil
			}
			h := startTestHandle(t, db, makeRegistry(t, first, second), store, time.Hour)
			waitReady(t, h)
			if err := <-availabilityEntered; err != nil {
				t.Fatalf("Availability received canceled context: %v", err)
			}
			shutdownResult := make(chan error, 1)
			go func() { shutdownResult <- h.Shutdown(context.Background()) }()
			select {
			case <-globalGate.entered:
			case <-time.After(3 * time.Second):
				t.Fatal("Shutdown did not reach global terminal attempt")
			}
			if h.Availability() != CoordinatorShuttingDown {
				t.Fatalf("Shutdown did not publish ownership: %v", h.Availability())
			}
			close(availabilityRelease)
			waitUntil(t, "both Source reports", func() bool { return len(h.SourceReports()) == 2 })
			if test.wantRun {
				if err := <-runContextErr; !errors.Is(err, context.Canceled) {
					t.Fatalf("RunScan context error = %v, want canceled", err)
				}
				if first.runNum.Load() != 1 {
					t.Fatalf("Available Source skipped RunScan after Shutdown: %d", first.runNum.Load())
				}
			} else if first.runNum.Load() != 0 {
				t.Fatalf("RunScan called for non-available Source: %d", first.runNum.Load())
			}
			close(globalGate.release)
			if err := <-shutdownResult; err != nil {
				t.Fatal(err)
			}
			starts := store.callsFor(opStart)
			snapshot, global := readTarget(t, db, starts[0].scanID)
			if global.State != domain.ScanRunFailed || global.ErrorCode == nil || *global.ErrorCode != "SCAN_CANCELLED" || snapshot.AppState.Scan.StatusRevision != test.wantStatusRev {
				t.Fatalf("global terminal/revision = %+v revision=%d, want %d", global, snapshot.AppState.Scan.StatusRevision, test.wantStatusRev)
			}
			if snapshot.Sources[0].State != test.childState {
				t.Fatalf("first child = %+v, want %s", snapshot.Sources[0], test.childState)
			}
			if test.childCode != "" && (snapshot.Sources[0].ErrorCode == nil || *snapshot.Sources[0].ErrorCode != test.childCode) {
				t.Fatalf("first child code = %+v, want %s", snapshot.Sources[0].ErrorCode, test.childCode)
			}
			if snapshot.Sources[1].State != domain.SourceScanFailed || snapshot.Sources[1].ErrorCode == nil || *snapshot.Sources[1].ErrorCode != "SCAN_CANCELLED" {
				t.Fatalf("next queued child was not globally cancelled: %+v", snapshot.Sources[1])
			}
			if store.count(opChildStart) != boolCount(test.wantRun) || store.count(opChildSkip) != boolCount(test.availability.Kind() == source.AvailabilityUnavailable) {
				t.Fatalf("unexpected child transition sequence: %+v", store.callsSnapshot())
			}
			for _, call := range store.callsSnapshot() {
				if (call.operation == opChildStart || call.operation == opChildSkip || call.operation == opChildFailed) && call.ctxCanceled {
					t.Fatalf("child persistence got canceled context: %+v", call)
				}
			}
			reports := h.SourceReports()
			if reports[1].Source != "beta" || reports[1].ErrorCode == nil || *reports[1].ErrorCode != "SCAN_CANCELLED" || reports[1].Detail != "source run cancelled before execution" {
				t.Fatalf("next Source loop-entry report = %+v", reports[1])
			}
		})
	}
}

func boolCount(value bool) int {
	if value {
		return 1
	}
	return 0
}

func TestShutdownDuringRecoveryPreventsNewScanStart(t *testing.T) {
	for _, queuedFollowup := range []bool{false, true} {
		name := "startup"
		if queuedFollowup {
			name = "queued follow-up"
		}
		t.Run(name, func(t *testing.T) {
			db := openCoordinatorTestDB(t)
			adapter := newFakeAdapter("codex")
			registry := makeRegistry(t, adapter)
			if queuedFollowup {
				if _, err := db.MarkScanStartedWithSources(context.Background(), domain.ScanStartEvent{ScanID: "seed", Trigger: domain.ScanTriggerManual, RequestedAtMS: 1, StartedAtMS: 1}, nil); err != nil {
					t.Fatal(err)
				}
				if _, err := db.ReserveScanFollowup(context.Background(), domain.ReserveScanFollowupEvent{FollowupScanID: "queued", Trigger: domain.ScanTriggerManual, RequestedAtMS: 2}); err != nil {
					t.Fatal(err)
				}
				if _, err := db.MarkScanCompleted(context.Background(), domain.ScanCompletedEvent{ScanID: "seed", CompletedAtMS: 3}); err != nil {
					t.Fatal(err)
				}
			}
			store := newScriptedStore(db)
			gate := store.blockNext(opSnapshot)
			h := startTestHandle(t, db, registry, store, time.Hour)
			select {
			case <-gate.entered:
			case <-time.After(3 * time.Second):
				t.Fatal("Recovery did not block in Snapshot")
			}
			shutdownResult := make(chan error, 1)
			go func() { shutdownResult <- h.Shutdown(context.Background()) }()
			waitUntil(t, "Shutdown ownership before Recovery returns", func() bool { return h.Availability() == CoordinatorShuttingDown })
			close(gate.release)
			select {
			case err := <-shutdownResult:
				if err != nil {
					t.Fatal(err)
				}
			case <-time.After(3 * time.Second):
				t.Fatal("Shutdown did not finish after Recovery returned")
			}
			if h.Availability() != CoordinatorStopped {
				t.Fatalf("Availability after Shutdown = %v", h.Availability())
			}
			if store.count(opStart) != 0 || store.count(opFollowupStart) != 0 {
				t.Fatalf("Recovery started work after Shutdown ownership: %+v", store.callsSnapshot())
			}
			if queuedFollowup {
				_, followup := readTarget(t, db, "queued")
				if followup.State != domain.ScanRunStartFailed || followup.ErrorCode == nil || *followup.ErrorCode != "SCANNER_UNAVAILABLE" {
					t.Fatalf("queued follow-up shutdown cleanup = %+v", followup)
				}
			}
			assertNoDurableRunning(t, db)
		})
	}
}

func TestStartCommitRaceKeepsRustParity(t *testing.T) {
	t.Run("Recovery Startup", func(t *testing.T) {
		db := openCoordinatorTestDB(t)
		store := newScriptedStore(db)
		gate := store.blockNext(opStart)
		adapter := newFakeAdapter("codex")
		adapter.runFn = func(ctx context.Context, _ source.RunContext) error {
			<-ctx.Done()
			return nil
		}
		h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
		select {
		case <-gate.entered:
		case <-time.After(3 * time.Second):
			t.Fatal("Recovery did not enter Startup commit")
		}
		shutdownResult := make(chan error, 1)
		go func() { shutdownResult <- h.Shutdown(context.Background()) }()
		waitUntil(t, "Shutdown ownership during Startup commit", func() bool { return h.Availability() == CoordinatorShuttingDown })
		close(gate.release)
		if err := <-shutdownResult; err != nil {
			t.Fatal(err)
		}
		starts := store.callsFor(opStart)
		if len(starts) != 1 {
			t.Fatalf("Recovery start commit calls = %+v", starts)
		}
		waitUntil(t, "Recovery worker report after committed start", func() bool {
			reports := h.SourceReports()
			return len(reports) == 1 && reports[0].ScanID == starts[0].scanID
		})
		report := h.SourceReports()[0]
		if report.State != source.RunFailed || report.ErrorCode == nil || *report.ErrorCode != "SCAN_CANCELLED" {
			t.Fatalf("Recovery start-race report = %+v", report)
		}
		_, run := readTarget(t, db, starts[0].scanID)
		if run.State != domain.ScanRunFailed || run.ErrorCode == nil || *run.ErrorCode != "SCAN_CANCELLED" {
			t.Fatalf("Recovery commit-race terminal = %+v", run)
		}
		assertNoDurableRunning(t, db)
	})

	t.Run("Request direct start", func(t *testing.T) {
		db := openCoordinatorTestDB(t)
		store := newScriptedStore(db)
		adapter := newFakeAdapter("codex")
		var runs atomic.Int32
		initialStarted := make(chan struct{}, 1)
		initialRelease := make(chan struct{})
		adapter.runFn = func(ctx context.Context, _ source.RunContext) error {
			if runs.Add(1) == 1 {
				initialStarted <- struct{}{}
				select {
				case <-initialRelease:
					return nil
				case <-ctx.Done():
					return nil
				}
			}
			<-ctx.Done()
			return nil
		}
		h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
		<-initialStarted
		waitReady(t, h)
		close(initialRelease)
		waitIdle(t, db)
		gate := store.blockNext(opStart)
		requestReply := make(chan requestResult, 1)
		go func() {
			disposition, err := h.Request(context.Background(), domain.ScanTriggerManual)
			requestReply <- requestResult{disposition: disposition, err: err}
		}()
		select {
		case <-gate.entered:
		case <-time.After(3 * time.Second):
			t.Fatal("Request did not enter direct-start commit")
		}
		shutdownResult := make(chan error, 1)
		go func() { shutdownResult <- h.Shutdown(context.Background()) }()
		waitUntil(t, "Shutdown ownership during Request commit", func() bool { return h.Availability() == CoordinatorShuttingDown })
		close(gate.release)
		result := <-requestReply
		if result.err != nil || result.disposition.Kind != RequestStarted {
			t.Fatalf("Request commit race result = %+v, %v", result.disposition, result.err)
		}
		if err := <-shutdownResult; err != nil {
			t.Fatal(err)
		}
		waitUntil(t, "Request worker report after committed start", func() bool {
			reports := h.SourceReports()
			return len(reports) == 1 && reports[0].ScanID == result.disposition.ScanID
		})
		report := h.SourceReports()[0]
		if report.State != source.RunFailed || report.ErrorCode == nil || *report.ErrorCode != "SCAN_CANCELLED" {
			t.Fatalf("Request start-race report = %+v", report)
		}
		_, run := readTarget(t, db, result.disposition.ScanID)
		if run.State != domain.ScanRunFailed || run.ErrorCode == nil || *run.ErrorCode != "SCAN_CANCELLED" {
			t.Fatalf("Request commit-race terminal = %+v", run)
		}
		assertNoDurableRunning(t, db)
	})

	t.Run("tryStartQueued follow-up", func(t *testing.T) {
		db := openCoordinatorTestDB(t)
		store := newScriptedStore(db)
		adapter := newFakeAdapter("codex")
		var runs atomic.Int32
		initialStarted := make(chan struct{}, 1)
		initialRelease := make(chan struct{})
		adapter.runFn = func(ctx context.Context, run source.RunContext) error {
			if runs.Add(1) == 1 {
				initialStarted <- struct{}{}
				select {
				case <-initialRelease:
					return nil
				case <-ctx.Done():
					return nil
				}
			}
			<-ctx.Done()
			return nil
		}
		h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
		<-initialStarted
		waitReady(t, h)
		disposition, err := h.Request(context.Background(), domain.ScanTriggerManual)
		if err != nil || disposition.Kind != RequestCoalesced {
			t.Fatalf("queued follow-up Request = %+v, %v", disposition, err)
		}
		gate := store.blockNext(opFollowupStart)
		close(initialRelease)
		select {
		case <-gate.entered:
		case <-time.After(3 * time.Second):
			t.Fatal("tryStartQueued did not enter follow-up commit")
		}
		shutdownResult := make(chan error, 1)
		go func() { shutdownResult <- h.Shutdown(context.Background()) }()
		waitUntil(t, "Shutdown ownership during follow-up commit", func() bool { return h.Availability() == CoordinatorShuttingDown })
		close(gate.release)
		if err := <-shutdownResult; err != nil {
			t.Fatal(err)
		}
		waitUntil(t, "follow-up worker report after committed start", func() bool {
			reports := h.SourceReports()
			return len(reports) == 1 && reports[0].ScanID == disposition.ScanID
		})
		report := h.SourceReports()[0]
		if report.State != source.RunFailed || report.ErrorCode == nil || *report.ErrorCode != "SCAN_CANCELLED" {
			t.Fatalf("follow-up start-race report = %+v", report)
		}
		_, run := readTarget(t, db, disposition.ScanID)
		if run.State != domain.ScanRunFailed || run.ErrorCode == nil || *run.ErrorCode != "SCAN_CANCELLED" {
			t.Fatalf("follow-up commit-race terminal = %+v", run)
		}
		assertNoDurableRunning(t, db)
	})
}

func TestShutdownDurableBeforeAck(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	adapter := newFakeAdapter("codex")
	runStarted := make(chan struct{}, 1)
	adapter.runFn = func(ctx context.Context, _ source.RunContext) error {
		runStarted <- struct{}{}
		<-ctx.Done()
		return nil
	}
	h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
	<-runStarted
	waitReady(t, h)
	disposition, err := h.Request(context.Background(), domain.ScanTriggerManual)
	if err != nil || disposition.Kind != RequestCoalesced {
		t.Fatalf("queued follow-up Request = %+v, %v", disposition, err)
	}
	gate := store.blockNext(opGlobalFailed)
	shutdownResult := make(chan error, 1)
	go func() { shutdownResult <- h.Shutdown(context.Background()) }()
	select {
	case <-gate.entered:
	case <-time.After(3 * time.Second):
		t.Fatal("Shutdown did not enter durable terminal write")
	}
	select {
	case err := <-shutdownResult:
		t.Fatalf("Shutdown acknowledged before durable cleanup: %v", err)
	case <-time.After(25 * time.Millisecond):
	}
	snapshot, err := db.ScanStatusSnapshot(context.Background(), nil)
	if err != nil || snapshot.AppState.Scan.ActiveScanID == nil || snapshot.AppState.Scan.FollowupState == nil || *snapshot.AppState.Scan.FollowupState != domain.FollowupQueued {
		t.Fatalf("durable state changed before terminal commit: %+v, %v", snapshot.AppState.Scan, err)
	}
	close(gate.release)
	if err := <-shutdownResult; err != nil {
		t.Fatal(err)
	}
	assertNoDurableRunning(t, db)
	_, followup := readTarget(t, db, disposition.ScanID)
	if followup.State != domain.ScanRunStartFailed || followup.ErrorCode == nil || *followup.ErrorCode != "SCANNER_UNAVAILABLE" {
		t.Fatalf("shutdown follow-up terminal = %+v", followup)
	}
}

func TestShutdownCallerTimeoutDoesNotAbortCleanup(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	adapter := newFakeAdapter("codex")
	runStarted := make(chan struct{}, 1)
	adapter.runFn = func(ctx context.Context, _ source.RunContext) error {
		runStarted <- struct{}{}
		<-ctx.Done()
		return nil
	}
	h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
	<-runStarted
	waitReady(t, h)
	gate := store.blockNext(opGlobalFailed)
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	shutdownResult := make(chan error, 1)
	go func() { shutdownResult <- h.Shutdown(ctx) }()
	select {
	case <-gate.entered:
	case <-time.After(3 * time.Second):
		t.Fatal("Shutdown did not enter durable terminal write")
	}
	if err := <-shutdownResult; !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("timed-out Shutdown result = %v", err)
	}
	if h.Availability() != CoordinatorShuttingDown {
		t.Fatalf("caller timeout changed shutdown ownership: %v", h.Availability())
	}
	close(gate.release)
	waitUntil(t, "background Shutdown cleanup", func() bool { return h.Availability() == CoordinatorStopped })
	assertNoDurableRunning(t, db)
}

func TestConcurrentShutdownSingleOwner(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	adapter := newFakeAdapter("codex")
	runStarted := make(chan struct{}, 1)
	adapter.runFn = func(ctx context.Context, _ source.RunContext) error {
		runStarted <- struct{}{}
		<-ctx.Done()
		return nil
	}
	h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
	<-runStarted
	waitReady(t, h)
	gate := store.blockNext(opGlobalFailed)
	start := make(chan struct{})
	results := make(chan error, 2)
	for i := 0; i < 2; i++ {
		go func() {
			<-start
			results <- h.Shutdown(context.Background())
		}()
	}
	close(start)
	select {
	case <-gate.entered:
	case <-time.After(3 * time.Second):
		t.Fatal("neither Shutdown caller reached durable cleanup")
	}
	select {
	case err := <-results:
		if !errors.Is(err, ErrCoordinatorUnavailable) {
			t.Fatalf("losing Shutdown caller = %v, want unavailable", err)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("second Shutdown caller did not return")
	}
	close(gate.release)
	if err := <-results; err != nil {
		t.Fatalf("Shutdown owner result = %v", err)
	}
	if h.Availability() != CoordinatorStopped {
		t.Fatalf("Availability after one owner completes = %v", h.Availability())
	}
}

func TestShutdownReentry(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	adapter := newFakeAdapter("codex")
	runStarted := make(chan struct{}, 1)
	adapter.runFn = func(ctx context.Context, _ source.RunContext) error {
		runStarted <- struct{}{}
		<-ctx.Done()
		return nil
	}
	h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
	<-runStarted
	waitReady(t, h)
	gate := store.blockNext(opGlobalFailed)
	firstResult := make(chan error, 1)
	go func() { firstResult <- h.Shutdown(context.Background()) }()
	select {
	case <-gate.entered:
	case <-time.After(3 * time.Second):
		t.Fatal("first Shutdown did not reach cleanup")
	}
	if err := h.Shutdown(context.Background()); !errors.Is(err, ErrCoordinatorUnavailable) {
		t.Fatalf("in-progress Shutdown reentry = %v", err)
	}
	close(gate.release)
	if err := <-firstResult; err != nil {
		t.Fatal(err)
	}
	if err := h.Shutdown(context.Background()); err != nil {
		t.Fatalf("Stopped Shutdown reentry = %v", err)
	}
}

func TestShutdownTerminalPendingOwnership(t *testing.T) {
	for _, local := range []bool{true, false} {
		name := "orphan durable active"
		if local {
			name = "local active worker"
		}
		t.Run(name, func(t *testing.T) {
			db := openCoordinatorTestDB(t)
			const scanID = "pending-owner"
			if _, err := db.MarkScanStartedWithSources(context.Background(), domain.ScanStartEvent{ScanID: scanID, Trigger: domain.ScanTriggerStartup, RequestedAtMS: 1, StartedAtMS: 1}, nil); err != nil {
				t.Fatal(err)
			}
			store := newScriptedStore(db)
			h := &Handle{commands: make(chan command, commandCapacity), loopDone: make(chan struct{})}
			h.availability.Store(uint32(CoordinatorShuttingDown))
			reply := make(chan error, 1)
			loop := coordinatorLoop{db: db, registry: makeRegistry(t), store: store, handle: h, shutdownReply: reply}
			if local {
				loop.activeWorker = &activeWorker{scanID: scanID, cancel: func() {}}
			}
			if loop.driveShutdown() {
				t.Fatal("Shutdown completed before terminal ownership was persisted")
			}
			if local {
				if loop.pendingTerminal == nil || loop.pendingTerminal.scanID != scanID || loop.pendingShutdownActive != nil {
					t.Fatalf("local ownership = terminal:%+v orphan:%v", loop.pendingTerminal, loop.pendingShutdownActive)
				}
			} else if loop.pendingTerminal != nil || loop.pendingShutdownActive == nil || *loop.pendingShutdownActive != scanID {
				t.Fatalf("orphan ownership = terminal:%+v orphan:%v", loop.pendingTerminal, loop.pendingShutdownActive)
			}
			loop.retryPending()
			if loop.driveShutdown() != true {
				t.Fatal("Shutdown did not finish after terminal persistence")
			}
			if loop.pendingTerminal != nil || loop.pendingShutdownActive != nil {
				t.Fatalf("terminal ownership remained after success: %+v / %v", loop.pendingTerminal, loop.pendingShutdownActive)
			}
			if err := <-reply; err != nil {
				t.Fatal(err)
			}
			_, run := readTarget(t, db, scanID)
			if run.State != domain.ScanRunFailed || run.ErrorCode == nil || *run.ErrorCode != "SCAN_CANCELLED" {
				t.Fatalf("terminal row = %+v", run)
			}
		})
	}
}

func TestShutdownSnapshotFailureKeepsLocalPendingTerminal(t *testing.T) {
	db := openCoordinatorTestDB(t)
	store := newScriptedStore(db)
	adapter := newFakeAdapter("codex")
	runStarted := make(chan context.Context, 1)
	release := make(chan struct{})
	adapter.runFn = func(ctx context.Context, _ source.RunContext) error {
		runStarted <- ctx
		<-release
		return nil
	}
	h := startTestHandle(t, db, makeRegistry(t, adapter), store, time.Hour)
	workerCtx := <-runStarted
	waitReady(t, h)
	store.inject(opSnapshot, injectedError(storage.ErrorInvalidState))
	shutdownResult := make(chan error, 1)
	go func() { shutdownResult <- h.Shutdown(context.Background()) }()
	waitUntil(t, "failed Shutdown snapshot", func() bool {
		calls := store.callsFor(opSnapshot)
		return len(calls) >= 2
	})
	if workerCtx.Err() == nil {
		t.Fatal("Shutdown snapshot failure did not cancel local Worker")
	}
	select {
	case err := <-shutdownResult:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("local pending terminal was lost after Snapshot failure")
	}
	if h.Availability() != CoordinatorStopped {
		t.Fatalf("Availability after pending terminal = %v", h.Availability())
	}
	if store.count(opSnapshot) < 3 {
		t.Fatalf("Shutdown did not retry its failed Snapshot: %+v", store.callsFor(opSnapshot))
	}
	assertNoDurableRunning(t, db)
	close(release)
	waitUntil(t, "late local Worker report", func() bool { return len(h.SourceReports()) == 1 })
}

func TestWorkerFinishedReplacesStalePendingTerminal(t *testing.T) {
	db := openCoordinatorTestDB(t)
	const scanID = "late-finished"
	if _, err := db.MarkScanStartedWithSources(context.Background(), domain.ScanStartEvent{ScanID: scanID, Trigger: domain.ScanTriggerManual, RequestedAtMS: 1, StartedAtMS: 1}, nil); err != nil {
		t.Fatal(err)
	}
	store := newScriptedStore(db)
	h := &Handle{commands: make(chan command, commandCapacity), loopDone: make(chan struct{})}
	h.availability.Store(uint32(CoordinatorShuttingDown))
	loop := coordinatorLoop{
		db: db, registry: makeRegistry(t), store: store, handle: h,
		activeWorker:    &activeWorker{scanID: scanID, cancel: func() {}},
		pendingTerminal: &pendingTerminal{scanID: scanID, result: workerResult{}},
	}
	loop.handleWorkerFinished(workerFinishedCommand{scanID: scanID, result: workerResult{failed: true, errorCode: "SOURCE_RUN_FAILED"}})
	if store.count(opGlobalFailed) != 1 || store.count(opGlobalComplete) != 0 {
		t.Fatalf("workerFinished did not replace and persist the current terminal attempt once: %+v", store.callsSnapshot())
	}
	if loop.activeWorker != nil || loop.pendingTerminal != nil {
		t.Fatalf("successful workerFinished left local ownership: active=%+v pending=%+v", loop.activeWorker, loop.pendingTerminal)
	}
	_, run := readTarget(t, db, scanID)
	if run.State != domain.ScanRunFailed || run.ErrorCode == nil || *run.ErrorCode != "SCAN_CANCELLED" {
		t.Fatalf("shutdown worker terminal = %+v", run)
	}
}

func TestShutdownCommandMustInstallReplyBeforeStopped(t *testing.T) {
	for _, workerFinishedFirst := range []bool{false, true} {
		name := "Recovery return"
		if workerFinishedFirst {
			name = "WorkerFinished"
		}
		t.Run(name, func(t *testing.T) {
			db := openCoordinatorTestDB(t)
			store := newScriptedStore(db)
			if workerFinishedFirst {
				if _, err := db.MarkScanStartedWithSources(context.Background(), domain.ScanStartEvent{ScanID: "worker-finished", Trigger: domain.ScanTriggerStartup, RequestedAtMS: 1, StartedAtMS: 1}, nil); err != nil {
					t.Fatal(err)
				}
			}
			h := &Handle{commands: make(chan command, commandCapacity), loopDone: make(chan struct{})}
			h.availability.Store(uint32(CoordinatorShuttingDown))
			loop := coordinatorLoop{db: db, registry: makeRegistry(t), store: store, handle: h}
			if workerFinishedFirst {
				loop.activeWorker = &activeWorker{scanID: "worker-finished", cancel: func() {}}
				if loop.handleCommand(workerFinishedCommand{scanID: "worker-finished", result: workerResult{}}) {
					t.Fatal("WorkerFinished exited before the Shutdown reply was installed")
				}
			} else {
				loop.recover()
				if loop.driveShutdown() {
					t.Fatal("Recovery return exited before the Shutdown reply was installed")
				}
			}
			if h.Availability() != CoordinatorShuttingDown {
				t.Fatalf("Availability before explicit Shutdown command = %v", h.Availability())
			}
			select {
			case <-h.loopDone:
				t.Fatal("loopDone closed before the Shutdown reply was installed")
			default:
			}
			reply := make(chan error, 1)
			if !loop.handleCommand(shutdownCommand{reply: reply}) {
				t.Fatal("Shutdown command did not finish clean durable cleanup")
			}
			if err := <-reply; err != nil {
				t.Fatal(err)
			}
			if h.Availability() != CoordinatorStopped {
				t.Fatalf("Availability after installed reply = %v", h.Availability())
			}
		})
	}
}

func TestRequestLoopDoneDoesNotHang(t *testing.T) {
	h := &Handle{commands: make(chan command, commandCapacity), loopDone: make(chan struct{})}
	h.availability.Store(uint32(CoordinatorReady))
	close(h.loopDone)
	result := make(chan error, 1)
	go func() {
		_, err := h.Request(context.Background(), domain.ScanTriggerManual)
		result <- err
	}()
	select {
	case err := <-result:
		requestErr, ok := err.(RequestError)
		if !ok || requestErr.Kind != RequestErrorShuttingDown {
			t.Fatalf("Request after loopDone = %#v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Request remained blocked after loopDone")
	}
}

func TestShutdownLoopDoneDoesNotHang(t *testing.T) {
	for _, bufferedReply := range []bool{false, true} {
		name := "missing reply"
		if bufferedReply {
			name = "buffered reply wins"
		}
		t.Run(name, func(t *testing.T) {
			h := &Handle{commands: make(chan command, 1), loopDone: make(chan struct{})}
			h.availability.Store(uint32(CoordinatorReady))
			result := make(chan error, 1)
			go func() { result <- h.Shutdown(context.Background()) }()
			var cmd shutdownCommand
			select {
			case value := <-h.commands:
				var ok bool
				cmd, ok = value.(shutdownCommand)
				if !ok {
					t.Fatalf("queued command = %T", value)
				}
			case <-time.After(time.Second):
				t.Fatal("Shutdown did not enqueue its command")
			}
			if bufferedReply {
				cmd.reply <- nil
			}
			close(h.loopDone)
			select {
			case err := <-result:
				if bufferedReply && err != nil {
					t.Fatalf("buffered Shutdown reply lost to loopDone: %v", err)
				}
				if !bufferedReply && !errors.Is(err, ErrCoordinatorUnavailable) {
					t.Fatalf("Shutdown without reply after loopDone = %v", err)
				}
			case <-time.After(time.Second):
				t.Fatal("Shutdown remained blocked after loopDone")
			}
		})
	}
}

func TestLateWorkerFinishedAfterLoopExitDoesNotLeak(t *testing.T) {
	db := openCoordinatorTestDB(t)
	adapter := newFakeAdapter("codex")
	runStarted := make(chan struct{}, 1)
	release := make(chan struct{})
	adapter.runFn = func(context.Context, source.RunContext) error {
		runStarted <- struct{}{}
		<-release
		return nil
	}
	h := startTestHandle(t, db, makeRegistry(t, adapter), db, time.Hour)
	<-runStarted
	waitReady(t, h)
	if err := h.Shutdown(context.Background()); err != nil {
		t.Fatal(err)
	}
	if h.Availability() != CoordinatorStopped {
		t.Fatalf("Shutdown did not stop loop while Adapter remained blocked: %v", h.Availability())
	}
	for len(h.commands) < commandCapacity {
		h.commands <- requestCommand{trigger: domain.ScanTriggerManual, reply: make(chan requestResult, 1)}
	}
	close(release)
	waitUntil(t, "late Worker report", func() bool { return len(h.SourceReports()) == 1 })
	waitUntil(t, "late Worker goroutine exit", func() bool {
		var dump strings.Builder
		if err := pprof.Lookup("goroutine").WriteTo(&dump, 2); err != nil {
			t.Fatalf("read goroutine profile: %v", err)
		}
		return !strings.Contains(dump.String(), "coordinatorLoop).spawnWorker.func1")
	})
	assertNoDurableRunning(t, db)
}

func TestSpec02FakeAdapterEndToEnd(t *testing.T) {
	db := openCoordinatorTestDB(t)
	adapter := newFakeAdapter("codex")
	adapter.runFn = func(_ context.Context, run source.RunContext) error {
		return writeCanonicalData(run, "event-e2e")
	}
	h := startTestHandle(t, db, makeRegistry(t, adapter), db, time.Hour)
	waitReady(t, h)
	waitIdle(t, db)
	runs := findRows(t, db, "")
	if len(runs) != 1 || runs[0].State != domain.ScanRunCompleted || runs[0].RequestKind != domain.ScanRequestDirect || runs[0].Trigger != domain.ScanTriggerStartup {
		t.Fatalf("E2E scan row = %+v", runs)
	}
	snapshot, _ := readTarget(t, db, runs[0].ScanID)
	if len(snapshot.Sources) != 1 || snapshot.Sources[0].State != domain.SourceScanCompleted {
		t.Fatalf("E2E child row = %+v", snapshot.Sources)
	}
	if reports := h.SourceReports(); len(reports) != 1 || reports[0].ScanID != runs[0].ScanID || reports[0].State != source.RunCompleted {
		t.Fatalf("E2E Source reports = %+v", reports)
	}
	revision := db.CurrentRevision()
	if revision.DataRevision != 1 || revision.StatusRevision != 4 {
		t.Fatalf("E2E revisions = %+v, want data=1 status=4", revision)
	}
	var threads, events int
	if err := db.Read(context.Background(), func(conn *sql.Conn) error {
		if err := conn.QueryRowContext(context.Background(), "SELECT count(*) FROM threads WHERE thread_id='thread-event-e2e'").Scan(&threads); err != nil {
			return err
		}
		return conn.QueryRowContext(context.Background(), "SELECT count(*) FROM usage_events WHERE source='codex' AND event_id='event-e2e'").Scan(&events)
	}); err != nil {
		t.Fatal(err)
	}
	epoch, found, err := db.GetSourceUsageEpoch(context.Background(), "codex")
	if err != nil || !found || epoch.ActiveEpoch != 1 || epoch.ActiveParserVersion != 1 || epoch.BuildEpoch != nil || threads != 1 || events != 1 {
		t.Fatalf("E2E canonical/epoch state: epoch=%+v found=%t threads=%d events=%d err=%v", epoch, found, threads, events, err)
	}
	shutdownHandle(t, h)
	assertNoDurableRunning(t, db)
}
