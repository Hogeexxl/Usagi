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
		{Model: "zeta", Usage: usageFromTotals(query.UsageTotals{Calls: 2, Input: 10, Output: 5, Cost: 0.25})},
		{Model: "alpha", Usage: usageFromTotals(query.UsageTotals{Calls: 2, Input: 10, Unpriced: 1})},
		{Model: "mid", Usage: usageFromTotals(query.UsageTotals{Calls: 1, Input: 8})},
	}}
	if !reflect.DeepEqual(models, wantModels) {
		t.Fatalf("ModelDistribution() = %+v, want %+v", models, wantModels)
	}

	projects, err := service.ProjectDistribution(context.Background(), scope)
	if err != nil {
		t.Fatalf("ProjectDistribution() error = %v", err)
	}
	wantProjects := ProjectDistributionView{Items: []ProjectDistributionItem{
		{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/z", Name: "z"}, Usage: usageFromTotals(query.UsageTotals{Calls: 1, Input: 5, Output: 5, Cost: .25})},
		{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/large", Name: "large"}, Usage: usageFromTotals(query.UsageTotals{Calls: 1, Input: 8})},
		{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/a", Name: "a"}, Usage: usageFromTotals(query.UsageTotals{Calls: 1, Input: 5})},
		{Project: query.ProjectRef{Kind: query.ProjectKindProjectless}, Usage: usageFromTotals(query.UsageTotals{Calls: 1, Input: 5})},
		{Project: query.ProjectRef{Kind: query.ProjectKindUnknown}, Usage: usageFromTotals(query.UsageTotals{Calls: 1, Input: 5, Unpriced: 1})},
	}}
	if !reflect.DeepEqual(projects, wantProjects) {
		t.Fatalf("ProjectDistribution() = %+v, want %+v", projects, wantProjects)
	}
}

func distributionRow(at time.Time, agent, model, provider string, input, output int64, project query.ProjectRef, priced bool, cost float64) query.UsageRow {
	return query.UsageRow{Time: at, Agent: agent, Model: model, Provider: provider, Input: input, Output: output,
		Project: project, Priced: priced, Cost: cost, ResolvedSession: query.SessionRef{Agent: agent, ID: "session", Matched: project.Kind != query.ProjectKindUnknown}}
}
