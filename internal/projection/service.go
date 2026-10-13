package projection

import (
	"context"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/query"
)

type Service struct {
	query queryAPI
}

type queryAPI interface {
	SessionDataset(context.Context, query.SessionQuery) (query.SessionDatasetResult, error)
	FilterOptions(context.Context, query.DateRange) (query.FilterOptionsResult, error)
	Quota(context.Context, bool) (magpie.QuotasResponse, error)
	Overview(context.Context, query.OverviewQuery) (magpie.SessionOverview, error)
}

func NewService(q *query.Service) *Service {
	return newService(q)
}

func newService(q queryAPI) *Service {
	return &Service{query: q}
}
