package projection

import (
	"cmp"
	"context"
	"fmt"
	"slices"

	"github.com/Hogeexxl/Usagi/internal/query"
)

func (s *Service) ModelDistribution(ctx context.Context, q ScopeQuery) (ModelDistributionView, error) {
	result, err := s.query.Usage(ctx, query.UsageQuery{
		Range: q.Range, Filter: q.Filter, GroupBy: query.GroupModel,
	})
	if err != nil {
		return ModelDistributionView{}, fmt.Errorf("projection: model distribution: %w", err)
	}
	view := ModelDistributionView{Items: make([]ModelDistributionItem, 0, len(result.Groups))}
	for _, group := range result.Groups {
		view.Items = append(view.Items, ModelDistributionItem{Model: group.Model, Usage: usageFromTotals(group.Totals)})
	}
	slices.SortFunc(view.Items, func(a, b ModelDistributionItem) int {
		if c := cmp.Compare(b.Usage.Total, a.Usage.Total); c != 0 {
			return c
		}
		return cmp.Compare(a.Model, b.Model)
	})
	return view, nil
}

func (s *Service) ProjectDistribution(ctx context.Context, q ScopeQuery) (ProjectDistributionView, error) {
	result, err := s.query.Usage(ctx, query.UsageQuery{
		Range: q.Range, Filter: q.Filter, GroupBy: query.GroupProject,
	})
	if err != nil {
		return ProjectDistributionView{}, fmt.Errorf("projection: project distribution: %w", err)
	}
	view := ProjectDistributionView{Items: make([]ProjectDistributionItem, 0, len(result.Groups))}
	for _, group := range result.Groups {
		view.Items = append(view.Items, ProjectDistributionItem{Project: group.Project, Usage: usageFromTotals(group.Totals)})
	}
	slices.SortFunc(view.Items, func(a, b ProjectDistributionItem) int {
		if c := cmp.Compare(b.Usage.Total, a.Usage.Total); c != 0 {
			return c
		}
		if c := cmp.Compare(a.Project.Kind, b.Project.Kind); c != 0 {
			return c
		}
		return cmp.Compare(a.Project.Path, b.Project.Path)
	})
	return view, nil
}
