package query

import (
	"cmp"
	"context"
	"slices"
)

// FilterOptions lists the agents, provider models and projects that appear in
// the request rows of r, so menus only offer choices the range can match.
func (s *Service) FilterOptions(ctx context.Context, r DateRange) (FilterOptionsResult, error) {
	if _, err := normalizeUsageInput(r, UsageFilter{}); err != nil {
		return FilterOptionsResult{}, err
	}
	dataset, err := s.SessionDataset(ctx, SessionQuery{Range: r})
	if err != nil {
		return FilterOptionsResult{}, err
	}
	agents := make(map[string]AgentOption)
	models := make(map[ModelSelection]ModelOption)
	projects := make(map[ProjectRef]struct{})
	for _, session := range dataset.Sessions {
		if session.Agent != "" {
			if _, ok := agents[session.Agent]; !ok {
				agents[session.Agent] = AgentOption{Agent: session.Agent, DisplayName: session.Agent}
			}
		}
		projects[session.Project] = struct{}{}
	}
	for _, row := range dataset.Rows {
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
