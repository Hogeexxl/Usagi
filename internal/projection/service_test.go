package projection

import (
	"context"
	"math"
	"reflect"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/query"
)

type fakeQuery struct {
	dataset          query.SessionDatasetResult
	filterOptions    query.FilterOptionsResult
	quotas           magpie.QuotasResponse
	overview         magpie.SessionOverview
	onSessionDataset func(context.Context, query.SessionQuery) (query.SessionDatasetResult, error)
	onFilterOptions  func(context.Context, query.DateRange) (query.FilterOptionsResult, error)
	onQuota          func(context.Context, bool) (magpie.QuotasResponse, error)
	onOverview       func(context.Context, query.OverviewQuery) (magpie.SessionOverview, error)
}

func (q *fakeQuery) SessionDataset(ctx context.Context, input query.SessionQuery) (query.SessionDatasetResult, error) {
	if q.onSessionDataset != nil {
		return q.onSessionDataset(ctx, input)
	}
	return q.dataset, nil
}

func (q *fakeQuery) FilterOptions(ctx context.Context, r query.DateRange) (query.FilterOptionsResult, error) {
	if q.onFilterOptions != nil {
		return q.onFilterOptions(ctx, r)
	}
	return q.filterOptions, nil
}
func (q *fakeQuery) Quota(ctx context.Context, asked bool) (magpie.QuotasResponse, error) {
	if q.onQuota != nil {
		return q.onQuota(ctx, asked)
	}
	return q.quotas, nil
}

func (q *fakeQuery) Overview(ctx context.Context, input query.OverviewQuery) (magpie.SessionOverview, error) {
	if q.onOverview != nil {
		return q.onOverview(ctx, input)
	}
	return q.overview, nil
}

func TestSummary(t *testing.T) {
	scope := ScopeQuery{
		Range:  query.DateRange{From: "2026-10-01", To: "2026-10-11"},
		Filter: query.UsageFilter{Agents: []string{"claude", "codex"}, Models: []query.ModelSelection{{ProviderID: "openai", Model: "m"}}},
	}
	wantSessionQuery := query.SessionQuery{Range: scope.Range, Filter: scope.Filter}
	datasetCalls := 0
	project := query.ProjectRef{Kind: query.ProjectKindProject, Path: "/workspace/project", Name: "project"}
	fake := &fakeQuery{
		onSessionDataset: func(_ context.Context, got query.SessionQuery) (query.SessionDatasetResult, error) {
			datasetCalls++
			if !reflect.DeepEqual(got, wantSessionQuery) {
				t.Fatalf("SessionDataset query = %+v, want %+v", got, wantSessionQuery)
			}
			return query.SessionDatasetResult{
				Sessions: []query.SessionUsage{
					{Agent: "claude", ID: "A", Project: project, HasRows: true, Matched: true},
					{Agent: "codex", ID: "X", Project: project, HasRows: true, Matched: true},
				},
				Rows: []query.UsageRow{
					{Time: testTime(12, 0), Agent: "claude", ResolvedSession: query.SessionRef{Agent: "claude", ID: "A", Matched: true}, Input: 2, Cost: .25, Priced: true},
					{Time: testTime(11, 0), Agent: "claude", ResolvedSession: query.SessionRef{Agent: "claude", ID: "A", Matched: true}, Subagent: "B", Input: 3, Cost: .25, Priced: true},
					{Time: testTime(10, 0), Agent: "claude", ResolvedSession: query.SessionRef{Agent: "claude", ID: "A", Matched: true}, Subagent: "C", ParentAgent: "B", Input: 4},
					{Time: testTime(9, 0), Agent: "codex", ResolvedSession: query.SessionRef{Agent: "codex", ID: "X", Matched: true}, Input: 11, Output: 10, Priced: true},
				},
			}, nil
		},
	}

	service := newService(fake)
	got, err := service.Summary(context.Background(), scope)
	if err != nil {
		t.Fatalf("Summary() error = %v", err)
	}
	if datasetCalls != 1 {
		t.Fatalf("Summary SessionDataset calls = %d, want one", datasetCalls)
	}
	wantUsage := usageFromTotals(query.UsageTotals{
		Calls: 4, Input: 20, Output: 10, Cost: 0.5, Unpriced: 1,
	})
	if !reflect.DeepEqual(got.Usage, wantUsage) || got.RootSessionCount != 2 || got.RootUnpricedSessionCount != 1 ||
		got.SessionMedian == nil || *got.SessionMedian != 9 || got.SessionP90 == nil || *got.SessionP90 != 21 {
		t.Fatalf("Summary() = %+v; want usage %+v and two roots with one unpriced root", got, wantUsage)
	}
	list, err := service.Sessions(context.Background(), scope)
	if err != nil {
		t.Fatalf("Sessions() error = %v", err)
	}
	if got.RootSessionCount != int64(len(list.Items)) || len(list.Items) != 2 {
		t.Fatalf("Summary RootSessionCount = %d, Sessions item count = %d", got.RootSessionCount, len(list.Items))
	}
	if datasetCalls != 2 {
		t.Fatalf("SessionDataset calls = %d, want one for each projection", datasetCalls)
	}
}

