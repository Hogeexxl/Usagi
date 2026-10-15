package api

import (
	"context"
	"errors"
	"reflect"
	"sync"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/projection"
	"github.com/Hogeexxl/Usagi/internal/query"
)

type syncResult struct {
	revision int64
	err      error
}

type watcherTestQuery struct {
	mu      sync.Mutex
	results []syncResult
	calls   int
}

func (q *watcherTestQuery) Sync(context.Context) (int64, error) {
	q.mu.Lock()
	defer q.mu.Unlock()
	index := q.calls
	q.calls++
	if index >= len(q.results) {
		index = len(q.results) - 1
	}
	if index < 0 {
		return 0, nil
	}
	result := q.results[index]
	return result.revision, result.err
}

func (*watcherTestQuery) InitialSyncComplete(context.Context) (bool, error) {
	return true, nil
}

func (q *watcherTestQuery) callCount() int {
	q.mu.Lock()
	defer q.mu.Unlock()
	return q.calls
}

type quotaResult struct {
	view projection.QuotaView
	err  error
}

type watcherTestQuotaService struct {
	mu      sync.Mutex
	results []quotaResult
	asked   []bool
	calls   int
}

func (q *watcherTestQuotaService) Quotas(_ context.Context, asked bool) (projection.QuotaView, error) {
	q.mu.Lock()
	defer q.mu.Unlock()
	q.asked = append(q.asked, asked)
	index := q.calls
	q.calls++
	if index >= len(q.results) {
		index = len(q.results) - 1
	}
	if index < 0 {
		return projection.QuotaView{}, nil
	}
	result := q.results[index]
	return result.view, result.err
}

func TestWatcherPublishesDatabaseRevisionEvenWhenSyncReportsError(t *testing.T) {
	failure := errors.New("one day failed after another day committed")
	queryService := &watcherTestQuery{results: []syncResult{{revision: 7, err: failure}, {revision: 7}}}
	revisions := NewRevisionService(6)
	watcher := newWatchers(queryService, nil, revisions, nil, time.Second, time.Second)
	watcher.refreshUsage(context.Background())
	if got := revisions.Current(); got.DataRevision != 7 {
		t.Fatalf("revision after partial sync = %+v, want database revision 7", got)
	}
	watcher.refreshUsage(context.Background())
	if got := revisions.Current(); got.DataRevision != 7 || queryService.callCount() != 2 {
		t.Fatalf("revision after repeated sync = %+v, calls %d, want unchanged revision 7 and 2 calls", got, queryService.callCount())
	}
}

func TestUsageFingerprintIncludesTimingAndExcludesGeneratedFields(t *testing.T) {
	base := magpie.UsageSummary{
		Period: "all", Bucket: "month", Since: time.Date(2020, 1, 1, 0, 0, 0, 0, time.UTC),
		UsageTotals: magpie.UsageTotals{Calls: 2, Input: 10, Timed: 1, TTFTMs: 4, DecodeMs: 5, DecodeOut: 6, DecodeN: 1},
		Models:      []magpie.UsageGroup{{ID: "provider/model", Provider: "provider", Model: "model", UsageTotals: magpie.UsageTotals{Calls: 2, Input: 10}}},
	}
	changed := base
	changed.Period = "different-generated-period"
	changed.Bucket = "different-generated-bucket"
	changed.Since = time.Date(2021, 2, 3, 0, 0, 0, 0, time.UTC)
	a, err := query.UsageFingerprint(base)
	if err != nil {
		t.Fatal(err)
	}
	b, err := query.UsageFingerprint(changed)
	if err != nil {
		t.Fatal(err)
	}
	if a != b {
		t.Fatal("generated boundary fields altered the Usage fingerprint")
	}
	for name, alter := range map[string]func(*magpie.UsageTotals){
		"timed":      func(value *magpie.UsageTotals) { value.Timed++ },
		"ttft":       func(value *magpie.UsageTotals) { value.TTFTMs++ },
		"decode_ms":  func(value *magpie.UsageTotals) { value.DecodeMs++ },
		"decode_out": func(value *magpie.UsageTotals) { value.DecodeOut++ },
		"decode_n":   func(value *magpie.UsageTotals) { value.DecodeN++ },
	} {
		t.Run(name, func(t *testing.T) {
			value := base
			alter(&value.UsageTotals)
			fingerprint, err := query.UsageFingerprint(value)
			if err != nil {
				t.Fatal(err)
			}
			if a == fingerprint {
				t.Fatalf("timing-only %s change did not alter the Usage fingerprint", name)
			}
		})
	}
	changed.Models[0].Provider = "other-provider"
	b, err = query.UsageFingerprint(changed)
	if err != nil {
		t.Fatal(err)
	}
	if a == b {
		t.Fatal("provider identity change did not alter the Usage fingerprint")
	}
}

