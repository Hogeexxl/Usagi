package api

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"reflect"
	"sync"
	"testing"
	"testing/fstest"
	"time"

	"github.com/Hogeexxl/Usagi/internal/api/testsupport"
	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/projection"
)

type quotaStatusSource struct {
	mu          sync.Mutex
	status      magpie.RuntimeStatus
	nextID      uint64
	subscribers map[uint64]chan magpie.RuntimeStatus
	statusCalls int
}

func newQuotaStatusSource(status magpie.RuntimeStatus) *quotaStatusSource {
	return &quotaStatusSource{status: status, subscribers: make(map[uint64]chan magpie.RuntimeStatus)}
}

func (s *quotaStatusSource) Status() magpie.RuntimeStatus {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.statusCalls++
	return s.status
}

func (s *quotaStatusSource) Subscribe() (<-chan magpie.RuntimeStatus, func()) {
	s.mu.Lock()
	s.nextID++
	id := s.nextID
	updates := make(chan magpie.RuntimeStatus, 8)
	updates <- s.status
	s.subscribers[id] = updates
	s.mu.Unlock()
	var once sync.Once
	return updates, func() {
		once.Do(func() {
			s.mu.Lock()
			delete(s.subscribers, id)
			close(updates)
			s.mu.Unlock()
		})
	}
}

func (s *quotaStatusSource) set(status magpie.RuntimeStatus) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.status == status {
		return
	}
	s.status = status
	for _, updates := range s.subscribers {
		updates <- status
	}
}

func (s *quotaStatusSource) counts() (int, int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.subscribers), s.statusCalls
}

type quotaWorkerService struct {
	mu        sync.Mutex
	results   []quotaResult
	firstGate chan struct{}
	started   chan int
	asked     []bool
	calls     int
	active    int
	maxActive int
}

func newQuotaWorkerService(results []quotaResult) *quotaWorkerService {
	return &quotaWorkerService{results: results, started: make(chan int, 16)}
}

func (s *quotaWorkerService) Quotas(ctx context.Context, asked bool) (projection.QuotaView, error) {
	s.mu.Lock()
	call := s.calls
	s.calls++
	s.asked = append(s.asked, asked)
	s.active++
	if s.active > s.maxActive {
		s.maxActive = s.active
	}
	result := quotaResult{}
	if call < len(s.results) {
		result = s.results[call]
	} else if len(s.results) != 0 {
		result = s.results[len(s.results)-1]
	}
	gate := s.firstGate
	s.mu.Unlock()

	s.started <- call + 1
	if call == 0 && gate != nil {
		select {
		case <-gate:
		case <-ctx.Done():
			result.err = ctx.Err()
		}
	}

	s.mu.Lock()
	s.active--
	s.mu.Unlock()
	return result.view, result.err
}

func (s *quotaWorkerService) snapshot() (int, int, []bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.calls, s.maxActive, append([]bool(nil), s.asked...)
}

type quotaWorkerProjection struct {
	*testsupport.FakeProjectionService
	quota watcherQuotaService
}

func (p quotaWorkerProjection) Quotas(ctx context.Context, asked bool) (projection.QuotaView, error) {
	return p.quota.Quotas(ctx, asked)
}

func newQuotaWorkerServer(t *testing.T, runtime RuntimeStatusSource, revisions RevisionSource, snapshot *QuotaSnapshot, quota watcherQuotaService, quotaInterval time.Duration) *Server {
	t.Helper()
	config := DefaultConfig()
	config.StaticFS = fstest.MapFS{}
	config.UsageRefreshInterval = time.Hour
	config.QuotaRefreshInterval = quotaInterval
	server, err := NewServer(config, Dependencies{
		Projection: quotaWorkerProjection{FakeProjectionService: &testsupport.FakeProjectionService{}, quota: quota},
		Query:      &testsupport.FakeWatcherQuery{}, Revisions: revisions,
		RuntimeStatus: runtime, QuotaSnapshot: snapshot, ShutdownRequester: func() {},
	})
	if err != nil {
		t.Fatalf("NewServer() error = %v", err)
	}
	return server
}

