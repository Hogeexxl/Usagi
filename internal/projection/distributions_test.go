package projection

import (
	"context"
	"database/sql"
	"path/filepath"
	"reflect"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/database"
	"github.com/Hogeexxl/Usagi/internal/query"
)

func TestModelDistributionSeparatesSameModelAcrossProviders(t *testing.T) {
	db, err := database.Open(filepath.Join(t.TempDir(), "usagi.sqlite"))
	if err != nil {
		t.Fatalf("open test database: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	service := NewService(query.NewService(db, nil, time.UTC))
	rangeValue := query.DateRange{From: "2026-10-11", To: "2026-10-11"}
	insertDistributionSession(t, db, "session-a", "/repo/a")
	insertDistributionSession(t, db, "session-b", "/repo/b")
	insertDistributionRequest(t, db, "session-a", "provider-a", "gpt-x", 1)
	insertDistributionRequest(t, db, "session-b", "provider-b", "gpt-x", 10)
	for _, test := range []struct {
		provider string
		input    int64
	}{
		{provider: "provider-a", input: 1},
		{provider: "provider-b", input: 10},
	} {
		view, err := service.ModelDistribution(context.Background(), ScopeQuery{
			Range:  rangeValue,
			Filter: query.UsageFilter{Models: []query.ModelSelection{{ProviderID: test.provider, Model: "gpt-x"}}},
		})
		if err != nil || len(view.Items) != 1 || view.Items[0].Model != "gpt-x" || view.Items[0].Usage.Input != test.input {
			t.Fatalf("ModelDistribution(%s/gpt-x) = %+v, %v", test.provider, view, err)
		}
	}
}

func insertDistributionSession(t *testing.T, db *sql.DB, id, cwd string) {
	t.Helper()
	stamp := time.Date(2026, 10, 11, 12, 0, 0, 0, time.UTC).UnixNano()
	if _, err := db.Exec(`INSERT INTO sessions (agent, id, cwd, start_ns, last_ns, first_seen_ns, last_seen_ns)
		VALUES ('codex', ?, ?, ?, ?, ?, ?)`, id, cwd, stamp, stamp, stamp, stamp); err != nil {
		t.Fatalf("insert distribution session: %v", err)
	}
}

func insertDistributionRequest(t *testing.T, db *sql.DB, session, provider, model string, input int64) {
	t.Helper()
	stamp := time.Date(2026, 10, 11, 12, 1, 0, 0, time.UTC).UnixNano()
	if _, err := db.Exec(`INSERT INTO requests (t_ns, local_day, agent, provider, model, session, input)
		VALUES (?, '2026-10-11', 'codex', ?, ?, ?, ?)`, stamp, provider, model, session, input); err != nil {
		t.Fatalf("insert distribution request: %v", err)
	}
}

func TestDistribution(t *testing.T) {
	scope := ScopeQuery{
		Range:  query.DateRange{From: "2026-10-01", To: "2026-10-11"},
		Filter: query.UsageFilter{Agents: []string{"codex"}, Models: []query.ModelSelection{{ProviderID: "openai", Model: "gpt"}}},
	}
	rows := []query.UsageRow{
		distributionRow(testTime(9, 0), "codex", "zeta", "provider-a", 5, 5, query.ProjectRef{Kind: query.ProjectKindProject, Path: "/z", Name: "z"}, true, .25),
		distributionRow(testTime(9, 1), "codex", "alpha", "provider-a", 5, 0, query.ProjectRef{Kind: query.ProjectKindUnknown}, false, 0),
		distributionRow(testTime(9, 2), "codex", "mid", "provider-a", 8, 0, query.ProjectRef{Kind: query.ProjectKindProject, Path: "/large", Name: "large"}, true, 0),
		distributionRow(testTime(9, 3), "codex", "alpha", "provider-a", 5, 0, query.ProjectRef{Kind: query.ProjectKindProjectless}, true, 0),
		distributionRow(testTime(9, 4), "codex", "zeta", "provider-a", 5, 0, query.ProjectRef{Kind: query.ProjectKindProject, Path: "/a", Name: "a"}, true, 0),
	}
	fake := &fakeQuery{dataset: query.SessionDatasetResult{Rows: rows}}

	service := newService(fake)
	models, err := service.ModelDistribution(context.Background(), scope)
	if err != nil {
		t.Fatalf("ModelDistribution() error = %v", err)
	}
	wantModels := ModelDistributionView{Items: []ModelDistributionItem{
		{Model: "zeta", Usage: usageWithRequestCalls(query.UsageTotals{Calls: 2, Input: 10, Output: 5, Cost: 0.25}, 2)},
		{Model: "alpha", Usage: usageWithRequestCalls(query.UsageTotals{Calls: 2, Input: 10, Unpriced: 1}, 2)},
		{Model: "mid", Usage: usageWithRequestCalls(query.UsageTotals{Calls: 1, Input: 8}, 1)},
	}}
	if !reflect.DeepEqual(models, wantModels) {
		t.Fatalf("ModelDistribution() = %+v, want %+v", models, wantModels)
	}

	projects, err := service.ProjectDistribution(context.Background(), scope)
	if err != nil {
		t.Fatalf("ProjectDistribution() error = %v", err)
	}
	wantProjects := ProjectDistributionView{Items: []ProjectDistributionItem{
		{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/z", Name: "z"}, Usage: usageWithRequestCalls(query.UsageTotals{Calls: 1, Input: 5, Output: 5, Cost: .25}, 1)},
		{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/large", Name: "large"}, Usage: usageWithRequestCalls(query.UsageTotals{Calls: 1, Input: 8}, 1)},
		{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/a", Name: "a"}, Usage: usageWithRequestCalls(query.UsageTotals{Calls: 1, Input: 5}, 1)},
		{Project: query.ProjectRef{Kind: query.ProjectKindProjectless}, Usage: usageWithRequestCalls(query.UsageTotals{Calls: 1, Input: 5}, 1)},
		{Project: query.ProjectRef{Kind: query.ProjectKindUnknown}, Usage: usageWithRequestCalls(query.UsageTotals{Calls: 1, Input: 5, Unpriced: 1}, 1)},
	}}
	if !reflect.DeepEqual(projects, wantProjects) {
		t.Fatalf("ProjectDistribution() = %+v, want %+v", projects, wantProjects)
	}
}

func TestProjectDistributionAggregatesWorkspaces(t *testing.T) {
	workspaceOne := distributionRow(testTime(9, 0), "codex", "workspace-one", "provider-a", 10, 4,
		query.ProjectRef{Kind: query.ProjectKindWorkspace, Path: "/tmp/claude-1"}, true, .5)
	workspaceOne.CacheRead, workspaceOne.CacheWrite, workspaceOne.CacheWrite1h, workspaceOne.Reasoning = 3, 2, 1, 2
	workspaceTwo := distributionRow(testTime(9, 1), "codex", "workspace-two", "provider-a", 20, 5,
		query.ProjectRef{Kind: query.ProjectKindWorkspace, Path: "/tmp/claude-2"}, true, .75)
	workspaceTwo.CacheRead, workspaceTwo.CacheWrite, workspaceTwo.CacheWrite1h, workspaceTwo.Reasoning = 4, 1, 2, 3
	projectOne := distributionRow(testTime(9, 2), "codex", "project-one", "provider-a", 5, 5,
		query.ProjectRef{Kind: query.ProjectKindProject, Path: "/repo-one/same-name", Name: "same-name"}, true, .4)
	projectTwo := distributionRow(testTime(9, 3), "codex", "project-two", "provider-a", 4, 6,
		query.ProjectRef{Kind: query.ProjectKindProject, Path: "/repo-two/same-name", Name: "same-name"}, true, .3)
	service := newService(&fakeQuery{dataset: query.SessionDatasetResult{Rows: []query.UsageRow{workspaceOne, workspaceTwo, projectOne, projectTwo}}})

	view, err := service.ProjectDistribution(context.Background(), ScopeQuery{})
	if err != nil {
		t.Fatalf("ProjectDistribution() error = %v", err)
	}
	if len(view.Items) != 3 {
		t.Fatalf("ProjectDistribution() returned %d groups, want one workspace and two same-named projects: %+v", len(view.Items), view.Items)
	}
	projects := make(map[string]ProjectDistributionItem)
	var workspace ProjectDistributionItem
	for _, item := range view.Items {
		if item.Project.Kind == query.ProjectKindWorkspace {
			workspace = item
		} else {
			projects[item.Project.Path] = item
		}
	}
	if workspace.Project != (query.ProjectRef{Kind: query.ProjectKindWorkspace}) {
		t.Fatalf("workspace aggregate retained a member path or name: %+v", workspace.Project)
	}
	wantWorkspaceUsage := usageWithRequestCalls(query.UsageTotals{
		Calls: 2, Input: 30, Output: 9, CacheRead: 7, CacheWrite: 3, CacheWrite1h: 3,
		Reasoning: 5, Cost: 1.25,
	}, 2)
	if !reflect.DeepEqual(workspace.Usage, wantWorkspaceUsage) {
		t.Fatalf("workspace aggregate usage = %+v, want %+v", workspace.Usage, wantWorkspaceUsage)
	}
	if projects["/repo-one/same-name"].Project.Name != "same-name" || projects["/repo-two/same-name"].Project.Name != "same-name" {
		t.Fatalf("same-named project paths were merged: %+v", projects)
	}
}

func distributionRow(at time.Time, agent, model, provider string, input, output int64, project query.ProjectRef, priced bool, cost float64) query.UsageRow {
	return query.UsageRow{Time: at, Agent: agent, Model: model, Provider: provider, Input: input, Output: output,
		Project: project, Priced: priced, Cost: cost, ResolvedSession: query.SessionRef{Agent: agent, ID: "session", Matched: project.Kind != query.ProjectKindUnknown}}
}
