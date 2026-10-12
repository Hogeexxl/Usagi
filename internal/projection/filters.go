package projection

import (
	"cmp"
	"context"
	"fmt"
	"slices"

	"github.com/Hogeexxl/Usagi/internal/query"
)

// FilterOptions lists the filter choices present in q.Range. Concrete projects
// come first by path, then projectless and unknown sessions when present.
func (s *Service) FilterOptions(ctx context.Context, q ScopeQuery) (FilterOptionsView, error) {
	result, err := s.query.FilterOptions(ctx, q.Range)
	if err != nil {
		return FilterOptionsView{}, fmt.Errorf("projection: filter options: %w", err)
	}
	projects := slices.Clone(result.Projects)
	slices.SortFunc(projects, func(a, b query.ProjectRef) int {
		if c := cmp.Compare(projectKindOrder(a.Kind), projectKindOrder(b.Kind)); c != 0 {
			return c
		}
		return cmp.Compare(a.Path, b.Path)
	})
	projects = slices.CompactFunc(projects, func(a, b query.ProjectRef) bool {
		return a.Kind == b.Kind && a.Path == b.Path
	})
	return FilterOptionsView{
		Agents:   append([]query.AgentOption(nil), result.Agents...),
		Models:   append([]query.ModelOption(nil), result.Models...),
		Projects: projects,
	}, nil
}

func projectKindOrder(kind query.ProjectKind) int {
	switch kind {
	case query.ProjectKindProject:
		return 0
	case query.ProjectKindProjectless:
		return 1
	default:
		return 2
	}
}
