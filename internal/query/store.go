package query

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"sync"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

const (
	// Magpie caps request pages at 500 rows and re-aggregates the whole period
	// for every page, so rows are fetched one local day at a time.
	storePageLimit = 500
	// Magpie only reports a per-day series for windows up to 60 days.
	storeDigestChunkDays = 60
	storeFetchWorkers    = 4
	storeFetchAttempts   = 3
	storeFetchTimeout    = 2 * time.Minute
	storeRetryDelay      = 500 * time.Millisecond
	// The warm window always stays cached; other days are evicted once idle.
	storeWarmDays = 30
	storeIdleTTL  = 30 * time.Minute
)

// dayBucket holds every Magpie request row of one local day. Buckets are
// immutable once published; a refresh swaps in a new bucket.
type dayBucket struct {
	rows   []magpie.UsageRequest
	digest [32]byte
}

type dayFetch struct {
	done   chan struct{}
	bucket *dayBucket
	err    error
}

type dayDigest struct {
	digest [32]byte
	empty  bool
}

// usageStore caches Magpie request rows by local day and keeps them in step
// with Magpie by comparing per-day series digests.
type usageStore struct {
	upstream upstream
	now      func() time.Time
	location *time.Location
	warmDays int
	slots    chan struct{}

	mu       sync.Mutex
	epoch    uint64
	days     map[string]*dayBucket
	lastUsed map[string]time.Time
	inflight map[string]*dayFetch
}

func newUsageStore(client upstream) *usageStore {
	return &usageStore{
		upstream: client,
		now:      time.Now,
		location: time.Local,
		warmDays: storeWarmDays,
		slots:    make(chan struct{}, storeFetchWorkers),
		days:     make(map[string]*dayBucket),
		lastUsed: make(map[string]time.Time),
		inflight: make(map[string]*dayFetch),
	}
}

// reset drops every bucket. Fetches already running finish for their waiters
// but are no longer published.
func (s *usageStore) reset() {
	s.mu.Lock()
	s.epoch++
	s.days = make(map[string]*dayBucket)
	s.lastUsed = make(map[string]time.Time)
	s.inflight = make(map[string]*dayFetch)
	s.mu.Unlock()
}

// rows returns the buckets for [from, to], newest day first, fetching any day
// that is not cached yet.
func (s *usageStore) rows(ctx context.Context, from, to string) ([]*dayBucket, error) {
	days, err := daysBetween(from, to)
	if err != nil {
		return nil, err
	}
	s.mu.Lock()
	now := s.now()
	// Days after today hold no rows yet, so ranges such as "year" stop at today.
	today := now.Format(time.DateOnly)
	days = slices.DeleteFunc(days, func(day string) bool { return day > today })
	slices.Reverse(days)
	buckets := make([]*dayBucket, len(days))
	var missing []string
	for i, day := range days {
		s.lastUsed[day] = now
		if bucket, ok := s.days[day]; ok {
			buckets[i] = bucket
		} else {
			missing = append(missing, day)
		}
	}
	s.mu.Unlock()
	if len(missing) == 0 {
		return buckets, nil
	}
	fetched, err := s.fetch(ctx, missing)
	if err != nil {
		return nil, err
	}
	for i, day := range days {
		if buckets[i] == nil {
			buckets[i] = fetched[day]
		}
	}
	return buckets, nil
}

// refresh keeps the warm window loaded, evicts idle days and refetches every
// cached day whose Magpie digest moved. forceRecent also refetches today and
// yesterday, for row changes the per-day series cannot see. It reports whether
// any cached rows changed.
func (s *usageStore) refresh(ctx context.Context, forceRecent bool) (bool, error) {
	now := s.now()
	today := now.Format(time.DateOnly)
	warm := make(map[string]bool, s.warmDays)
	for i := range s.warmDays {
		warm[now.AddDate(0, 0, -i).Format(time.DateOnly)] = true
	}
	s.mu.Lock()
	for day := range s.days {
		if !warm[day] && now.Sub(s.lastUsed[day]) > storeIdleTTL {
			delete(s.days, day)
			delete(s.lastUsed, day)
		}
	}
	cached := make(map[string]*dayBucket, len(s.days))
	for day, bucket := range s.days {
		cached[day] = bucket
	}
	s.mu.Unlock()

	digests, verified, err := s.digests(ctx, mapKeys(cached))
	if err != nil {
		return false, err
	}
	var targets []string
	for day, bucket := range cached {
		// Without digests no day can be proven unchanged, so all are refetched.
		stale := !verified || digests[day].digest != bucket.digest
		recent := forceRecent && (day == today || day == now.AddDate(0, 0, -1).Format(time.DateOnly))
		if stale || recent {
			targets = append(targets, day)
		}
	}
	for day := range warm {
		if _, ok := cached[day]; !ok {
			targets = append(targets, day)
		}
	}
	if len(targets) == 0 {
		return false, nil
	}
	fetched, err := s.fetch(ctx, targets)
	if err != nil {
		return false, err
	}
	changed := false
	for day, bucket := range fetched {
		if previous, ok := cached[day]; ok && !slices.Equal(previous.rows, bucket.rows) {
			changed = true
		}
	}
	return changed, nil
}

