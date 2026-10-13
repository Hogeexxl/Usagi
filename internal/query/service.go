package query

import (
	"cmp"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"sync"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type upstream interface {
	Usage(context.Context, string) (magpie.UsageSummary, error)
	UsageRequests(context.Context, magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error)
	UsageDayDigests(context.Context, string, string) ([]magpie.UsageDayDigest, error)
	Providers(context.Context) (magpie.ProviderCatalogsResponse, error)
	Sessions(context.Context, int) (magpie.SessionsResponse, error)
	Session(context.Context, string) (magpie.Session, error)
	SessionStats(context.Context, int) (magpie.SessionStats, error)
	Quotas(context.Context, bool) (magpie.QuotasResponse, error)
	SessionOverview(context.Context, magpie.SessionOverviewQuery) (magpie.SessionOverview, error)
}

var ErrUpstreamUnavailable = errors.New("query: upstream unavailable")

// generational is implemented by upstreams whose Magpie client can be replaced.
type generational interface {
	Generation() uint64
}

type SessionRefreshFact struct {
	Agent string
	ID    string
	Title string
	Cwd   string
	Start time.Time
	Last  time.Time
}

type Service struct {
	upstream            upstream
	db                  *sql.DB
	location            *time.Location
	now                 func() time.Time
	store               *usageStore
	pricingHTTPClient   *http.Client
	snapshotMu          sync.RWMutex
	refreshMu           sync.Mutex
	syncMu              sync.Mutex
	snapshot            *sessionSnapshot
	loaded              bool
	facts               []SessionRefreshFact
	lookups             *sessionLookups
	generation          uint64
	usageFingerprint    [32]byte
	hasUsageFingerprint bool
	pendingRecent       map[string]struct{}
}

type sessionKey struct {
	Agent, ID string
}

type sessionSnapshot struct {
	sessions map[sessionKey]magpie.Session
}

func NewService(db *sql.DB, client *magpie.Client, location *time.Location) *Service {
	return newService(db, client, location)
}

func newService(db *sql.DB, client upstream, location *time.Location) *Service {
	service := &Service{
		upstream:          client,
		db:                db,
		location:          location,
		now:               time.Now,
		pendingRecent:     make(map[string]struct{}),
		store:             newUsageStore(client),
		pricingHTTPClient: &http.Client{Timeout: 30 * time.Second},
		snapshot:          &sessionSnapshot{sessions: make(map[sessionKey]magpie.Session)},
		lookups:           newSessionLookups(),
	}
	service.store.location = location
	return service
}

func (s *Service) Refresh(ctx context.Context) error {
	_, err := s.RefreshSessions(ctx)
	return err
}

// syncGeneration drops every cache built from a Magpie client the Runtime has
// since replaced.
func (s *Service) syncGeneration() {
	source, ok := s.upstream.(generational)
	if !ok {
		return
	}
	generation := source.Generation()
	s.snapshotMu.Lock()
	if generation == s.generation {
		s.snapshotMu.Unlock()
		return
	}
	s.generation = generation
	s.loaded = false
	s.facts = nil
	s.lookups = newSessionLookups()
	s.snapshotMu.Unlock()
	s.store.reset()
}

func (s *Service) RefreshSessions(ctx context.Context) ([]SessionRefreshFact, error) {
	s.syncGeneration()
	s.refreshMu.Lock()
	defer s.refreshMu.Unlock()

	response, err := s.upstream.Sessions(ctx, 0)
	if err != nil {
		return nil, fmt.Errorf("query: refresh sessions: %w", err)
	}
	snapshot := &sessionSnapshot{sessions: make(map[sessionKey]magpie.Session, len(response.Sessions))}
	facts := make([]SessionRefreshFact, 0, len(response.Sessions))
	for _, session := range response.Sessions {
		snapshot.sessions[sessionKey{Agent: session.Agent, ID: session.ID}] = session
		facts = append(facts, SessionRefreshFact{
			Agent: session.Agent, ID: session.ID, Title: session.Title,
			Cwd: session.Cwd, Start: session.Start, Last: session.Last,
		})
	}
	slices.SortFunc(facts, func(a, b SessionRefreshFact) int {
		if a.Agent != b.Agent {
			return cmp.Compare(a.Agent, b.Agent)
		}
		if a.ID != b.ID {
			return cmp.Compare(a.ID, b.ID)
		}
		if a.Title != b.Title {
			return cmp.Compare(a.Title, b.Title)
		}
		if a.Cwd != b.Cwd {
			return cmp.Compare(a.Cwd, b.Cwd)
		}
		if c := a.Start.Compare(b.Start); c != 0 {
			return c
		}
		return a.Last.Compare(b.Last)
	})
	s.snapshotMu.Lock()
	s.snapshot = snapshot
	s.loaded = true
	if !slices.EqualFunc(s.facts, facts, sessionFactEqual) {
		// Sessions that missed the old snapshot may now resolve differently.
		s.lookups = newSessionLookups()
	}
	s.facts = facts
	s.snapshotMu.Unlock()
	return facts, nil
}

func sessionFactEqual(a, b SessionRefreshFact) bool {
	return a.Agent == b.Agent && a.ID == b.ID && a.Title == b.Title && a.Cwd == b.Cwd &&
		a.Start.Equal(b.Start) && a.Last.Equal(b.Last)
}

// ensureSessions loads the session snapshot once; the session watcher keeps it
// current afterwards, so queries do not re-download every session.
func (s *Service) ensureSessions(ctx context.Context) error {
	s.syncGeneration()
	s.snapshotMu.RLock()
	loaded := s.loaded
	s.snapshotMu.RUnlock()
	if loaded {
		return nil
	}
	_, err := s.RefreshSessions(ctx)
	return err
}

// RefreshUsageRows brings cached request rows in line with Magpie and reports
// whether any cached row changed. forceRecent refetches today and yesterday
// even when their per-day digests did not move.
func (s *Service) RefreshUsageRows(ctx context.Context, forceRecent bool) (bool, error) {
	s.syncGeneration()
	return s.store.refresh(ctx, forceRecent)
}

func (s *Service) UsageSnapshot(ctx context.Context) (magpie.UsageSummary, error) {
	usage, err := s.upstream.Usage(ctx, "all")
	if err != nil {
		return magpie.UsageSummary{}, fmt.Errorf("query: usage snapshot: %w", err)
	}
	return usage, nil
}

func (s *Service) Quota(ctx context.Context, asked bool) (magpie.QuotasResponse, error) {
	quotas, err := s.upstream.Quotas(ctx, asked)
	if err != nil {
		return magpie.QuotasResponse{}, fmt.Errorf("query: quotas: %w", err)
	}
	return quotas, nil
}

// Skills 产品消费者固定调用一次 OverviewQuery{Days: 7}，Agent/Model/Cwd 为空，读取最近 7 天全局 Top Skills。
func (s *Service) Overview(ctx context.Context, q OverviewQuery) (magpie.SessionOverview, error) {
	if q.Days < 0 {
		return magpie.SessionOverview{}, ErrInvalidFilter
	}
	overview, err := s.upstream.SessionOverview(ctx, magpie.SessionOverviewQuery{
		Days: q.Days, Agent: q.Agent, Model: q.Model, Cwd: q.Cwd,
	})
	if err != nil {
		return magpie.SessionOverview{}, fmt.Errorf("query: overview: %w", err)
	}
	return overview, nil
}
