package query

import (
	"context"
	"database/sql"
	"sync"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type ClientProvider interface {
	Client() (*magpie.Client, bool)
}

type runtimeUpstream struct {
	provider ClientProvider

	mu         sync.Mutex
	last       *magpie.Client
	generation uint64
}

func NewRuntimeService(db *sql.DB, provider ClientProvider, location *time.Location) *Service {
	return newService(db, &runtimeUpstream{provider: provider}, location)
}

func (u *runtimeUpstream) current() (*magpie.Client, error) {
	client, ready := u.provider.Client()
	if !ready || client == nil {
		return nil, ErrUpstreamUnavailable
	}
	u.observe(client)
	return client, nil
}

// Generation changes whenever the Runtime hands out a different Magpie client,
// so caches built from the previous client can be dropped.
func (u *runtimeUpstream) Generation() uint64 {
	if client, ready := u.provider.Client(); ready && client != nil {
		u.observe(client)
	}
	u.mu.Lock()
	defer u.mu.Unlock()
	return u.generation
}

func (u *runtimeUpstream) observe(client *magpie.Client) {
	u.mu.Lock()
	if client != u.last {
		u.last = client
		u.generation++
	}
	u.mu.Unlock()
}

func (u *runtimeUpstream) Usage(ctx context.Context, period string) (magpie.UsageSummary, error) {
	client, err := u.current()
	if err != nil {
		return magpie.UsageSummary{}, err
	}
	return client.Usage(ctx, period)
}

func (u *runtimeUpstream) UsageRequests(ctx context.Context, q magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
	client, err := u.current()
	if err != nil {
		return magpie.UsageRequestsPage{}, err
	}
	return client.UsageRequests(ctx, q)
}

func (u *runtimeUpstream) UsageDayDigests(ctx context.Context, from, to string) ([]magpie.UsageDayDigest, error) {
	client, err := u.current()
	if err != nil {
		return nil, err
	}
	return client.UsageDayDigests(ctx, from, to)
}

func (u *runtimeUpstream) Sessions(ctx context.Context, limit int) (magpie.SessionsResponse, error) {
	client, err := u.current()
	if err != nil {
		return magpie.SessionsResponse{}, err
	}
	return client.Sessions(ctx, limit)
}

func (u *runtimeUpstream) Session(ctx context.Context, key string) (magpie.Session, error) {
	client, err := u.current()
	if err != nil {
		return magpie.Session{}, err
	}
	return client.Session(ctx, key)
}

func (u *runtimeUpstream) SessionStats(ctx context.Context, days int) (magpie.SessionStats, error) {
	client, err := u.current()
	if err != nil {
		return magpie.SessionStats{}, err
	}
	return client.SessionStats(ctx, days)
}

func (u *runtimeUpstream) Quotas(ctx context.Context, asked bool) (magpie.QuotasResponse, error) {
	client, err := u.current()
	if err != nil {
		return magpie.QuotasResponse{}, err
	}
	return client.Quotas(ctx, asked)
}

func (u *runtimeUpstream) SessionOverview(ctx context.Context, q magpie.SessionOverviewQuery) (magpie.SessionOverview, error) {
	client, err := u.current()
	if err != nil {
		return magpie.SessionOverview{}, err
	}
	return client.SessionOverview(ctx, q)
}
