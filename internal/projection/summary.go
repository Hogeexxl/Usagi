package projection

import (
	"context"
	"fmt"

	"github.com/Hogeexxl/Usagi/internal/query"
)

func (s *Service) Summary(ctx context.Context, q ScopeQuery) (SummaryView, error) {
	usage, err := s.query.Usage(ctx, query.UsageQuery{
		Range: q.Range, Filter: q.Filter, GroupBy: query.GroupNone,
	})
	if err != nil {
		return SummaryView{}, fmt.Errorf("projection: summary usage: %w", err)
	}
	threads, err := s.loadThreadProjection(ctx, q)
	if err != nil {
		return SummaryView{}, err
	}
	return SummaryView{Usage: usageFromTotals(usage.Totals), SessionCount: int64(len(threads.Threads))}, nil
}
