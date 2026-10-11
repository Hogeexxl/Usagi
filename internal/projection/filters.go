package projection

import (
	"cmp"
	"context"
	"fmt"
	"slices"

	"github.com/Hogeexxl/Usagi/internal/query"
)

func (s *Service) FilterOptions(ctx context.Context) (FilterOptionsView, error) {
	result, err := s.query.FilterOptions(ctx)
	if err != nil {
		return FilterOptionsView{}, fmt.Errorf("projection: filter options: %w", err)
	}

	type projectKey struct {
		kind query.ProjectKind
		path string
	}
	projects := make(map[projectKey]query.ProjectRef, len(result.Projects)+2)
	for _, project := range result.Projects {
		key := projectKey{kind: project.Kind, path: project.Path}
		if _, ok := projects[key]; !ok {
			projects[key] = project
		}
	}
	projectlessKey := projectKey{kind: query.ProjectKindProjectless}
	unknownKey := projectKey{kind: query.ProjectKindUnknown}
	if _, ok := projects[projectlessKey]; !ok {
		projects[projectlessKey] = query.ProjectRef{Kind: query.ProjectKindProjectless}
	}
	if _, ok := projects[unknownKey]; !ok {
		projects[unknownKey] = query.ProjectRef{Kind: query.ProjectKindUnknown}
	}

	var concrete []query.ProjectRef
	for _, project := range projects {
		if project.Kind == query.ProjectKindProject {
			concrete = append(concrete, project)
		}
	}
	slices.SortFunc(concrete, func(a, b query.ProjectRef) int {
		return cmp.Compare(a.Path, b.Path)
	})
	orderedProjects := append(concrete, projects[projectlessKey], projects[unknownKey])
	return FilterOptionsView{
		Agents:   append([]query.AgentOption(nil), result.Agents...),
		Models:   append([]string(nil), result.Models...),
		Projects: orderedProjects,
	}, nil
}
