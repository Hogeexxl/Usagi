package query

import (
	"context"
	"errors"
	"reflect"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type serviceUpstream struct {
	upstream
	quotas   func(context.Context, bool) (magpie.QuotasResponse, error)
	overview func(context.Context, magpie.SessionOverviewQuery) (magpie.SessionOverview, error)
}

func (u *serviceUpstream) Quotas(ctx context.Context, asked bool) (magpie.QuotasResponse, error) {
	return u.quotas(ctx, asked)
}

func (u *serviceUpstream) SessionOverview(ctx context.Context, q magpie.SessionOverviewQuery) (magpie.SessionOverview, error) {
	return u.overview(ctx, q)
}

func TestQuota(t *testing.T) {
	responses := []magpie.QuotasResponse{
		{Reading: true, Quotas: []magpie.Quota{{Provider: "p", Name: "Provider", Plan: "first", Windows: []magpie.QuotaWindow{{Name: "5h", Used: 0.25}}, Resets: &magpie.ResetCredits{Count: 1}}}},
		{Quotas: []magpie.Quota{{Provider: "p", Plan: "second", Windows: []magpie.QuotaWindow{{Name: "5h", Used: 0.75}}, Resets: &magpie.ResetCredits{Count: 2}}}},
	}
	calls := 0
	u := &serviceUpstream{quotas: func(_ context.Context, asked bool) (magpie.QuotasResponse, error) {
		if asked != (calls == 0) {
			t.Fatalf("asked = %v on call %d", asked, calls)
		}
		response := responses[calls]
		calls++
		return response, nil
	}}
	s := newService(queryTestDB(t), u, time.Local)
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
	u := &serviceUpstream{overview: func(_ context.Context, q magpie.SessionOverviewQuery) (magpie.SessionOverview, error) {
		response := responses[len(queries)]
		queries = append(queries, q)
		return response, nil
	}}
	s := newService(queryTestDB(t), u, time.Local)
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
	db := queryTestDB(t)
	insertDatasetSession(t, db, "codex", "s1", "/work/usagi", "", day, day, false)
	insertDatasetSession(t, db, "codex", "s2", "", "", day, day, false)
	rows := []struct {
		time                 time.Time
		agent, agentName     string
		provider, providerName, model string
		session              string
		rejected             int
	}{
		{day, "codex", "Codex", "openai", "OpenAI", "gpt-x", "s1", 0},
		{day, "codex", "", "openai", "", "gpt-x", "s2", 0},
		{day, "claude", "", "session-unknown", "Local session", "", "gone", 0},
		{day, "rejected", "", "p", "", "m", "bad", 1},
		{day.AddDate(0, 0, -1), "old", "", "old", "", "old", "old", 0},
	}
	for _, row := range rows {
		if _, err := db.Exec(`INSERT INTO requests (t_ns,local_day,agent,agent_name,provider,provider_name,model,session,rejected)
			VALUES (?,?,?,?,?,?,?,?,?)`, row.time.UnixNano(), row.time.Format(time.DateOnly), row.agent, row.agentName,
			row.provider, row.providerName, row.model, row.session, row.rejected); err != nil {
			t.Fatalf("insert filter option request: %v", err)
		}
	}
	s := newService(db, nil, time.Local)
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
