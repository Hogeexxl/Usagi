package api

import (
	"context"
	"log/slog"
	"sync"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/projection"
)

type watcherQuotaService interface {
	Quotas(context.Context, bool) (projection.QuotaView, error)
}

type QuotaSnapshot struct {
	mu     sync.RWMutex
	view   projection.QuotaView
	loaded bool
	err    error
}

func (s *QuotaSnapshot) Current() (projection.QuotaView, bool, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.view, s.loaded, s.err
}

func (s *QuotaSnapshot) Replace(view projection.QuotaView) {
	s.mu.Lock()
	s.view = view
	s.loaded = true
	s.err = nil
	s.mu.Unlock()
}

func (s *QuotaSnapshot) RecordError(err error) {
	s.mu.Lock()
	s.err = err
	s.mu.Unlock()
}

type watchers struct {
	query         WatcherQuery
	quotaService  watcherQuotaService
	revisions     RevisionService
	quotaSnapshot *QuotaSnapshot
	usageInterval time.Duration
	quotaInterval time.Duration
	usageMu       sync.Mutex
}

const quotaReadingPollInterval = 60 * time.Second

func newWatchers(queryService WatcherQuery, quotaService watcherQuotaService, revisions RevisionService, quotaSnapshot *QuotaSnapshot, usageInterval, quotaInterval time.Duration) *watchers {
	return &watchers{
		query:         queryService,
		quotaService:  quotaService,
		revisions:     revisions,
		quotaSnapshot: quotaSnapshot,
		usageInterval: usageInterval,
		quotaInterval: quotaInterval,
	}
}

func (w *watchers) start(ctx context.Context, runtime RuntimeStatusSource) <-chan struct{} {
	done := make(chan struct{})
	go func() {
		var workers sync.WaitGroup
		workers.Add(2)
		go func() {
			defer workers.Done()
			w.runSync(ctx, runtime)
		}()
		go func() {
			defer workers.Done()
			w.runQuota(ctx, runtime, quotaReadingPollInterval)
		}()
		workers.Wait()
		close(done)
	}()
	return done
}

func (w *watchers) run(ctx context.Context, interval time.Duration, refresh func(context.Context)) {
	refresh(ctx)
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			refresh(ctx)
		}
	}
}

// runSync owns the single synchronization loop. It runs immediately, on the
// interval, and when Magpie becomes ready again.
func (w *watchers) runSync(ctx context.Context, runtime RuntimeStatusSource) {
	var statuses <-chan magpie.RuntimeStatus
	if runtime != nil {
		subscription, cancel := runtime.Subscribe()
		defer cancel()
		statuses = subscription
	}
	w.refreshUsage(ctx)
	ticker := time.NewTicker(w.usageInterval)
	defer ticker.Stop()
	var previous magpie.RuntimeStatus
	seenStatus := false
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			w.refreshUsage(ctx)
		case status, ok := <-statuses:
			if !ok {
				statuses = nil
				continue
			}
			if seenStatus && status == magpie.RuntimeStatusReady && previous != magpie.RuntimeStatusReady {
				w.refreshUsage(ctx)
			}
			previous = status
			seenStatus = true
		}
	}
}

// refreshUsage publishes the revision returned by the persistent sync even
// when another day's fetch failed after earlier days committed.
func (w *watchers) refreshUsage(ctx context.Context) {
	w.usageMu.Lock()
	defer w.usageMu.Unlock()
	revision, err := w.query.Sync(ctx)
	w.revisions.SetDataRevision(revision)
	if err != nil && ctx.Err() == nil {
		slog.Error("synchronize local usage database", "error", err)
	}
}

func (w *watchers) runQuota(ctx context.Context, runtime RuntimeStatusSource, readingInterval time.Duration) {
	statuses, cancel := runtime.Subscribe()
	defer cancel()

	var status magpie.RuntimeStatus
	var hasStatus bool
	var timer *time.Timer
	var timerC <-chan time.Time
	stopTimer := func() {
		if timer != nil && !timer.Stop() {
			select {
			case <-timer.C:
			default:
			}
		}
		timerC = nil
	}
	defer stopTimer()
	schedule := func(interval time.Duration) {
		if timer == nil {
			timer = time.NewTimer(interval)
		} else {
			stopTimer()
			timer.Reset(interval)
		}
		timerC = timer.C
	}

	for {
		select {
		case <-ctx.Done():
			return
		case state, ok := <-statuses:
			if !ok {
				return
			}
			if hasStatus && state == status {
				continue
			}
			status = state
			hasStatus = true
			if state == magpie.RuntimeStatusReady {
				schedule(0)
			} else {
				stopTimer()
			}
		case <-timerC:
			timerC = nil
			if !hasStatus || status != magpie.RuntimeStatusReady {
				continue
			}
			if w.refreshQuota(ctx) {
				schedule(readingInterval)
			} else {
				schedule(w.quotaInterval)
			}
		}
	}
}

func (w *watchers) refreshQuota(ctx context.Context) bool {
	view, err := w.quotaService.Quotas(ctx, false)
	if err != nil {
		if ctx.Err() == nil {
			w.quotaSnapshot.RecordError(err)
		}
		view, loaded, _ := w.quotaSnapshot.Current()
		return !loaded || view.Reading
	}
	w.quotaSnapshot.Replace(view)
	return view.Reading
}
