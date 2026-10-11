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
	Usage(context.Context, query.UsageQuery) (query.UsageResult, error)
	UsageRows(context.Context, query.UsageRowsQuery) (query.UsageRowsResult, error)
	Sessions(context.Context, query.SessionQuery) (query.SessionResult, error)
	FilterOptions(context.Context) (query.FilterOptionsResult, error)
	Quota(context.Context, bool) (magpie.QuotasResponse, error)
}

func NewService(q *query.Service) *Service {
	return newService(q)
}

func newService(q queryAPI) *Service {
	return &Service{query: q}
}
