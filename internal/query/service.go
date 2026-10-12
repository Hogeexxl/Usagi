package query

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"slices"
	"sync"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type upstream interface {
	Usage(context.Context, string) (magpie.UsageSummary, error)
	UsageRequests(context.Context, magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error)
	Sessions(context.Context, int) (magpie.SessionsResponse, error)
	Session(context.Context, string) (magpie.Session, error)
	SessionStats(context.Context, int) (magpie.SessionStats, error)
	Quotas(context.Context, bool) (magpie.QuotasResponse, error)
	SessionOverview(context.Context, magpie.SessionOverviewQuery) (magpie.SessionOverview, error)
}

var ErrUpstreamUnavailable = errors.New("query: upstream unavailable")

type SessionRefreshFact struct {
	Agent string
	ID    string
	Title string
	Cwd   string
	Start time.Time
	Last  time.Time
}

type Service struct {
	upstream   upstream
	snapshotMu sync.RWMutex
	refreshMu  sync.Mutex
	snapshot   *sessionSnapshot
}

type sessionKey struct {
	Agent, ID string
}

type sessionSnapshot struct {
	sessions map[sessionKey]magpie.Session
}

func NewService(client *magpie.Client) *Service {
	return newService(client)
}

func newService(client upstream) *Service {
	return &Service{upstream: client, snapshot: &sessionSnapshot{sessions: make(map[sessionKey]magpie.Session)}}
}

func (s *Service) Refresh(ctx context.Context) error {
	_, err := s.RefreshSessions(ctx)
	return err
}

func (s *Service) RefreshSessions(ctx context.Context) ([]SessionRefreshFact, error) {
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
	s.snapshotMu.Unlock()
	return facts, nil
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