func TestQuotaWatcherStoresProjectionViewWithoutBumpingRevision(t *testing.T) {
	first := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Name: "Codex", Plan: "first"}}}
	second := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Name: "Codex", Plan: "second"}}}
	service := &watcherTestQuotaService{results: []quotaResult{{view: first}, {view: second}}}
	snapshot := &QuotaSnapshot{}
	revisions := NewRevisionService(0)
	watcher := newWatchers(nil, service, revisions, snapshot, time.Second, 2*time.Second)
	if _, loaded, err := snapshot.Current(); loaded || err != nil {
		t.Fatalf("initial quota snapshot = loaded %t, error %v", loaded, err)
	}
	watcher.refreshQuota(context.Background())
	watcher.refreshQuota(context.Background())
	got, loaded, err := snapshot.Current()
	if err != nil || !loaded || !reflect.DeepEqual(got, second) {
		t.Fatalf("quota snapshot = %+v, loaded %t, error %v", got, loaded, err)
	}
	if !reflect.DeepEqual(service.asked, []bool{false, false}) {
		t.Fatalf("background Quotas asked values = %v, want [false false]", service.asked)
	}
	if got := revisions.Current(); got != (RevisionTuple{}) {
		t.Fatalf("quota changes tuple = %+v, want no revision bump", got)
	}
}

func TestQuotaWatcherFailurePreservesSnapshotAndRecordsError(t *testing.T) {
	view := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Name: "Codex"}}}
	failure := errors.New("quota refresh failed")
	service := &watcherTestQuotaService{results: []quotaResult{{view: view}, {err: failure}}}
	snapshot := &QuotaSnapshot{}
	watcher := newWatchers(nil, service, NewRevisionService(0), snapshot, time.Second, 2*time.Second)
	watcher.refreshQuota(context.Background())
	watcher.refreshQuota(context.Background())
	got, loaded, err := snapshot.Current()
	if !loaded || !errors.Is(err, failure) || !reflect.DeepEqual(got, view) {
		t.Fatalf("quota snapshot after error = %+v, loaded %t, error %v", got, loaded, err)
	}
}

func TestQuotaWatcherPollsReadingSnapshotUntilLoadedWithoutAsking(t *testing.T) {
	reading := projection.QuotaView{Reading: true, Items: []projection.QuotaProviderView{}}
	loaded := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Name: "Codex", Plan: "loaded"}}}
	service := &watcherTestQuotaService{results: []quotaResult{{view: reading}, {view: loaded}}}
	snapshot := &QuotaSnapshot{}
	watcher := newWatchers(nil, service, NewRevisionService(0), snapshot, time.Hour, time.Hour)
	runtime := newStatusTestSource(magpie.RuntimeStatusReady)
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		defer close(done)
		watcher.runQuota(ctx, runtime, 10*time.Millisecond)
	}()

	waitQuotaWatcherSnapshot(t, snapshot, func(view projection.QuotaView, loadedSnapshot bool, err error) bool {
		return loadedSnapshot && err == nil && reflect.DeepEqual(view, reading)
	})
	waitQuotaWatcherSnapshot(t, snapshot, func(view projection.QuotaView, loadedSnapshot bool, err error) bool {
		return loadedSnapshot && err == nil && reflect.DeepEqual(view, loaded)
	})
	time.Sleep(30 * time.Millisecond)
	service.mu.Lock()
	calls := service.calls
	asked := append([]bool(nil), service.asked...)
	service.mu.Unlock()
	if calls != 2 || !reflect.DeepEqual(asked, []bool{false, false}) {
		t.Fatalf("startup quota reads = %d asked %v, want two cached reads without asking", calls, asked)
	}

	cancel()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("quota watcher did not stop after context cancellation")
	}
}

