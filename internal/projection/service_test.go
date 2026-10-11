package projection

import (
	"context"
	"reflect"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/query"
)

type fakeQuery struct {
	usageResult     query.UsageResult
	usageRows       query.UsageRowsResult
	sessions        query.SessionResult
	filterOptions   query.FilterOptionsResult
	quotas          magpie.QuotasResponse
	onUsage         func(context.Context, query.UsageQuery) (query.UsageResult, error)
	onUsageRows     func(context.Context, query.UsageRowsQuery) (query.UsageRowsResult, error)
	onSessions      func(context.Context, query.SessionQuery) (query.SessionResult, error)
	onFilterOptions func(context.Context) (query.FilterOptionsResult, error)
	onQuota         func(context.Context, bool) (magpie.QuotasResponse, error)
}

func (q *fakeQuery) Usage(ctx context.Context, input query.UsageQuery) (query.UsageResult, error) {
	if q.onUsage != nil {
		return q.onUsage(ctx, input)
	}
	return q.usageResult, nil
}
func (q *fakeQuery) UsageRows(ctx context.Context, input query.UsageRowsQuery) (query.UsageRowsResult, error) {
	if q.onUsageRows != nil {
		return q.onUsageRows(ctx, input)
	}
	return q.usageRows, nil
}
func (q *fakeQuery) Sessions(ctx context.Context, input query.SessionQuery) (query.SessionResult, error) {
	if q.onSessions != nil {
		return q.onSessions(ctx, input)
	}
	return q.sessions, nil
}

func (q *fakeQuery) FilterOptions(ctx context.Context) (query.FilterOptionsResult, error) {
	if q.onFilterOptions != nil {
		return q.onFilterOptions(ctx)
	}
	return q.filterOptions, nil
}
func (q *fakeQuery) Quota(ctx context.Context, asked bool) (magpie.QuotasResponse, error) {
	if q.onQuota != nil {
		return q.onQuota(ctx, asked)
	}
	return q.quotas, nil
}

func TestSummary(t *testing.T) {
	scope := ScopeQuery{
		Range:  query.DateRange{From: "2026-10-01", To: "2026-10-11"},
		Filter: query.UsageFilter{Agents: []string{"claude", "codex"}, Models: []string{"m"}},
	}
	wantUsageQuery := query.UsageQuery{Range: scope.Range, Filter: scope.Filter, GroupBy: query.GroupNone}
	wantSessionQuery := query.SessionQuery{Range: scope.Range, Filter: scope.Filter}
	wantRowsQuery := query.UsageRowsQuery{Range: scope.Range, Filter: scope.Filter}
	usageCalls, sessionCalls, rowCalls := 0, 0, 0
	project := query.ProjectRef{Kind: query.ProjectKindProject, Path: "/workspace/project", Name: "project"}
	fake := &fakeQuery{
		onUsage: func(_ context.Context, got query.UsageQuery) (query.UsageResult, error) {
			usageCalls++
			if !reflect.DeepEqual(got, wantUsageQuery) {
				t.Fatalf("Usage query = %+v, want %+v", got, wantUsageQuery)
			}
			return query.UsageResult{Totals: query.UsageTotals{
				Calls: 4, Errors: 1, Input: 20, Output: 10, CacheRead: 20, CacheWrite: 50,
				CacheWrite1h: 30, Reasoning: 5, Cost: 0.75, Unpriced: 2,
			}}, nil
		},
		onSessions: func(_ context.Context, got query.SessionQuery) (query.SessionResult, error) {
			sessionCalls++
			if !reflect.DeepEqual(got, wantSessionQuery) {
				t.Fatalf("Sessions query = %+v, want %+v", got, wantSessionQuery)
			}
			return query.SessionResult{Sessions: []query.SessionUsage{
				{Agent: "claude", ID: "A", Project: project, FirstUsage: testTime(9, 0), LastUsage: testTime(12, 0)},
				{Agent: "codex", ID: "X", Project: project, Totals: query.UsageTotals{Input: 100}},
			}}, nil
		},
		onUsageRows: func(_ context.Context, got query.UsageRowsQuery) (query.UsageRowsResult, error) {
			rowCalls++
			if !reflect.DeepEqual(got, wantRowsQuery) {
				t.Fatalf("UsageRows query = %+v, want %+v", got, wantRowsQuery)
			}
			return query.UsageRowsResult{Rows: []query.UsageRow{
				{Time: testTime(12, 0), Agent: "claude", ResolvedSession: query.SessionRef{Agent: "claude", ID: "A"}, Input: 2, Priced: true},
				{Time: testTime(11, 0), Agent: "claude", ResolvedSession: query.SessionRef{Agent: "claude", ID: "A"}, Subagent: "B", Input: 3, Priced: true},
				{Time: testTime(10, 0), Agent: "claude", ResolvedSession: query.SessionRef{Agent: "claude", ID: "A"}, Subagent: "C", ParentAgent: "B", Input: 4, Priced: true},
			}}, nil
		},
	}

	service := newService(fake)
	got, err := service.Summary(context.Background(), scope)
	if err != nil {
		t.Fatalf("Summary() error = %v", err)
	}
	wantUsage := usageFromTotals(query.UsageTotals{
		Calls: 4, Errors: 1, Input: 20, Output: 10, CacheRead: 20, CacheWrite: 50,
		CacheWrite1h: 30, Reasoning: 5, Cost: 0.75, Unpriced: 2,
	})
	if !reflect.DeepEqual(got.Usage, wantUsage) || got.SessionCount != 4 {
		t.Fatalf("Summary() = %+v; want usage %+v and projected thread count 4", got, wantUsage)
	}
	list, err := service.Sessions(context.Background(), scope)
	if err != nil {
		t.Fatalf("Sessions() error = %v", err)
	}
	if got.SessionCount != int64(len(list.Threads)) || len(list.Threads) != 4 {
		t.Fatalf("Summary SessionCount = %d, Sessions thread count = %d", got.SessionCount, len(list.Threads))
	}
	if usageCalls != 1 || sessionCalls != 2 || rowCalls != 2 {
		t.Fatalf("query calls = usage %d, sessions %d, rows %d", usageCalls, sessionCalls, rowCalls)
	}
}
