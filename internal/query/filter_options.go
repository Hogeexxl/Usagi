package query

import (
	"cmp"
	"context"
	"fmt"
	"slices"
	"strings"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

func (s *Service) FilterOptions(ctx context.Context) (FilterOptionsResult, error) {
	page, err := s.upstream.UsageRequests(ctx, magpie.UsageRequestsQuery{Period: "all", Offset: 0, Limit: 1})
	if err != nil {
		return FilterOptionsResult{}, fmt.Errorf("query: filter options requests: %w", err)
	}
	stats, err := s.upstream.SessionStats(ctx, 0)
	if err != nil {
		return FilterOptionsResult{}, fmt.Errorf("query: filter options stats: %w", err)
	}
	agents := make(map[string]AgentOption)
	for _, agent := range page.Agents {
		if _, ok := agents[agent.ID]; !ok {
			agents[agent.ID] = AgentOption{Agent: agent.ID, DisplayName: agent.Name}
		}
	}
	providers := make(map[string]string, len(page.Providers))
	for _, provider := range page.Providers {
		if _, ok := providers[provider.ID]; !ok {
			providers[provider.ID] = provider.Name
		}
	}
	models := make(map[ModelSelection]ModelOption)
	for _, model := range page.By["modelAt"] {
		providerID, modelID, ok := strings.Cut(model.ID, "/")
		if !ok || providerID == "" || modelID == "" {
			continue
		}
		selection := ModelSelection{ProviderID: providerID, Model: modelID}
		if _, ok := models[selection]; !ok {
			providerName := providers[providerID]
			if providerName == "" {
				providerName = providerID
			}
			models[selection] = ModelOption{
				Model: modelID, ProviderID: providerID, ProviderName: providerName,
			}
		}
	}
	projects := make(map[string]ProjectRef)
	for _, day := range stats.Days {
		for _, usage := range day.Usage {
			if usage.Cwd != "" {
				projects[usage.Cwd] = projectOf(magpie.Session{Cwd: usage.Cwd}, true)
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
	for _, project := range projects {
		result.Projects = append(result.Projects, project)
	}
	slices.SortFunc(result.Agents, func(a, b AgentOption) int { return cmp.Compare(a.Agent, b.Agent) })
	slices.SortFunc(result.Models, func(a, b ModelOption) int {
		if c := cmp.Compare(a.ProviderID, b.ProviderID); c != 0 {
			return c
		}
		return cmp.Compare(a.Model, b.Model)
	})
	slices.SortFunc(result.Projects, func(a, b ProjectRef) int { return cmp.Compare(a.Path, b.Path) })
	return result, nil
}