func TestServerQuotaWatcherStartsOnReadyAndFollowsReading(t *testing.T) {
	first := projection.QuotaView{Reading: true, Items: []projection.QuotaProviderView{}}
	second := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Name: "Codex", Plan: "loaded"}}}
	third := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Name: "Codex", Plan: "reloaded"}}}
	service := newQuotaWorkerService([]quotaResult{{view: first}, {view: second}, {view: third}})
	runtime := newQuotaStatusSource(magpie.RuntimeStatusUnavailable)
	revisions := NewRevisionService(0)
	snapshot := &QuotaSnapshot{}
	server := newQuotaWorkerServer(t, runtime, revisions, snapshot, service, 200*time.Millisecond)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	server.Start(ctx)

	initial := request(server, http.MethodGet, "/api/quota")
	var initialView quotaResponseDTO
	if err := json.Unmarshal(initial.Body.Bytes(), &initialView); err != nil {
		t.Fatal(err)
	}
	if initial.Code != http.StatusOK || !initialView.Reading || len(initialView.Items) != 0 {
		t.Fatalf("initial cached quota = %d %+v, want an unloaded reading snapshot", initial.Code, initialView)
	}
	if calls, _, _ := service.snapshot(); calls != 0 {
		t.Fatalf("GET /api/quota triggered %d reads while Runtime was unavailable", calls)
	}

	runtime.set(magpie.RuntimeStatusReady)
	waitQuotaCall(t, service, 1)
	waitQuotaSnapshot(t, snapshot, func(view projection.QuotaView, loaded bool, err error) bool {
		return loaded && err == nil && view.Reading
	})
	loading := request(server, http.MethodGet, "/api/quota")
	var loadingView quotaResponseDTO
	if err := json.Unmarshal(loading.Body.Bytes(), &loadingView); err != nil {
		t.Fatal(err)
	}
	if loading.Code != http.StatusOK || !loadingView.Reading || len(loadingView.Items) != 0 {
		t.Fatalf("cached reading quota = %d %+v, want reading=true", loading.Code, loadingView)
	}
	if calls, _, asked := service.snapshot(); calls != 1 || !reflect.DeepEqual(asked, []bool{false}) {
		t.Fatalf("background quota calls after cached GET = %d asked %v, want one asked=false read", calls, asked)
	}
	waitQuotaCall(t, service, 2)
	waitQuotaSnapshot(t, snapshot, func(view projection.QuotaView, loaded bool, err error) bool {
		return loaded && err == nil && reflect.DeepEqual(view, second)
	})
	readyTuple := RevisionTuple{DataRevision: 1, StatusRevision: 1}
	waitForQuota(t, func() bool { return revisions.Current() == readyTuple })
	if got := revisions.Current(); got != readyTuple {
		t.Fatalf("quota loading completion tuple = %+v, want ready transition only %+v", got, readyTuple)
	}

	runtime.set(magpie.RuntimeStatusUnavailable)
	waitForQuota(t, func() bool { return revisions.Current() == (RevisionTuple{DataRevision: 1, StatusRevision: 2}) })
	time.Sleep(3 * 200 * time.Millisecond)
	if calls, _, _ := service.snapshot(); calls != 2 {
		t.Fatalf("quota watcher made %d reads while Runtime was unavailable, want 2", calls)
	}
	runtime.set(magpie.RuntimeStatusReady)
	waitQuotaCall(t, service, 3)
	waitQuotaSnapshot(t, snapshot, func(view projection.QuotaView, loaded bool, err error) bool {
		return loaded && err == nil && reflect.DeepEqual(view, third)
	})
	readyAgainTuple := RevisionTuple{DataRevision: 2, StatusRevision: 3}
	waitForQuota(t, func() bool { return revisions.Current() == readyAgainTuple })
	if got := revisions.Current(); got != readyAgainTuple {
		t.Fatalf("quota reload tuple = %+v, want ready transition only %+v", got, readyAgainTuple)
	}
	if calls, maxActive, asked := service.snapshot(); calls != 3 || maxActive != 1 || !reflect.DeepEqual(asked, []bool{false, false, false}) {
		t.Fatalf("quota watcher calls = %d, max concurrency %d, asked %v", calls, maxActive, asked)
	}

	cancel()
	waitForQuota(t, func() bool {
		subscriptions, _ := runtime.counts()
		return subscriptions == 0
	})
	time.Sleep(250 * time.Millisecond)
	if calls, _, _ := service.snapshot(); calls != 3 {
		t.Fatalf("quota worker made %d reads after cancellation, want 3", calls)
	}
	if _, statusCalls := runtime.counts(); statusCalls != 0 {
		t.Fatalf("quota watcher polled Runtime.Status %d times", statusCalls)
	}
}

