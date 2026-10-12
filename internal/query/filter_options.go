package query

import (
	"cmp"
	"context"
	"slices"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

// FilterOptions lists the agents, provider models and projects that appear in
// the request rows of r, so menus only offer choices the range can match.
func (s *Service) FilterOptions(ctx context.Context, r DateRange) (FilterOptionsResult, error) {
	if _, err := normalizeUsageInput(r, UsageFilter{}); err != nil {
		return FilterOptionsResult{}, err
	}
	if err := s.ensureSessions(ctx); err != nil {
		return FilterOptionsResult{}, err
	}
	agents := make(map[string]AgentOption)
	models := make(map[ModelSelection]ModelOption)
	projects := make(map[ProjectRef]struct{})
	err := s.runRequests(ctx, r, UsageFilter{}, s.newWorkspace(), func(row magpie.UsageRequest, _ SessionRef, _ magpie.Session, project ProjectRef) {
		if row.Agent != "" {
			if option, ok := agents[row.Agent]; !ok || option.DisplayName == row.Agent {
				agents[row.Agent] = AgentOption{Agent: row.Agent, DisplayName: cmp.Or(row.AgentName, row.Agent)}
			}
		}
		if row.Provider != "" {
			selection := ModelSelection{ProviderID: row.Provider, Model: ModelName(row.Model)}
			if option, ok := models[selection]; !ok || option.ProviderName == row.Provider {
				models[selection] = ModelOption{
					Model: selection.Model, ProviderID: row.Provider, ProviderName: cmp.Or(row.ProviderName, row.Provider),
				}
			}
		}
		projects[project] = struct{}{}
	})
	if err != nil {
		return FilterOptionsResult{}, err
	}
	var result FilterOptionsResult
	for _, agent := range agents {
		result.Agents = append(result.Agents, agent)
	}
	for _, model := range models {
		result.Models = append(result.Models, model)
	}
	for project := range projects {
		result.Projects = append(result.Projects, project)
	}
	slices.SortFunc(result.Agents, func(a, b AgentOption) int { return cmp.Compare(a.Agent, b.Agent) })
	slices.SortFunc(result.Models, func(a, b ModelOption) int {
		if c := cmp.Compare(a.ProviderID, b.ProviderID); c != 0 {
			return c
		}
		return cmp.Compare(a.Model, b.Model)
	})
	slices.SortFunc(result.Projects, func(a, b ProjectRef) int {
		if c := cmp.Compare(a.Kind, b.Kind); c != 0 {
			return c
		}
		return cmp.Compare(a.Path, b.Path)
	})
	return result, nil
}
