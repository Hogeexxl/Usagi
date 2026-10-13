package query

import (
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"reflect"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type recordedQueries struct {
	mu      sync.Mutex
	queries []magpie.UsageRequestsQuery
}

func (r *recordedQueries) add(q magpie.UsageRequestsQuery) {
	r.mu.Lock()
	r.queries = append(r.queries, q)
	r.mu.Unlock()
}

func (r *recordedQueries) all() []magpie.UsageRequestsQuery {
	r.mu.Lock()
	defer r.mu.Unlock()
	return slices.Clone(r.queries)
}

func (r *recordedQueries) periods() []string {
	var periods []string
	for _, q := range r.all() {
		periods = append(periods, q.Period)
	}
	slices.Sort(periods)
	return periods
}

func (r *recordedQueries) reset() {
	r.mu.Lock()
	r.queries = nil
	r.mu.Unlock()
}

// dayRequests serves single-day periods from rows, keyed by each row's UTC date.
func dayRequests(rows []magpie.UsageRequest, queries *recordedQueries) func(context.Context, magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
	return func(_ context.Context, q magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
		queries.add(q)
		from, to, ok := strings.Cut(q.Period, "..")
		if !ok || from != to {
			return magpie.UsageRequestsPage{}, fmt.Errorf("period %q is not a single day", q.Period)
		}
		var matched []magpie.UsageRequest
		for _, row := range rows {
			if row.Time.Format(time.DateOnly) == from {
				matched = append(matched, row)
			}
		}
		end := min(q.Offset+q.Limit, len(matched))
		return magpie.UsageRequestsPage{Rows: matched[q.Offset:end], Total: len(matched)}, nil
	}
}

// storeFixture is a Magpie with per-day rows and per-day digests that tracks
// what the store fetched.
type storeFixture struct {
	mu        sync.Mutex
	rows      map[string][]magpie.UsageRequest
	hidden    map[string]bool // days whose digest ignores row changes
	fetches   []string
	digestsOn []string
	block     chan struct{}
	fail      error
	failures  int // when set, only this many requests fail
}

func newStoreFixture() *storeFixture {
	return &storeFixture{rows: make(map[string][]magpie.UsageRequest), hidden: make(map[string]bool)}
}

func (f *storeFixture) set(day string, inputs ...int) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.rows[day] = nil
	for _, input := range inputs {
		f.rows[day] = append(f.rows[day], magpie.UsageRequest{Provider: "p", Input: input, Priced: true})
	}
}

func (f *storeFixture) upstream() *sessionUpstream {
	return &sessionUpstream{
		requests: func(_ context.Context, q magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
			f.mu.Lock()
			block, fail := f.block, f.fail
			if f.failures > 0 {
				f.failures--
				if f.failures == 0 {
					f.fail = nil
				}
			}
			f.fetches = append(f.fetches, q.Period)
			f.mu.Unlock()
			if block != nil {
				<-block
			}
			if fail != nil {
				return magpie.UsageRequestsPage{}, fail
			}
			day, _, _ := strings.Cut(q.Period, "..")
			f.mu.Lock()
			rows := slices.Clone(f.rows[day])
			f.mu.Unlock()
			end := min(q.Offset+q.Limit, len(rows))
			return magpie.UsageRequestsPage{Rows: rows[q.Offset:end], Total: len(rows)}, nil
		},
		digests: func(_ context.Context, from, to string) ([]magpie.UsageDayDigest, error) {
			f.mu.Lock()
			defer f.mu.Unlock()
			f.digestsOn = append(f.digestsOn, from+".."+to)
			days, err := daysBetween(from, to)
			if err != nil {
				return nil, err
			}
			var digests []magpie.UsageDayDigest
			for _, day := range days {
				rows := f.rows[day]
				content := fmt.Sprint(rows)
				if f.hidden[day] {
					content = "hidden"
				}
				digests = append(digests, magpie.UsageDayDigest{Day: day, Calls: len(rows), Digest: sha256.Sum256([]byte(content))})
			}
			return digests, nil
		},
	}
}

func (f *storeFixture) fetched() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	fetches := slices.Clone(f.fetches)
	f.fetches = nil
	slices.Sort(fetches)
	return fetches
}

