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
	result := SummaryView{Usage: usageFromTotals(usage.Totals)}
	for index, thread := range threads.Threads {
		if thread.Type != ThreadMain {
			continue
		}
		result.RootSessionCount++
		if threads.inclusiveRollup(index).inclusiveUsage.Unpriced > 0 {
			result.RootUnpricedSessionCount++
		}
	}
	return result, nil
}