func TestQuotaWorkerBoundsFailuresAndSerializesReads(t *testing.T) {
	failure := errors.New("quota refresh failed")
	old := projection.QuotaView{Reading: true, Items: []projection.QuotaProviderView{}}
	loaded := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Plan: "loaded"}}}
	service := newQuotaWorkerService([]quotaResult{{view: old}, {err: failure}, {view: loaded}})
	service.firstGate = make(chan struct{})
	runtime := newQuotaStatusSource(magpie.RuntimeStatusReady)
	snapshot := &QuotaSnapshot{}
	revisions := NewRevisionService(0)
	watcher := newWatchers(nil, service, revisions, snapshot, time.Hour, 200*time.Millisecond)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan struct{})
	go func() {
		defer close(done)
		watcher.runQuota(ctx, runtime, 80*time.Millisecond)
	}()
	waitQuotaCall(t, service, 1)
	time.Sleep(20 * time.Millisecond)
	if calls, _, _ := service.snapshot(); calls != 1 {
		t.Fatalf("quota worker started %d reads while the first read was blocked", calls)
	}
	close(service.firstGate)
	waitQuotaCall(t, service, 2)
	waitForQuota(t, func() bool {
		_, _, err := snapshot.Current()
		return errors.Is(err, failure)
	})
	time.Sleep(25 * time.Millisecond)
	if calls, maxActive, asked := service.snapshot(); calls != 2 || maxActive != 1 || !reflect.DeepEqual(asked, []bool{false, false}) {
		t.Fatalf("failed quota refresh was retried too quickly or overlapped: calls %d, max concurrency %d, asked %v", calls, maxActive, asked)
	}
	view, loadedSnapshot, err := snapshot.Current()
	if !loadedSnapshot || !errors.Is(err, failure) || !reflect.DeepEqual(view, old) {
		t.Fatalf("quota snapshot after failed read = %+v, loaded %t, error %v", view, loadedSnapshot, err)
	}

	runtime.set(magpie.RuntimeStatusUnavailable)
	time.Sleep(20 * time.Millisecond)
	time.Sleep(200 * time.Millisecond)
	if calls, _, _ := service.snapshot(); calls != 2 {
		t.Fatalf("quota worker retried %d times while Runtime was unavailable, want 2 total reads", calls)
	}
	runtime.set(magpie.RuntimeStatusReady)
	waitQuotaCall(t, service, 3)
	waitQuotaSnapshot(t, snapshot, func(view projection.QuotaView, isLoaded bool, err error) bool {
		return isLoaded && err == nil && reflect.DeepEqual(view, loaded)
	})

	cancel()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("quota worker did not stop after context cancellation")
	}
	if subscriptions, statusCalls := runtime.counts(); subscriptions != 0 || statusCalls != 0 {
		t.Fatalf("runtime subscriptions after cancellation = %d, Status calls = %d", subscriptions, statusCalls)
	}
	time.Sleep(220 * time.Millisecond)
	if calls, maxActive, asked := service.snapshot(); calls != 3 || maxActive != 1 || !reflect.DeepEqual(asked, []bool{false, false, false}) {
		t.Fatalf("quota worker after cancellation = calls %d, max concurrency %d, asked %v", calls, maxActive, asked)
	}
	if got := revisions.Current(); got != (RevisionTuple{}) {
		t.Fatalf("quota reads changed revisions to %+v, want zero", got)
	}
}