func newFixtureStore(f *storeFixture, today string, warmDays int) *usageStore {
	store := newUsageStore(f.upstream())
	now, _ := time.Parse(time.DateOnly, today)
	store.now = func() time.Time { return now.Add(12 * time.Hour) }
	store.warmDays = warmDays
	return store
}

func bucketInputs(buckets []*dayBucket) [][]int {
	var result [][]int
	for _, bucket := range buckets {
		var inputs []int
		for _, row := range bucket.rows {
			inputs = append(inputs, row.Input)
		}
		result = append(result, inputs)
	}
	return result
}

func TestStoreFetchesMissingDaysOnceAndSkipsEmptyDays(t *testing.T) {
	ctx := context.Background()
	f := newStoreFixture()
	f.set("2026-10-10", 1, 2)
	f.set("2026-10-12", 3)
	store := newFixtureStore(f, "2026-10-12", 0)

	buckets, err := store.rows(ctx, "2026-10-10", "2026-10-12")
	if err != nil {
		t.Fatal(err)
	}
	if got, want := bucketInputs(buckets), [][]int{{3}, nil, {1, 2}}; !reflect.DeepEqual(got, want) {
		t.Fatalf("buckets newest first = %v, want %v", got, want)
	}
	// 10-11 has no calls in its digest, so its rows are never requested.
	if got, want := f.fetched(), []string{"2026-10-10..2026-10-10", "2026-10-12..2026-10-12"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("fetched periods = %v, want %v", got, want)
	}
	if _, err := store.rows(ctx, "2026-10-11", "2026-10-12"); err != nil {
		t.Fatal(err)
	}
	if got := f.fetched(); len(got) != 0 {
		t.Fatalf("cached days refetched: %v", got)
	}
}

func TestStoreRefreshRefetchesOnlyChangedDays(t *testing.T) {
	ctx := context.Background()
	f := newStoreFixture()
	f.set("2026-10-10", 1)
	f.set("2026-10-11", 2)
	f.set("2026-10-12", 3)
	store := newFixtureStore(f, "2026-10-12", 0)
	if _, err := store.rows(ctx, "2026-10-10", "2026-10-12"); err != nil {
		t.Fatal(err)
	}
	f.fetched()

	changed, err := store.refresh(ctx, false)
	if err != nil || changed || len(f.fetched()) != 0 {
		t.Fatalf("idle refresh = %v, %v; want no refetch", changed, err)
	}

	f.set("2026-10-12", 3, 4)
	f.set("2026-10-10", 10)
	changed, err = store.refresh(ctx, false)
	if err != nil || !changed {
		t.Fatalf("refresh after change = %v, %v; want changed", changed, err)
	}
	if got, want := f.fetched(), []string{"2026-10-10..2026-10-10", "2026-10-12..2026-10-12"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("refetched = %v, want only changed days %v", got, want)
	}
	buckets, err := store.rows(ctx, "2026-10-10", "2026-10-12")
	if err != nil || !reflect.DeepEqual(bucketInputs(buckets), [][]int{{3, 4}, {2}, {10}}) {
		t.Fatalf("buckets after refresh = %v, %v", bucketInputs(buckets), err)
	}
}

func TestStoreForceRecentCatchesChangesHiddenFromDigests(t *testing.T) {
	ctx := context.Background()
	f := newStoreFixture()
	for _, day := range []string{"2026-10-10", "2026-10-11", "2026-10-12"} {
		f.set(day, 1)
		f.hidden[day] = true
	}
	store := newFixtureStore(f, "2026-10-12", 0)
	if _, err := store.rows(ctx, "2026-10-10", "2026-10-12"); err != nil {
		t.Fatal(err)
	}
	f.fetched()
	f.set("2026-10-12", 5)
	if changed, err := store.refresh(ctx, false); err != nil || changed {
		t.Fatalf("digest-invisible change = %v, %v; want unnoticed without force", changed, err)
	}
	changed, err := store.refresh(ctx, true)
	if err != nil || !changed {
		t.Fatalf("forced refresh = %v, %v; want changed", changed, err)
	}
	if got, want := f.fetched(), []string{"2026-10-11..2026-10-11", "2026-10-12..2026-10-12"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("forced refetch = %v, want today and yesterday %v", got, want)
	}
}

