package query

import (
	"context"
	"errors"
	"net/http"
	"reflect"
	"testing"
	"time"

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

func TestFilterOptionsListOnlyChoicesInRange(t *testing.T) {
	day := time.Date(2026, 10, 11, 9, 0, 0, 0, time.UTC)
	rows := []magpie.UsageRequest{
		{Time: day, Agent: "codex", AgentName: "Codex", Provider: "openai", ProviderName: "OpenAI", Model: "gpt-x", NativeSession: "s1", Priced: true},
		{Time: day, Agent: "codex", Provider: "openai", Model: "gpt-x", NativeSession: "s2", Priced: true},
		{Time: day, Agent: "claude", Provider: "session-unknown", ProviderName: "Local session", Model: "", Session: "gone", Priced: true},
		{Time: day, Agent: "rejected", Provider: "p", Model: "m", Rejected: true},
		{Time: day.AddDate(0, 0, -1), Agent: "old", Provider: "old", Model: "old", Session: "old"},
	}
	u := &sessionUpstream{
		requests: func(_ context.Context, q magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
			var matched []magpie.UsageRequest
			for _, row := range rows {
				if q.Period == row.Time.Format(time.DateOnly)+".."+row.Time.Format(time.DateOnly) {
					matched = append(matched, row)
				}
			}
			return magpie.UsageRequestsPage{Rows: matched, Total: len(matched)}, nil
		},
		sessions: func(context.Context, int) (magpie.SessionsResponse, error) {
			return magpie.SessionsResponse{Sessions: []magpie.Session{{Agent: "codex", ID: "s1", Cwd: "/work/usagi"}, {Agent: "codex", ID: "s2"}}}, nil
		},
		session: func(context.Context, string) (magpie.Session, error) {
			return magpie.Session{}, &magpie.HTTPError{StatusCode: http.StatusNotFound}
		},
	}
	s := newService(u)
	s.store.warmDays = 0
	got, err := s.FilterOptions(context.Background(), DateRange{From: "2026-10-11", To: "2026-10-11"})
	want := FilterOptionsResult{
		Agents: []AgentOption{{Agent: "claude", DisplayName: "claude"}, {Agent: "codex", DisplayName: "Codex"}},
		Models: []ModelOption{
			{Model: "gpt-x", ProviderID: "openai", ProviderName: "OpenAI"},
			{Model: UnknownModel, ProviderID: "session-unknown", ProviderName: "Local session"},
		},
		Projects: []ProjectRef{
			{Kind: ProjectKindProject, Path: "/work/usagi", Name: "usagi"},
			{Kind: ProjectKindProjectless},
			{Kind: ProjectKindUnknown},
		},
	}
	if err != nil || !reflect.DeepEqual(got, want) {
		t.Fatalf("FilterOptions = %+v, %v; want %+v", got, err, want)
	}
	if _, err := s.FilterOptions(context.Background(), DateRange{From: "2026-10-12", To: "2026-10-11"}); !errors.Is(err, ErrInvalidRange) {
		t.Fatalf("reversed range error = %v", err)
	}
}