func TestQuotaWorkerRetriesInitialFailureWithShortInterval(t *testing.T) {
	failure := errors.New("initial quota read failed")
	loaded := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Plan: "loaded"}}}
	service := newQuotaWorkerService([]quotaResult{{err: failure}, {view: loaded}})
	runtime := newQuotaStatusSource(magpie.RuntimeStatusReady)
	snapshot := &QuotaSnapshot{}
	watcher := newWatchers(nil, service, NewRevisionService(0), snapshot, time.Hour, time.Second)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan struct{})
	go func() {
		defer close(done)
		watcher.runQuota(ctx, runtime, 5*time.Millisecond)
	}()

	waitQuotaCall(t, service, 1)
	firstReadAt := time.Now()
	waitForQuota(t, func() bool {
		_, _, err := snapshot.Current()
		return errors.Is(err, failure)
	})
	waitQuotaCall(t, service, 2)
	if elapsed := time.Since(firstReadAt); elapsed >= watcher.quotaInterval/2 {
		t.Fatalf("second quota read started after %v, want it before the %v regular interval", elapsed, watcher.quotaInterval)
	}
	waitQuotaSnapshot(t, snapshot, func(view projection.QuotaView, loadedSnapshot bool, err error) bool {
		return loadedSnapshot && err == nil && reflect.DeepEqual(view, loaded)
	})
	if calls, _, asked := service.snapshot(); calls != 2 || !reflect.DeepEqual(asked, []bool{false, false}) {
		t.Fatalf("startup quota reads = %d asked %v, want two background reads", calls, asked)
	}
	cancel()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("quota worker did not stop after context cancellation")
	}
}

func TestQuotaWorkerKeepsCompletedSnapshotFailuresOnRegularInterval(t *testing.T) {
	failure := errors.New("completed quota refresh failed")
	previous := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Plan: "previous"}}}
	updated := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Plan: "updated"}}}
	service := newQuotaWorkerService([]quotaResult{{err: failure}, {view: updated}})
	runtime := newQuotaStatusSource(magpie.RuntimeStatusReady)
	snapshot := &QuotaSnapshot{}
	snapshot.Replace(previous)
	watcher := newWatchers(nil, service, NewRevisionService(0), snapshot, time.Hour, 250*time.Millisecond)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan struct{})
	go func() {
		defer close(done)
		watcher.runQuota(ctx, runtime, 5*time.Millisecond)
	}()

	waitQuotaCall(t, service, 1)
	firstReadAt := time.Now()
	waitForQuota(t, func() bool {
		_, _, err := snapshot.Current()
		return errors.Is(err, failure)
	})
	view, loaded, err := snapshot.Current()
	if !loaded || !errors.Is(err, failure) || !reflect.DeepEqual(view, previous) {
		t.Fatalf("completed snapshot after failed read = %+v, loaded %t, error %v", view, loaded, err)
	}
	waitQuotaCall(t, service, 2)
	if elapsed := time.Since(firstReadAt); elapsed < watcher.quotaInterval/2 {
		t.Fatalf("completed snapshot retried after %v, want the configured %v interval", elapsed, watcher.quotaInterval)
	}
	waitQuotaSnapshot(t, snapshot, func(view projection.QuotaView, loadedSnapshot bool, err error) bool {
		return loadedSnapshot && err == nil && reflect.DeepEqual(view, updated)
	})
	cancel()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("quota worker did not stop after context cancellation")
	}
}

func waitQuotaCall(t *testing.T, service *quotaWorkerService, want int) {
	t.Helper()
	for {
		select {
		case got := <-service.started:
			if got == want {
				return
			}
			if got > want {
				t.Fatalf("quota call count advanced to %d before %d", got, want)
			}
		case <-time.After(3 * time.Second):
			t.Fatalf("timed out waiting for quota call %d", want)
		}
	}
}

func waitQuotaSnapshot(t *testing.T, snapshot *QuotaSnapshot, ready func(projection.QuotaView, bool, error) bool) {
	t.Helper()
	waitForQuota(t, func() bool {
		view, loaded, err := snapshot.Current()
		return ready(view, loaded, err)
	})
}

func waitForQuota(t *testing.T, ready func() bool) {
	t.Helper()
	deadline := time.NewTimer(3 * time.Second)
	defer deadline.Stop()
	ticker := time.NewTicker(time.Millisecond)
	defer ticker.Stop()
	for !ready() {
		select {
		case <-ticker.C:
		case <-deadline.C:
			t.Fatal("timed out waiting for quota watcher activity")
		}
	}
}