func TestStoreRefreshWarmsWindowAndEvictsIdleDays(t *testing.T) {
	ctx := context.Background()
	f := newStoreFixture()
	f.set("2026-10-11", 1)
	f.set("2026-10-12", 2)
	f.set("2026-09-01", 9)
	store := newFixtureStore(f, "2026-10-12", 3)
	changed, err := store.refresh(ctx, false)
	if err != nil || changed {
		t.Fatalf("warming refresh = %v, %v; want no change reported", changed, err)
	}
	if got, want := f.fetched(), []string{"2026-10-11..2026-10-11", "2026-10-12..2026-10-12"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("warm fetches = %v, want %v", got, want)
	}
	if _, err := store.rows(ctx, "2026-09-01", "2026-09-01"); err != nil {
		t.Fatal(err)
	}
	store.mu.Lock()
	_, cached := store.days["2026-09-01"]
	store.lastUsed["2026-09-01"] = store.now().Add(-storeIdleTTL - time.Second)
	store.mu.Unlock()
	if !cached {
		t.Fatal("on-demand day was not cached")
	}
	if _, err := store.refresh(ctx, false); err != nil {
		t.Fatal(err)
	}
	store.mu.Lock()
	_, cached = store.days["2026-09-01"]
	_, warm := store.days["2026-10-12"]
	store.mu.Unlock()
	if cached || !warm {
		t.Fatalf("after eviction cached old=%v warm=%v; want old evicted, warm kept", cached, warm)
	}
}

func TestStoreSharesInflightFetchesAndSurvivesCallerCancel(t *testing.T) {
	f := newStoreFixture()
	f.set("2026-10-12", 7)
	f.block = make(chan struct{})
	store := newFixtureStore(f, "2026-10-12", 0)

	cancelled, cancel := context.WithCancel(context.Background())
	first := make(chan error, 1)
	go func() {
		_, err := store.rows(cancelled, "2026-10-12", "2026-10-12")
		first <- err
	}()
	second := make(chan []*dayBucket, 1)
	go func() {
		buckets, _ := store.rows(context.Background(), "2026-10-12", "2026-10-12")
		second <- buckets
	}()
	for {
		store.mu.Lock()
		_, inflight := store.inflight["2026-10-12"]
		store.mu.Unlock()
		if inflight {
			break
		}
		time.Sleep(time.Millisecond)
	}
	cancel()
	if err := <-first; !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled caller = %v, want context.Canceled", err)
	}
	f.mu.Lock()
	close(f.block)
	f.block = nil
	f.mu.Unlock()
	if got := bucketInputs(<-second); !reflect.DeepEqual(got, [][]int{{7}}) {
		t.Fatalf("waiting caller = %v, want the shared fetch result", got)
	}
	if got := f.fetched(); len(got) != 1 {
		t.Fatalf("fetches = %v, want one shared fetch", got)
	}
}

func TestStoreDigestsChunkLongSpans(t *testing.T) {
	f := newStoreFixture()
	store := newFixtureStore(f, "2026-10-12", 0)
	if _, err := store.rows(context.Background(), "2026-01-01", "2026-04-10"); err != nil {
		t.Fatal(err)
	}
	want := []string{"2026-01-01..2026-03-01", "2026-03-02..2026-04-10"}
	if !reflect.DeepEqual(f.digestsOn, want) {
		t.Fatalf("digest windows = %v, want %v", f.digestsOn, want)
	}
}

func TestStoreWithoutDigestsRefetchesEveryCachedDay(t *testing.T) {
	ctx := context.Background()
	f := newStoreFixture()
	f.set("2026-10-11", 1)
	f.set("2026-10-12", 2)
	u := f.upstream()
	u.digests = nil
	store := newUsageStore(u)
	store.warmDays = 0
	if _, err := store.rows(ctx, "2026-10-11", "2026-10-12"); err != nil {
		t.Fatal(err)
	}
	f.fetched()
	if changed, err := store.refresh(ctx, false); err != nil || changed {
		t.Fatalf("unchanged refresh = %v, %v", changed, err)
	}
	if got := f.fetched(); len(got) != 2 {
		t.Fatalf("refetched = %v, want every cached day", got)
	}
}

