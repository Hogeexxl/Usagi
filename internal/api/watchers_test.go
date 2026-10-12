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

type usageResult struct {
	summary magpie.UsageSummary
	err     error
}

type sessionResult struct {
	facts []query.SessionRefreshFact
	err   error
}

type watcherTestQuery struct {
	mu             sync.Mutex
	usageResults   []usageResult
	sessionResults []sessionResult
	rowsChanged    []bool
	usageCalls     int
	sessionCalls   int
	rowCalls       []bool
}

func (q *watcherTestQuery) RefreshUsageRows(_ context.Context, forceRecent bool) (bool, error) {
	q.mu.Lock()
	defer q.mu.Unlock()
	changed := len(q.rowCalls) < len(q.rowsChanged) && q.rowsChanged[len(q.rowCalls)]
	q.rowCalls = append(q.rowCalls, forceRecent)
	return changed, nil
}

func (q *watcherTestQuery) UsageSnapshot(context.Context) (magpie.UsageSummary, error) {
	q.mu.Lock()
	defer q.mu.Unlock()
	result := q.usageResults[q.usageCalls]
	q.usageCalls++
	return result.summary, result.err
}

func (q *watcherTestQuery) RefreshSessions(context.Context) ([]query.SessionRefreshFact, error) {
	q.mu.Lock()
	defer q.mu.Unlock()
	result := q.sessionResults[q.sessionCalls]
	q.sessionCalls++
	return result.facts, result.err
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
	result := q.results[q.calls]
	q.calls++
	return result.view, result.err
}

func TestUsageWatcherBaselinesAndBumpsOnceForChangedSnapshot(t *testing.T) {
	base := magpie.UsageSummary{UsageTotals: magpie.UsageTotals{Calls: 2, Input: 10, Cost: 0.5, Unpriced: 1}}
	tests := []struct {
		name    string
		changed magpie.UsageSummary
	}{
		{name: "tokens", changed: magpie.UsageSummary{UsageTotals: magpie.UsageTotals{Calls: 2, Input: 11, Cost: 0.5, Unpriced: 1}}},
		{name: "cost", changed: magpie.UsageSummary{UsageTotals: magpie.UsageTotals{Calls: 2, Input: 10, Cost: 0.75, Unpriced: 1}}},
		{name: "model", changed: magpie.UsageSummary{Models: []magpie.UsageGroup{{ID: "provider/new-model", Provider: "provider", Model: "new-model"}}}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			query := &watcherTestQuery{usageResults: []usageResult{{summary: base}, {summary: base}, {summary: test.changed}}}
			revisions := NewRevisionService()
			watcher := newWatchers(query, nil, revisions, nil, time.Second, 2*time.Second, 3*time.Second)
			watcher.refreshUsage(context.Background())
			watcher.refreshUsage(context.Background())
			if got := revisions.Current(); got.DataRevision != 0 {
				t.Fatalf("baseline/repeat tuple = %+v, want no bump", got)
			}
			watcher.refreshUsage(context.Background())
			if got := revisions.Current(); got.DataRevision != 1 {
				t.Fatalf("changed snapshot tuple = %+v, want exactly one data bump", got)
			}
		})
	}
}

func TestUsageWatcherFindsHistoricalOnlyChanges(t *testing.T) {
	timeAt := time.Date(2022, time.January, 1, 0, 0, 0, 0, time.UTC)
	base := magpie.UsageSummary{
		UsageTotals: magpie.UsageTotals{Calls: 1, Input: 10},
		Series:      []magpie.UsagePoint{{Label: "2022-01-01", Time: timeAt, UsageTotals: magpie.UsageTotals{Calls: 1, Input: 10}}},
	}
	changed := base
	changed.Series = []magpie.UsagePoint{{Label: "2022-01-01", Time: timeAt, UsageTotals: magpie.UsageTotals{Calls: 1, Input: 11}}}
	query := &watcherTestQuery{usageResults: []usageResult{{summary: base}, {summary: changed}}}
	revisions := NewRevisionService()
	watcher := newWatchers(query, nil, revisions, nil, time.Second, 2*time.Second, 3*time.Second)
	watcher.refreshUsage(context.Background())
	watcher.refreshUsage(context.Background())
	if got := revisions.Current().DataRevision; got != 1 {
		t.Fatalf("historical-only change data revision = %d, want 1", got)
	}
}

func TestUsageFingerprintExcludesGeneratedAndTimingFields(t *testing.T) {
	base := magpie.UsageSummary{
		Period: "all", Bucket: "month", Since: time.Date(2020, 1, 1, 0, 0, 0, 0, time.UTC),
		UsageTotals: magpie.UsageTotals{Calls: 2, Input: 10, Timed: 1, TTFTMs: 4, DecodeMs: 5, DecodeOut: 6},
		Models:      []magpie.UsageGroup{{ID: "provider/model", Provider: "provider", Model: "model", UsageTotals: magpie.UsageTotals{Calls: 2, Input: 10}}},
	}
	changed := base
	changed.Period = "different-generated-period"
	changed.Bucket = "different-generated-bucket"
	changed.Since = time.Date(2021, 2, 3, 0, 0, 0, 0, time.UTC)
	changed.Timed++
	changed.TTFTMs++
	changed.DecodeMs++
	changed.DecodeOut++
	a, err := usageFingerprint(base)
	if err != nil {
		t.Fatal(err)
	}
	b, err := usageFingerprint(changed)
	if err != nil {
		t.Fatal(err)
	}
	if a != b {
		t.Fatal("generated boundary or timing-only changes altered the Usage fingerprint")
	}
	changed.Models[0].Provider = "other-provider"
	b, err = usageFingerprint(changed)
	if err != nil {
		t.Fatal(err)
	}
	if a == b {
		t.Fatal("provider identity change did not alter the Usage fingerprint")
	}
}