func TestSummaryRequestMetricsIncludeUnknownAndLegacyRowsOnce(t *testing.T) {
	project := query.ProjectRef{Kind: query.ProjectKindProject, Path: "/work", Name: "work"}
	rows := []query.UsageRow{
		{
			Agent: "codex", Session: "X", SessionRole: "main", Output: 100, Millis: 1100, TTFTMs: 100,
			ResolvedSession: query.SessionRef{Agent: "codex", ID: "X"}, Project: project,
		},
		{
			Agent: "codex", Session: "X", SessionRole: "unknown", Output: 100, Millis: 2100, TTFTMs: 100, Tries: 2,
			ResolvedSession: query.SessionRef{Agent: "codex", ID: "X"}, Project: project,
		},
		{
			Agent: "codex", Session: "X", Kind: "collab_spawn", Status: 429, Input: 4, Output: 5,
			ResolvedSession: query.SessionRef{Agent: "codex", ID: "X"}, Project: project,
		},
	}
	service := newService(&fakeQuery{dataset: query.SessionDatasetResult{
		Sessions: []query.SessionUsage{{Agent: "codex", ID: "X", Project: project, HasRows: true}}, Rows: rows,
	}})

	got, err := service.Summary(context.Background(), ScopeQuery{Range: query.DateRange{From: "2026-10-01", To: "2026-10-15"}})
	if err != nil {
		t.Fatalf("Summary() error = %v", err)
	}
	if got.Usage.Calls != 3 || got.Usage.Input != 4 || got.Usage.Output != 205 {
		t.Fatalf("existing usage summary = %+v, want all three rows and their tokens", got.Usage)
	}
	metrics := got.RequestMetrics
	if metrics.Calls != 3 || metrics.Errors != 1 || metrics.Retried != 1 || metrics.SpeedSamples != 2 || metrics.TTFTSamples != 2 {
		t.Fatalf("request metrics = %+v, want main, unknown and legacy rows exactly once", metrics)
	}
	if metrics.OutputSpeed == nil || math.Abs(*metrics.OutputSpeed-200.0/3.0) > 1e-9 || metrics.MeanTTFTSeconds == nil || *metrics.MeanTTFTSeconds != 0.1 {
		t.Fatalf("request aggregate speed/TTFT = %v/%v, want weighted 66.67 token/s and 0.1 seconds", metrics.OutputSpeed, metrics.MeanTTFTSeconds)
	}
}

func TestSessionConsistencyToolUsageProjection(t *testing.T) {
	queries := make([]query.OverviewQuery, 0, 1)
	fake := &fakeQuery{onOverview: func(_ context.Context, input query.OverviewQuery) (magpie.SessionOverview, error) {
		queries = append(queries, input)
		return magpie.SessionOverview{Tools: magpie.ToolUse{Calls: 12, Top: []magpie.ToolStat{
			{Name: "Read", Calls: 8}, {Name: "Search", Calls: 4},
		}}}, nil
	}}
	service := newService(fake)
	got, err := service.ToolUsage(context.Background(), 7)
	if err != nil {
		t.Fatalf("ToolUsage(7) error = %v", err)
	}
	want := ToolUsageView{Days: 7, Calls: 12, Top: []ToolUsageItem{{Name: "Read", Calls: 8}, {Name: "Search", Calls: 4}}}
	if !reflect.DeepEqual(got, want) || !reflect.DeepEqual(queries, []query.OverviewQuery{{Days: 7}}) {
		t.Fatalf("ToolUsage projection = %+v queries=%+v, want %+v with one 7 day overview", got, queries, want)
	}
	for _, days := range []int{0, 2, 8, 31} {
		if _, err := service.ToolUsage(context.Background(), days); err == nil {
			t.Fatalf("ToolUsage(%d) accepted an unsupported range", days)
		}
	}
	if len(queries) != 1 {
		t.Fatalf("unsupported ToolUsage ranges made upstream calls: %+v", queries)
	}
}
