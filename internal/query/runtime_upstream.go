package query

import (
	"context"
	"database/sql"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type ClientProvider interface {
	Client() (*magpie.Client, bool)
}

type runtimeUpstream struct {
	provider ClientProvider
}

func NewRuntimeService(db *sql.DB, provider ClientProvider, location *time.Location) *Service {
	return newService(db, &runtimeUpstream{provider: provider}, location)
}

func (u *runtimeUpstream) current() (*magpie.Client, error) {
	client, ready := u.provider.Client()
	if !ready || client == nil {
		return nil, ErrUpstreamUnavailable
	}
	return client, nil
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
