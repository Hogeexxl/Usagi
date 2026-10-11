package query

import (
	"context"
	"errors"
	"reflect"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

func TestQuota(t *testing.T) {
	responses := []magpie.QuotasResponse{
		{Reading: true, Quotas: []magpie.Quota{{Provider: "p", Name: "Provider", Plan: "first", Windows: []magpie.QuotaWindow{{Name: "5h", Used: 0.25}}, Resets: &magpie.ResetCredits{Count: 1}}}},
		{Quotas: []magpie.Quota{{Provider: "p", Plan: "second", Windows: []magpie.QuotaWindow{{Name: "5h", Used: 0.75}}, Resets: &magpie.ResetCredits{Count: 2}}}},
	}
	calls := 0
	u := &sessionUpstream{quotas: func(_ context.Context, asked bool) (magpie.QuotasResponse, error) {
		if asked != (calls == 0) {
			t.Fatalf("asked = %v on call %d", asked, calls)
		}
		response := responses[calls]
		calls++
		return response, nil
	}}
	s := newService(u)
	for i, want := range responses {
		got, err := s.Quota(context.Background(), i == 0)
		if err != nil || !reflect.DeepEqual(got, want) {
			t.Fatalf("Quota call %d = %+v, %v; want %+v", i, got, err, want)
		}
	}
	if calls != 2 {
		t.Fatalf("quota calls = %d, want 2", calls)
	}
}

func TestOverview(t *testing.T) {
	responses := []magpie.SessionOverview{
		{Count: 1, Tools: magpie.ToolUse{Calls: 3}, Skills: magpie.SkillUse{Calls: 4, Count: 1, Top: []magpie.Skill{{Name: "review", Calls: 4, Sessions: 2, Last: "2026-10-11", Agents: map[string]int{"codex": 4}, Projects: []magpie.ToolStat{{Name: "Usagi", Calls: 4, Sessions: 2}}}}}},
		{Count: 2, Median: 9, Skills: magpie.SkillUse{Calls: 8, Top: []magpie.Skill{{Name: "build", Calls: 8}}}},
	}
	var queries []magpie.SessionOverviewQuery
	u := &sessionUpstream{overview: func(_ context.Context, q magpie.SessionOverviewQuery) (magpie.SessionOverview, error) {
		response := responses[len(queries)]
		queries = append(queries, q)
		return response, nil
	}}
	s := newService(u)
	if _, err := s.Overview(context.Background(), OverviewQuery{Days: -1}); !errors.Is(err, ErrInvalidFilter) || len(queries) != 0 {
		t.Fatalf("negative Days = %v, upstream calls %d", err, len(queries))
	}
	// Skills 产品只执行这一次最近 7 天、全局、无 Dashboard Filter 的查询。
	got, err := s.Overview(context.Background(), OverviewQuery{Days: 7})
	if err != nil || !reflect.DeepEqual(got, responses[0]) || !reflect.DeepEqual(queries, []magpie.SessionOverviewQuery{{Days: 7}}) {
		t.Fatalf("Skills Overview = %+v, %v, queries %+v", got, err, queries)
	}
	q := OverviewQuery{Days: 0, Agent: "claude", Model: "m", Cwd: "/project"}
	got, err = s.Overview(context.Background(), q)
	wantQueries := []magpie.SessionOverviewQuery{{Days: 7}, {Days: 0, Agent: "claude", Model: "m", Cwd: "/project"}}
	if err != nil || !reflect.DeepEqual(got, responses[1]) || !reflect.DeepEqual(queries, wantQueries) {
		t.Fatalf("generic Overview = %+v, %v, queries %+v", got, err, queries)
	}
}

func TestFilterOptions(t *testing.T) {
	const cwd = "/workspace/notes/../Usagi/"
	pages := []magpie.UsageRequestsPage{
		{Total: 5000, Rows: []magpie.UsageRequest{{Agent: "row-only", Model: "row-only"}}, Agents: []magpie.UsageNamed{{ID: "z", Name: "Z"}, {ID: "a", Name: "A"}, {ID: "z", Name: "Z"}}, By: map[string][]magpie.UsageShare{"model": {{ID: "z"}, {ID: ""}, {ID: "a"}, {ID: "z"}}}},
		{Total: 9000, Rows: []magpie.UsageRequest{{Agent: "other-row", Model: "other-row"}}, Agents: []magpie.UsageNamed{{ID: "new-agent", Name: "New"}}, By: map[string][]magpie.UsageShare{"model": {{ID: "new-model"}}}},
	}
	stats := []magpie.SessionStats{
		{Days: []magpie.SessionDay{{Usage: []magpie.SessionDayUsage{{Cwd: cwd}, {Cwd: "/a"}, {Cwd: ""}}}, {Usage: []magpie.SessionDayUsage{{Cwd: "/a"}, {Cwd: cwd}}}}},
		{Days: []magpie.SessionDay{{Usage: []magpie.SessionDayUsage{{Cwd: "/new-project"}}}}},
	}
	requestCalls, statsCalls := 0, 0
	u := &sessionUpstream{
		requests: func(_ context.Context, q magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
			if q != (magpie.UsageRequestsQuery{Period: "all", Offset: 0, Limit: 1}) {
				t.Fatalf("catalog query = %+v", q)
			}
			page := pages[requestCalls]
			requestCalls++
			return page, nil
		},
		stats: func(_ context.Context, days int) (magpie.SessionStats, error) {
			if days != 0 {
				t.Fatalf("catalog stats days = %d, want 0", days)
			}
			result := stats[statsCalls]
			statsCalls++
			return result, nil
		},
	}
	s := newService(u)
	wants := []FilterOptionsResult{
		{Agents: []AgentOption{{Agent: "a", DisplayName: "A"}, {Agent: "z", DisplayName: "Z"}}, Models: []string{"a", "z"}, Projects: []ProjectRef{{Kind: ProjectKindProject, Path: "/a", Name: "a"}, {Kind: ProjectKindProject, Path: cwd, Name: "Usagi"}}},
		{Agents: []AgentOption{{Agent: "new-agent", DisplayName: "New"}}, Models: []string{"new-model"}, Projects: []ProjectRef{{Kind: ProjectKindProject, Path: "/new-project", Name: "new-project"}}},
	}
	for i, want := range wants {
		got, err := s.FilterOptions(context.Background())
		if err != nil || !reflect.DeepEqual(got, want) || requestCalls != i+1 || statsCalls != i+1 {
			t.Fatalf("catalog %d = %+v, %v, calls %d/%d; want %+v", i, got, err, requestCalls, statsCalls, want)
		}
	}
}