func TestSessionWatcherUsesOneRefreshSeamAndFingerprintsVisibleFacts(t *testing.T) {
	baseline := []query.SessionRefreshFact{{Agent: "codex", ID: "id", Title: "Before", Cwd: "/old"}}
	query := &watcherTestQuery{sessionResults: []sessionResult{
		{facts: baseline}, {facts: baseline},
		{facts: []query.SessionRefreshFact{{Agent: "codex", ID: "id", Title: "After", Cwd: "/new"}}},
	}}
	revisions := NewRevisionService()
	watcher := newWatchers(query, nil, revisions, nil, time.Second, 2*time.Second, 3*time.Second)
	watcher.refreshSessions(context.Background())
	watcher.refreshSessions(context.Background())
	if got := revisions.Current().DataRevision; got != 0 {
		t.Fatalf("session baseline/repeat data revision = %d, want 0", got)
	}
	watcher.refreshSessions(context.Background())
	if got := revisions.Current().DataRevision; got != 1 {
		t.Fatalf("session metadata change data revision = %d, want 1", got)
	}
	if query.sessionCalls != 3 {
		t.Fatalf("RefreshSessions calls = %d, want one per tick (3)", query.sessionCalls)
	}
}

func TestWatcherFailuresRetainLastSuccessfulBaseline(t *testing.T) {
	failure := errors.New("temporary upstream failure")
	base := magpie.UsageSummary{UsageTotals: magpie.UsageTotals{Calls: 1, Input: 7}}
	changed := magpie.UsageSummary{UsageTotals: magpie.UsageTotals{Calls: 1, Input: 8}}
	facts := []query.SessionRefreshFact{{Agent: "codex", ID: "id", Title: "stable"}}
	query := &watcherTestQuery{
		usageResults:   []usageResult{{summary: base}, {err: failure}, {summary: base}, {summary: changed}},
		sessionResults: []sessionResult{{facts: facts}, {err: failure}, {facts: facts}, {facts: append([]query.SessionRefreshFact(nil), facts...)}},
	}
	revisions := NewRevisionService()
	watcher := newWatchers(query, nil, revisions, nil, time.Second, 2*time.Second, 3*time.Second)
	watcher.refreshUsage(context.Background())
	watcher.refreshSessions(context.Background())
	watcher.refreshUsage(context.Background())
	watcher.refreshSessions(context.Background())
	watcher.refreshUsage(context.Background())
	watcher.refreshSessions(context.Background())
	if got := revisions.Current().DataRevision; got != 0 {
		t.Fatalf("failed/recovered same snapshot tuple = %+v, want no bump", revisions.Current())
	}
	watcher.refreshUsage(context.Background())
	if got := revisions.Current().DataRevision; got != 1 {
		t.Fatalf("changed post-failure snapshot tuple = %+v, want one bump", revisions.Current())
	}
}

func TestQuotaWatcherStoresProjectionViewWithoutBumpingRevision(t *testing.T) {
	first := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Name: "Codex", Plan: "first"}}}
	second := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Name: "Codex", Plan: "second"}}}
	service := &watcherTestQuotaService{results: []quotaResult{{view: first}, {view: second}}}
	snapshot := &QuotaSnapshot{}
	revisions := NewRevisionService()
	watcher := newWatchers(nil, service, revisions, snapshot, time.Second, 2*time.Second, 3*time.Second)
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
	watcher := newWatchers(nil, service, NewRevisionService(), snapshot, time.Second, 2*time.Second, 3*time.Second)
	watcher.refreshQuota(context.Background())
	watcher.refreshQuota(context.Background())
	got, loaded, err := snapshot.Current()
	if !loaded || !errors.Is(err, failure) || !reflect.DeepEqual(got, view) {
		t.Fatalf("quota snapshot after error = %+v, loaded %t, error %v", got, loaded, err)
	}
}

func TestWatchersKeepIntervalsIndependent(t *testing.T) {
	watcher := newWatchers(nil, nil, NewRevisionService(), &QuotaSnapshot{}, 7*time.Second, 11*time.Second, 19*time.Second)
	if watcher.usageInterval != 7*time.Second || watcher.sessionInterval != 11*time.Second || watcher.quotaInterval != 19*time.Second {
		t.Fatalf("watcher intervals = %v/%v/%v", watcher.usageInterval, watcher.sessionInterval, watcher.quotaInterval)
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

func TestRuntimeStatusSubscriptionBumpsEachTransitionAndReadyAvailability(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	runtime := newStatusTestSource(magpie.RuntimeStatusUnavailable)
	revisions := NewRevisionService()
	startRuntimeStatusSubscription(ctx, runtime, revisions)
	runtime.set(magpie.RuntimeStatusStarting)
	waitRevision(t, revisions, RevisionTuple{StatusRevision: 1})
	runtime.set(magpie.RuntimeStatusReady)
	waitRevision(t, revisions, RevisionTuple{DataRevision: 1, StatusRevision: 2})
	runtime.set(magpie.RuntimeStatusUnavailable)
	waitRevision(t, revisions, RevisionTuple{DataRevision: 1, StatusRevision: 3})
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
