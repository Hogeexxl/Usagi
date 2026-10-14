package projection

import (
	"cmp"
	"context"
	"slices"

	"github.com/Hogeexxl/Usagi/internal/query"
)

func (s *Service) ModelDistribution(ctx context.Context, q ScopeQuery) (ModelDistributionView, error) {
	dataset, err := s.query.SessionDataset(ctx, query.SessionQuery{Range: q.Range, Filter: q.Filter})
	if err != nil {
		return ModelDistributionView{}, err
	}
	groups := make(map[string]*usageAccumulator)
	for _, row := range dataset.Rows {
		model := query.ModelName(row.Model)
		if groups[model] == nil {
			groups[model] = &usageAccumulator{}
		}
		groups[model].AddRow(row)
	}
	view := ModelDistributionView{Items: make([]ModelDistributionItem, 0, len(groups))}
	for model, usage := range groups {
		view.Items = append(view.Items, ModelDistributionItem{Model: model, Usage: usage.View()})
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
	dataset, err := s.query.SessionDataset(ctx, query.SessionQuery{Range: q.Range, Filter: q.Filter})
	if err != nil {
		return ProjectDistributionView{}, err
	}
	type projectKey struct {
		kind query.ProjectKind
		path string
	}
	groups := make(map[projectKey]*usageAccumulator)
	projects := make(map[projectKey]query.ProjectRef)
	for _, row := range dataset.Rows {
		key := projectKey{kind: row.Project.Kind}
		if row.Project.Kind == query.ProjectKindProject {
			key.path = row.Project.Path
		}
		if groups[key] == nil {
			groups[key] = &usageAccumulator{}
			if row.Project.Kind == query.ProjectKindProject {
				projects[key] = row.Project
			} else {
				projects[key] = query.ProjectRef{Kind: row.Project.Kind}
			}
		}
		groups[key].AddRow(row)
	}
	view := ProjectDistributionView{Items: make([]ProjectDistributionItem, 0, len(groups))}
	for key, usage := range groups {
		view.Items = append(view.Items, ProjectDistributionItem{Project: projects[key], Usage: usage.View()})
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
