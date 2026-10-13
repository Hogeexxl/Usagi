package query

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/http"
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
	Quotas(context.Context, bool) (magpie.QuotasResponse, error)
	SessionOverview(context.Context, magpie.SessionOverviewQuery) (magpie.SessionOverview, error)
}

var ErrUpstreamUnavailable = errors.New("query: upstream unavailable")

type Service struct {
	upstream            upstream
	db                  *sql.DB
	location            *time.Location
	now                 func() time.Time
	pricingHTTPClient   *http.Client
	syncMu              sync.Mutex
	usageFingerprint    [32]byte
	hasUsageFingerprint bool
	pendingRecent       map[string]struct{}
}

type sessionKey struct {
	Agent, ID string
}

func NewService(db *sql.DB, client *magpie.Client, location *time.Location) *Service {
	return newService(db, client, location)
}

func newService(db *sql.DB, client upstream, location *time.Location) *Service {
	return &Service{
		upstream:          client,
		db:                db,
		location:          location,
		now:               time.Now,
		pendingRecent:     make(map[string]struct{}),
		pricingHTTPClient: &http.Client{Timeout: 30 * time.Second},
	}
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
