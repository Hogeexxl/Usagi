package projection

import (
	"context"

	"github.com/Hogeexxl/Usagi/internal/query"
)

func (s *Service) Summary(ctx context.Context, q ScopeQuery) (SummaryView, error) {
	list, err := s.Sessions(ctx, q)
	if err != nil {
		return SummaryView{}, err
	}
	return list.Summary, nil
}

func (s *Service) ToolUsage(ctx context.Context, days int) (ToolUsageView, error) {
	if days != 1 && days != 7 && days != 30 {
		return ToolUsageView{}, query.ErrInvalidFilter
	}
	overview, err := s.query.Overview(ctx, query.OverviewQuery{Days: days})
	if err != nil {
		return ToolUsageView{}, err
	}
	result := ToolUsageView{Days: days, Calls: int64(overview.Tools.Calls), Top: make([]ToolUsageItem, 0, len(overview.Tools.Top))}
	for _, tool := range overview.Tools.Top {
		result.Top = append(result.Top, ToolUsageItem{Name: tool.Name, Calls: int64(tool.Calls)})
	}
	return result, nil
}