func TestStoreFetchFailureIsNotCached(t *testing.T) {
	ctx := context.Background()
	f := newStoreFixture()
	f.set("2026-10-12", 1)
	failure := errors.New("transport failed")
	f.fail = failure
	store := newFixtureStore(f, "2026-10-12", 0)
	if _, err := store.rows(ctx, "2026-10-12", "2026-10-12"); !errors.Is(err, failure) {
		t.Fatalf("failed fetch = %v, want %v", err, failure)
	}
	f.fail = nil
	buckets, err := store.rows(ctx, "2026-10-12", "2026-10-12")
	if err != nil || !reflect.DeepEqual(bucketInputs(buckets), [][]int{{1}}) {
		t.Fatalf("retry after failure = %v, %v", bucketInputs(buckets), err)
	}
}

func TestStoreRetriesTransientFetchFailures(t *testing.T) {
	ctx := context.Background()
	f := newStoreFixture()
	f.set("2026-10-11", 2)
	f.set("2026-10-12", 1)
	f.fail = &magpie.WebError{Err: &magpie.HTTPError{StatusCode: 503}}
	f.failures = 1
	store := newFixtureStore(f, "2026-10-12", 0)
	buckets, err := store.rows(ctx, "2026-10-12", "2026-10-12")
	if err != nil || !reflect.DeepEqual(bucketInputs(buckets), [][]int{{1}}) {
		t.Fatalf("rows after transient failure = %v, %v", bucketInputs(buckets), err)
	}
	if got := f.fetched(); len(got) != 2 {
		t.Fatalf("fetched = %v, want one retry", got)
	}

	rejected := &magpie.WebError{Err: &magpie.HTTPError{StatusCode: 400}}
	f.fail = rejected
	if _, err := store.rows(ctx, "2026-10-11", "2026-10-11"); !errors.Is(err, rejected) {
		t.Fatalf("rejected fetch = %v, want %v", err, rejected)
	}
	if got := f.fetched(); len(got) != 1 {
		t.Fatalf("rejected fetch retried: %v", got)
	}
}

func TestStoreSkipsDaysAfterToday(t *testing.T) {
	ctx := context.Background()
	f := newStoreFixture()
	f.set("2026-10-12", 1)
	store := newFixtureStore(f, "2026-10-12", 0)
	buckets, err := store.rows(ctx, "2026-10-12", "2026-12-31")
	if err != nil || !reflect.DeepEqual(bucketInputs(buckets), [][]int{{1}}) {
		t.Fatalf("rows = %v, %v", bucketInputs(buckets), err)
	}
	f.mu.Lock()
	digestsOn := slices.Clone(f.digestsOn)
	f.mu.Unlock()
	if want := []string{"2026-10-12..2026-10-12"}; !reflect.DeepEqual(digestsOn, want) {
		t.Fatalf("digests read for %v, want %v", digestsOn, want)
	}
}

type generationalUpstream struct {
	*sessionUpstream
	generation uint64
}

func (u *generationalUpstream) Generation() uint64 { return u.generation }

func TestServiceDropsCachesWhenMagpieClientIsReplaced(t *testing.T) {
	ctx := context.Background()
	f := newStoreFixture()
	f.set("2026-10-12", 1)
	sessionCalls := 0
	u := &generationalUpstream{sessionUpstream: f.upstream(), generation: 1}
	u.sessions = func(context.Context, int) (magpie.SessionsResponse, error) {
		sessionCalls++
		return magpie.SessionsResponse{}, nil
	}
	s := newService(queryTestDB(t), u, time.Local)
	s.store.warmDays = 0
	r := DateRange{From: "2026-10-12", To: "2026-10-12"}
	if got, err := s.Usage(ctx, UsageQuery{Range: r, GroupBy: GroupProject}); err != nil || got.Totals.Input != 1 {
		t.Fatalf("first Usage = %+v, %v", got, err)
	}
	f.set("2026-10-12", 42)
	if got, err := s.Usage(ctx, UsageQuery{Range: r, GroupBy: GroupProject}); err != nil || got.Totals.Input != 1 || sessionCalls != 1 {
		t.Fatalf("same client Usage = %+v, %v, session loads %d; want cached", got, err, sessionCalls)
	}
	u.generation = 2
	if got, err := s.Usage(ctx, UsageQuery{Range: r, GroupBy: GroupProject}); err != nil || got.Totals.Input != 42 || sessionCalls != 2 {
		t.Fatalf("replacement client Usage = %+v, %v, session loads %d; want fresh rows and sessions", got, err, sessionCalls)
	}
}