func waitQuotaWatcherSnapshot(t *testing.T, snapshot *QuotaSnapshot, ready func(projection.QuotaView, bool, error) bool) {
	t.Helper()
	deadline := time.NewTimer(time.Second)
	defer deadline.Stop()
	ticker := time.NewTicker(time.Millisecond)
	defer ticker.Stop()
	for {
		if view, loaded, err := snapshot.Current(); ready(view, loaded, err) {
			return
		}
		select {
		case <-ticker.C:
		case <-deadline.C:
			t.Fatal("timed out waiting for quota snapshot")
		}
	}
}

func TestWatchersKeepUsageAndQuotaIntervalsIndependent(t *testing.T) {
	watcher := newWatchers(nil, nil, NewRevisionService(0), &QuotaSnapshot{}, 7*time.Second, 11*time.Second)
	if watcher.usageInterval != 7*time.Second || watcher.quotaInterval != 11*time.Second {
		t.Fatalf("watcher intervals = %v/%v", watcher.usageInterval, watcher.quotaInterval)
	}
	if quotaReadingPollInterval != 5*time.Second {
		t.Fatalf("quota reading interval = %v, want 5 seconds", quotaReadingPollInterval)
	}
}

type statusTestSource struct {
	mu      sync.Mutex
	status  magpie.RuntimeStatus
	updates chan magpie.RuntimeStatus
	calls   int
}

func newStatusTestSource(status magpie.RuntimeStatus) *statusTestSource {
	return &statusTestSource{status: status, updates: make(chan magpie.RuntimeStatus, 8)}
}

func (s *statusTestSource) Status() magpie.RuntimeStatus {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.calls++
	return s.status
}

func (s *statusTestSource) Subscribe() (<-chan magpie.RuntimeStatus, func()) {
	s.mu.Lock()
	s.updates <- s.status
	s.mu.Unlock()
	return s.updates, func() {}
}

func (s *statusTestSource) set(status magpie.RuntimeStatus) {
	s.mu.Lock()
	if s.status != status {
		s.status = status
		s.updates <- status
	}
	s.mu.Unlock()
}

func (s *statusTestSource) statusCalls() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.calls
}

func TestRuntimeStatusSubscriptionBumpsOnlyStatusRevision(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	runtime := newStatusTestSource(magpie.RuntimeStatusUnavailable)
	revisions := NewRevisionService(0)
	startRuntimeStatusSubscription(ctx, runtime, revisions)
	runtime.set(magpie.RuntimeStatusStarting)
	waitRevision(t, revisions, RevisionTuple{StatusRevision: 1})
	runtime.set(magpie.RuntimeStatusReady)
	waitRevision(t, revisions, RevisionTuple{StatusRevision: 2})
	runtime.set(magpie.RuntimeStatusUnavailable)
	waitRevision(t, revisions, RevisionTuple{StatusRevision: 3})
	if calls := runtime.statusCalls(); calls != 0 {
		t.Fatalf("status source polled %d times; the subscription must be authoritative", calls)
	}
}

func waitRevision(t *testing.T, revisions RevisionService, want RevisionTuple) {
	t.Helper()
	deadline := time.NewTimer(time.Second)
	defer deadline.Stop()
	ticker := time.NewTicker(time.Millisecond)
	defer ticker.Stop()
	for {
		if got := revisions.Current(); got == want {
			return
		}
		select {
		case <-ticker.C:
		case <-deadline.C:
			t.Fatalf("revision tuple = %+v, want %+v", revisions.Current(), want)
		}
	}
}