// fetch loads fresh buckets for days, sharing in-flight loads with concurrent
// callers. Loads keep running when ctx ends so other waiters still benefit.
func (s *usageStore) fetch(ctx context.Context, days []string) (map[string]*dayBucket, error) {
	digests, _, err := s.digests(ctx, days)
	if err != nil {
		return nil, err
	}
	fetches := make(map[string]*dayFetch, len(days))
	for _, day := range days {
		fetches[day] = s.start(day, digests[day])
	}
	buckets := make(map[string]*dayBucket, len(days))
	for day, fetch := range fetches {
		select {
		case <-fetch.done:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
		if fetch.err != nil {
			return nil, fetch.err
		}
		buckets[day] = fetch.bucket
	}
	return buckets, nil
}

func (s *usageStore) start(day string, digest dayDigest) *dayFetch {
	s.mu.Lock()
	defer s.mu.Unlock()
	if fetch, ok := s.inflight[day]; ok {
		return fetch
	}
	fetch := &dayFetch{done: make(chan struct{})}
	if digest.empty {
		fetch.bucket = &dayBucket{digest: digest.digest}
		s.days[day] = fetch.bucket
		close(fetch.done)
		return fetch
	}
	s.inflight[day] = fetch
	epoch := s.epoch
	go func() {
		fetch.err = errDayFetchAborted
		// Deferred so waiters are released even if the fetch never returns.
		defer func() {
			s.mu.Lock()
			if s.inflight[day] == fetch {
				delete(s.inflight, day)
			}
			if fetch.err == nil && s.epoch == epoch {
				s.days[day] = fetch.bucket
			}
			s.mu.Unlock()
			close(fetch.done)
		}()
		s.slots <- struct{}{}
		defer func() { <-s.slots }()
		ctx, cancel := context.WithTimeout(context.Background(), storeFetchTimeout)
		defer cancel()
		fetch.bucket, fetch.err = s.fetchDay(ctx, day, digest.digest)
	}()
	return fetch
}

var errDayFetchAborted = errors.New("query: usage day fetch aborted")

// fetchDay pages through one day. Rows landing mid-scan shift offsets, so a
// scan whose total moved is retried, as is a scan hit by a transient error.
func (s *usageStore) fetchDay(ctx context.Context, day string, digest [32]byte) (*dayBucket, error) {
	for attempt := 1; ; attempt++ {
		q := magpie.UsageRequestsQuery{Period: day + ".." + day, Limit: storePageLimit}
		var rows []magpie.UsageRequest
		total, stable := -1, true
		var err error
		for {
			var page magpie.UsageRequestsPage
			page, err = s.upstream.UsageRequests(ctx, q)
			if err != nil {
				break
			}
			if total >= 0 && page.Total != total {
				stable = false
			}
			total = page.Total
			rows = append(rows, page.Rows...)
			q.Offset += len(page.Rows)
			if q.Offset >= page.Total || len(page.Rows) == 0 {
				break
			}
		}
		if err != nil {
			if attempt == storeFetchAttempts || !isTransientFetchError(err) {
				return nil, fmt.Errorf("query: usage requests %s: %w", day, err)
			}
			select {
			case <-time.After(storeRetryDelay * time.Duration(attempt)):
				continue
			case <-ctx.Done():
				return nil, fmt.Errorf("query: usage requests %s: %w", day, err)
			}
		}
		if stable || attempt == storeFetchAttempts {
			return &dayBucket{rows: rows, digest: digest}, nil
		}
	}
}

// isTransientFetchError reports whether a failed Magpie request is worth
// retrying: network failures and server errors, not rejected requests.
func isTransientFetchError(err error) bool {
	if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return false
	}
	var httpErr *magpie.HTTPError
	if errors.As(err, &httpErr) {
		return httpErr.StatusCode >= http.StatusInternalServerError
	}
	var webErr *magpie.WebError
	return errors.As(err, &webErr)
}

// digests reads Magpie's per-day digests for days, in windows Magpie still
// buckets by day. Days absent from the series get a zero digest and are
// fetched. verified is false when Magpie offers no per-day series at all.
func (s *usageStore) digests(ctx context.Context, days []string) (map[string]dayDigest, bool, error) {
	result := make(map[string]dayDigest, len(days))
	if len(days) == 0 {
		return result, true, nil
	}
	sorted := slices.Clone(days)
	slices.Sort(sorted)
	for start := 0; start < len(sorted); {
		first, _ := time.Parse(time.DateOnly, sorted[start])
		limit := first.AddDate(0, 0, storeDigestChunkDays-1).Format(time.DateOnly)
		end := start
		for end+1 < len(sorted) && sorted[end+1] <= limit {
			end++
		}
		values, err := s.upstream.UsageDayDigests(ctx, sorted[start], sorted[end])
		if errors.Is(err, magpie.ErrDayDigestsUnavailable) {
			return make(map[string]dayDigest), false, nil
		}
		if err != nil {
			return nil, false, fmt.Errorf("query: usage digests: %w", err)
		}
		for _, value := range values {
			day := value.Day
			if !value.Time.IsZero() {
				day = value.Time.In(s.location).Format(time.DateOnly)
			}
			result[day] = dayDigest{digest: value.Digest, empty: value.Calls == 0}
		}
		start = end + 1
	}
	return result, true, nil
}

func daysBetween(from, to string) ([]string, error) {
	start, err := time.Parse(time.DateOnly, from)
	if err != nil {
		return nil, ErrInvalidRange
	}
	end, err := time.Parse(time.DateOnly, to)
	if err != nil || start.After(end) {
		return nil, ErrInvalidRange
	}
	var days []string
	for day := start; !day.After(end); day = day.AddDate(0, 0, 1) {
		days = append(days, day.Format(time.DateOnly))
	}
	return days, nil
}

func mapKeys[V any](values map[string]V) []string {
	keys := make([]string, 0, len(values))
	for key := range values {
		keys = append(keys, key)
	}
	return keys
}
