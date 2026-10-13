package query

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"reflect"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

func TestUsageValidation(t *testing.T) {
	r := DateRange{From: "2026-10-01", To: "2026-10-11"}
	s := newService(&sessionUpstream{})
	tests := []struct {
		name string
		q    UsageQuery
		want error
	}{
		{"invalid from", UsageQuery{Range: DateRange{From: "2026-02-30", To: r.To}}, ErrInvalidRange},
		{"invalid to", UsageQuery{Range: DateRange{From: r.From, To: "2026-10-11T00:00:00Z"}}, ErrInvalidRange},
		{"reversed range", UsageQuery{Range: DateRange{From: r.To, To: r.From}}, ErrInvalidRange},
		{"unknown kind", UsageQuery{Range: r, Filter: UsageFilter{Projects: []ProjectSelection{{Kind: "other"}}}}, ErrInvalidFilter},
		{"missing project path", UsageQuery{Range: r, Filter: UsageFilter{Projects: []ProjectSelection{{Kind: ProjectKindProject}}}}, ErrInvalidFilter},
		{"projectless path", UsageQuery{Range: r, Filter: UsageFilter{Projects: []ProjectSelection{{Kind: ProjectKindProjectless, Path: "/a"}}}}, ErrInvalidFilter},
		{"unknown path", UsageQuery{Range: r, Filter: UsageFilter{Projects: []ProjectSelection{{Kind: ProjectKindUnknown, Path: "/a"}}}}, ErrInvalidFilter},
		{"invalid group", UsageQuery{Range: r, GroupBy: "other"}, ErrInvalidGroupBy},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if _, err := s.Usage(context.Background(), test.q); !errors.Is(err, test.want) {
				t.Fatalf("Usage error = %v, want %v", err, test.want)
			}
			if test.want != ErrInvalidGroupBy {
				if _, err := s.UsageRows(context.Background(), UsageRowsQuery{Range: test.q.Range, Filter: test.q.Filter}); !errors.Is(err, test.want) {
					t.Fatalf("UsageRows error = %v, want %v", err, test.want)
				}
			}
		})
	}
}

func TestUsageGroupsMissingModelAsUnknown(t *testing.T) {
	rows := []magpie.UsageRequest{
		{Agent: "codex", Provider: "p", Model: "", Session: "s", Input: 2, Priced: true},
		{Agent: "codex", Provider: "p", Model: "m", Session: "s", Input: 3, Priced: true},
	}
	u := &sessionUpstream{
		requests: func(context.Context, magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
			return magpie.UsageRequestsPage{Rows: rows, Total: len(rows)}, nil
		},
		sessions: func(context.Context, int) (magpie.SessionsResponse, error) { return magpie.SessionsResponse{}, nil },
		session: func(context.Context, string) (magpie.Session, error) {
			return magpie.Session{}, &magpie.HTTPError{StatusCode: http.StatusNotFound}
		},
	}
	s := newService(u)
	s.store.warmDays = 0
	r := DateRange{From: "2026-10-11", To: "2026-10-11"}
	for _, by := range []UsageGroupBy{GroupModel, GroupModelEffort, GroupProjectModel} {
		got, err := s.Usage(context.Background(), UsageQuery{Range: r, GroupBy: by})
		if err != nil || len(got.Groups) != 2 || got.Groups[0].Model != "m" || got.Groups[1].Model != UnknownModel || got.Groups[1].Totals.Input != 2 {
			t.Fatalf("Usage(%q) = %+v, %v; want missing model grouped as %q", by, got.Groups, err, UnknownModel)
		}
	}
	sessions, err := s.Sessions(context.Background(), SessionQuery{Range: r})
	if err != nil || len(sessions.Sessions) != 1 {
		t.Fatalf("Sessions = %+v, %v", sessions, err)
	}
	var models []string
	for _, group := range sessions.Sessions[0].ModelEffort {
		models = append(models, group.Model)
	}
	if want := []string{"m", UnknownModel}; !reflect.DeepEqual(models, want) {
		t.Fatalf("session model groups = %v, want %v", models, want)
	}
}

func TestUsageRequestsAndGroups(t *testing.T) {
	ctx := context.Background()
	r := DateRange{From: "2026-10-11", To: "2026-10-11"}
	rows := []magpie.UsageRequest{
		{Agent: "codex", AgentName: "Codex", Provider: "p", Model: "z", Effort: "high", Status: 200, Input: 10, Output: 2, CacheRead: 3, CacheWrite: 4, CacheWrite1h: 5, Reasoning: 6, Cost: 0.5, Priced: true},
		{Agent: "claude", AgentName: "Claude", Model: "a", Effort: "low", Source: "log", Input: 7, Output: 1, Cost: 0.25, Error: "direct failure"},
		{Agent: "codex", AgentName: "Codex", Provider: "p", Model: "z", Effort: "low", Status: 500},
		{Provider: "p", Rejected: true, Input: 1000},
		{Status: 400, Input: 1000},
	}
	current := rows
	u := &sessionUpstream{requests: func(_ context.Context, q magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
		want := magpie.UsageRequestsQuery{Period: "2026-10-11..2026-10-11", Limit: 500}
		if q != want {
			t.Fatalf("Requests query = %+v, want %+v", q, want)
		}
		return magpie.UsageRequestsPage{Rows: current, Total: len(current)}, nil
	}}
	s := newService(u)
	s.store.warmDays = 0
	gateway := UsageTotals{Calls: 1, Input: 10, Output: 2, CacheRead: 3, CacheWrite: 4, CacheWrite1h: 5, Reasoning: 6, Cost: 0.5}
	direct := UsageTotals{Calls: 1, Errors: 1, Input: 7, Output: 1, Cost: 0.25, Unpriced: 1}
	codex := gateway
	codex.Calls, codex.Errors = 2, 1
	wantTotals := UsageTotals{Calls: 3, Errors: 2, Input: 17, Output: 3, CacheRead: 3, CacheWrite: 4, CacheWrite1h: 5, Reasoning: 6, Cost: 0.75, Unpriced: 1}
	tests := []struct {
		by     UsageGroupBy
		groups []UsageGroup
	}{
		{GroupNone, nil},
		{GroupAgent, []UsageGroup{{Agent: "claude", AgentName: "Claude", Totals: direct}, {Agent: "codex", AgentName: "Codex", Totals: codex}}},
		{GroupModel, []UsageGroup{{Model: "a", Totals: direct}, {Model: "z", Totals: codex}}},
		{GroupModelEffort, []UsageGroup{{Model: "a", Effort: "low", Totals: direct}, {Model: "z", Effort: "high", Totals: gateway}, {Model: "z", Effort: "low", Totals: UsageTotals{Calls: 1, Errors: 1}}}},
	}
	for _, test := range tests {
		got, err := s.Usage(ctx, UsageQuery{Range: r, GroupBy: test.by})
		if err != nil || got.Totals != wantTotals || !reflect.DeepEqual(got.Groups, test.groups) {
			t.Fatalf("Usage(%q) = %+v, %v; want totals %+v, groups %+v", test.by, got, err, wantTotals, test.groups)
		}
		if test.by == GroupNone {
			current = []magpie.UsageRequest{{Provider: "p", Input: 99, Priced: true}}
			got, err = s.Usage(ctx, UsageQuery{Range: r})
			if err != nil || got.Totals != wantTotals {
				t.Fatalf("cached Usage before refresh = %+v, %v; want earlier rows", got, err)
			}
			changed, err := s.RefreshUsageRows(ctx, false)
			if err != nil || !changed {
				t.Fatalf("RefreshUsageRows = %v, %v; want changed rows", changed, err)
			}
			got, err = s.Usage(ctx, UsageQuery{Range: r})
			if err != nil || got.Totals != (UsageTotals{Calls: 1, Input: 99}) || len(got.Groups) != 0 {
				t.Fatalf("Usage after refresh kept stale rows: %+v, %v", got, err)
			}
			current = rows
			if _, err := s.RefreshUsageRows(ctx, false); err != nil {
				t.Fatal(err)
			}
		}
	}
}

func TestUsageFilters(t *testing.T) {
	r := DateRange{From: "2026-10-11", To: "2026-10-11"}
	rows := []magpie.UsageRequest{
		{Agent: "codex", Provider: "provider-a", Model: "gpt-x", Input: 1, Priced: true},
		{Agent: "claude", Provider: "provider-b", Model: "gpt-x", Input: 10, Priced: true},
		{Agent: "other", Provider: "provider-a", Model: "other", Input: 100, Priced: true},
		{Agent: "codex", Provider: "provider-b", Model: "other", Input: 1000, Priced: true},
	}
	var queries []magpie.UsageRequestsQuery
	u := &sessionUpstream{requests: func(_ context.Context, q magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
		queries = append(queries, q)
		var pageRows []magpie.UsageRequest
		for _, row := range rows {
			if (q.Agent == "" || row.Agent == q.Agent) && (q.Provider == "" || row.Provider == q.Provider) && (q.Model == "" || row.Model == q.Model) {
				pageRows = append(pageRows, row)
			}
		}
		return magpie.UsageRequestsPage{Rows: pageRows, Total: len(pageRows)}, nil
	}}
	s := newService(u)
	filters := []UsageFilter{
		{Agents: []string{"", "codex", "codex"}, Models: []ModelSelection{{ProviderID: "provider-a", Model: "gpt-x"}, {ProviderID: "provider-a", Model: "gpt-x"}}},
		{Agents: []string{"codex", "", "claude", "codex"}, Models: []ModelSelection{{ProviderID: "provider-b", Model: "other"}, {ProviderID: "provider-a", Model: "gpt-x"}, {ProviderID: "provider-b", Model: "other"}}},
	}
	for i, filter := range filters {
		got, err := s.Usage(context.Background(), UsageQuery{Range: r, Filter: filter})
		want := []UsageTotals{{Calls: 1, Input: 1}, {Calls: 2, Input: 1001}}[i]
		if err != nil || got.Totals != want {
			t.Fatalf("filter %d = %+v, %v; want %+v", i, got, err, want)
		}
	}
	// Filters run in memory over the cached day, so both queries share one fetch.
	wantQueries := []magpie.UsageRequestsQuery{{Period: "2026-10-11..2026-10-11", Limit: 500}}
	if !reflect.DeepEqual(queries, wantQueries) {
		t.Fatalf("day queries = %+v, want %+v", queries, wantQueries)
	}
	normalized, err := normalizeUsageInput(r, filters[1])
	wantModels := []ModelSelection{{ProviderID: "provider-a", Model: "gpt-x"}, {ProviderID: "provider-b", Model: "other"}}
	if err != nil || !reflect.DeepEqual(normalized.Agents, []string{"claude", "codex"}) || !reflect.DeepEqual(normalized.Models, wantModels) {
		t.Fatalf("normalized filter = %+v, %v", normalized, err)
	}
}

func TestProviderModelPairFiltersUsageGroupsAndSessions(t *testing.T) {
	rangeValue := DateRange{From: "2026-10-01", To: "2026-10-11"}
	rows := []magpie.UsageRequest{
		{Agent: "codex", Provider: "provider-a", Model: "gpt-x", Session: "session-a", Time: time.Date(2026, 10, 2, 0, 0, 0, 0, time.UTC), Input: 1, Priced: true},
		{Agent: "codex", Provider: "provider-b", Model: "gpt-x", Session: "session-b", Time: time.Date(2026, 10, 3, 0, 0, 0, 0, time.UTC), Input: 10, Priced: true},
	}
	sessions := []magpie.Session{
		{Agent: "codex", ID: "session-a", Title: "A", Cwd: "/a"},
		{Agent: "codex", ID: "session-b", Title: "B", Cwd: "/b"},
	}
	var queries recordedQueries
	upstream := &sessionUpstream{
		sessions: func(context.Context, int) (magpie.SessionsResponse, error) {
			return magpie.SessionsResponse{Sessions: sessions}, nil
		},
		requests: dayRequests(rows, &queries),
	}
	service := newService(upstream)
	for _, test := range []struct {
		provider string
		session  string
		input    int
	}{
		{provider: "provider-a", session: "session-a", input: 1},
		{provider: "provider-b", session: "session-b", input: 10},
	} {
		filter := UsageFilter{Models: []ModelSelection{{ProviderID: test.provider, Model: "gpt-x"}}}
		usage, err := service.Usage(context.Background(), UsageQuery{Range: rangeValue, Filter: filter, GroupBy: GroupModel})
		if err != nil || usage.Totals.Input != int64(test.input) || len(usage.Groups) != 1 || usage.Groups[0].Model != "gpt-x" || usage.Groups[0].Totals.Input != int64(test.input) {
			t.Fatalf("Usage(%s/gpt-x) = %+v, %v", test.provider, usage, err)
		}
		result, err := service.Sessions(context.Background(), SessionQuery{Range: rangeValue, Filter: filter})
		if err != nil || len(result.Sessions) != 1 || result.Sessions[0].ID != test.session || result.Sessions[0].Totals.Input != int64(test.input) {
			t.Fatalf("Sessions(%s/gpt-x) = %+v, %v", test.provider, result, err)
		}
	}
	// Each of the 11 days is fetched once, unfiltered, and reused by every query.
	got := queries.periods()
	if len(got) != 11 || got[0] != "2026-10-01..2026-10-01" || got[10] != "2026-10-11..2026-10-11" {
		t.Fatalf("day periods = %v, want one per day", got)
	}
	for _, q := range queries.all() {
		if q.Provider != "" || q.Model != "" || q.Agent != "" {
			t.Fatalf("filter pushed down to Magpie: %+v", q)
		}
	}
}

func TestUsagePagination(t *testing.T) {
	rows := make([]magpie.UsageRequest, 503)
	for i := range rows {
		rows[i] = magpie.UsageRequest{Provider: "p", Input: 1, Priced: true}
	}
	rows[0].Rejected = true
	rows[1].Provider, rows[1].Status = "", 400
	var offsets []int
	failure := errors.New("request transport failed")
	fail := false
	u := &sessionUpstream{requests: func(_ context.Context, q magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
		if fail {
			return magpie.UsageRequestsPage{}, failure
		}
		if q.Limit != 500 || q.Period != "2026-10-11..2026-10-11" {
			return magpie.UsageRequestsPage{}, fmt.Errorf("page query = %+v", q)
		}
		offsets = append(offsets, q.Offset)
		end := min(q.Offset+q.Limit, len(rows))
		return magpie.UsageRequestsPage{Rows: rows[q.Offset:end], Total: 600}, nil
	}}
	s := newService(u)
	s.store.warmDays = 0
	q := UsageQuery{Range: DateRange{From: "2026-10-11", To: "2026-10-11"}}
	got, err := s.Usage(context.Background(), q)
	if err != nil || got.Totals != (UsageTotals{Calls: 501, Input: 501}) || !reflect.DeepEqual(offsets, []int{0, 500, 503}) {
		t.Fatalf("paged Usage = %+v, %v, offsets %v", got, err, offsets)
	}
	fail = true
	if _, err := s.RefreshUsageRows(context.Background(), false); !errors.Is(err, failure) {
		t.Fatalf("failed refresh = %v; want wrapped upstream error", err)
	}
	got, err = s.Usage(context.Background(), q)
	if err != nil || got.Totals != (UsageTotals{Calls: 501, Input: 501}) {
		t.Fatalf("failed refresh dropped cached rows: %+v, %v", got, err)
	}
	got, err = s.Usage(context.Background(), UsageQuery{Range: DateRange{From: "2026-10-10", To: "2026-10-10"}})
	if !errors.Is(err, failure) || !reflect.DeepEqual(got, UsageResult{}) {
		t.Fatalf("failed Usage = %+v, %v; want wrapped upstream error and no partial result", got, err)
	}
}

func TestUsageProjectGroups(t *testing.T) {
	ctx := context.Background()
	r := DateRange{From: "2026-10-11", To: "2026-10-11"}
	cwd := "/a"
	var events []string
	u := &sessionUpstream{
		sessions: func(_ context.Context, limit int) (magpie.SessionsResponse, error) {
			if limit != 0 {
				t.Fatalf("refresh limit = %d", limit)
			}
			events = append(events, "refresh")
			return magpie.SessionsResponse{Sessions: []magpie.Session{{Agent: "codex", ID: "a", Cwd: cwd}, {Agent: "codex", ID: "b", Cwd: "/b"}, {Agent: "codex", ID: "blank"}}}, nil
		},
		requests: func(context.Context, magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
			events = append(events, "requests")
			return magpie.UsageRequestsPage{Total: 4, Rows: []magpie.UsageRequest{
				{Agent: "codex", Session: "b", Model: "z", Effort: "low", Input: 2, Priced: true},
				{Agent: "codex", Session: "a", Model: "a", Effort: "high", Input: 1, Priced: true},
				{Agent: "codex", Session: "blank", Model: "a", Effort: "low", Input: 3, Priced: true},
				{Agent: "codex", Session: "missing", Model: "x", Effort: "high", Input: 4, Priced: true},
			}}, nil
		},
		session: func(_ context.Context, key string) (magpie.Session, error) {
			events = append(events, "one")
			if key != "codex:missing" {
				t.Fatalf("unexpected fallback %q", key)
			}
			return magpie.Session{}, &magpie.HTTPError{StatusCode: http.StatusNotFound}
		},
	}
	s := newService(u)
	a := ProjectRef{Kind: ProjectKindProject, Path: "/a", Name: "a"}
	b := ProjectRef{Kind: ProjectKindProject, Path: "/b", Name: "b"}
	blank := ProjectRef{Kind: ProjectKindProjectless}
	unknown := ProjectRef{Kind: ProjectKindUnknown}
	tests := []struct {
		by     UsageGroupBy
		groups []UsageGroup
	}{
		{GroupProject, []UsageGroup{{Project: a, Totals: UsageTotals{Calls: 1, Input: 1}}, {Project: b, Totals: UsageTotals{Calls: 1, Input: 2}}, {Project: blank, Totals: UsageTotals{Calls: 1, Input: 3}}, {Project: unknown, Totals: UsageTotals{Calls: 1, Input: 4}}}},
		{GroupProjectModel, []UsageGroup{{Project: a, Model: "a", Totals: UsageTotals{Calls: 1, Input: 1}}, {Project: b, Model: "z", Totals: UsageTotals{Calls: 1, Input: 2}}, {Project: blank, Model: "a", Totals: UsageTotals{Calls: 1, Input: 3}}, {Project: unknown, Model: "x", Totals: UsageTotals{Calls: 1, Input: 4}}}},
		{GroupProjectEffort, []UsageGroup{{Project: a, Effort: "high", Totals: UsageTotals{Calls: 1, Input: 1}}, {Project: b, Effort: "low", Totals: UsageTotals{Calls: 1, Input: 2}}, {Project: blank, Effort: "low", Totals: UsageTotals{Calls: 1, Input: 3}}, {Project: unknown, Effort: "high", Totals: UsageTotals{Calls: 1, Input: 4}}}},
	}
	for i, test := range tests {
		events = nil
		got, err := s.Usage(ctx, UsageQuery{Range: r, GroupBy: test.by})
		// Only the first query loads sessions, rows and the fallback lookup;
		// later queries reuse all three.
		wantEvents := []string(nil)
		if i == 0 {
			wantEvents = []string{"refresh", "requests", "one"}
		}
		if err != nil || got.Totals != (UsageTotals{Calls: 4, Input: 10}) || !reflect.DeepEqual(got.Groups, test.groups) || !reflect.DeepEqual(events, wantEvents) {
			t.Fatalf("project group %q = %+v, %v, events %v; want %+v", test.by, got, err, events, test.groups)
		}
	}
	cwd = "/new"
	events = nil
	if _, err := s.RefreshSessions(ctx); err != nil {
		t.Fatal(err)
	}
	got, err := s.Usage(ctx, UsageQuery{Range: r, Filter: UsageFilter{Projects: []ProjectSelection{{Kind: ProjectKindProject, Path: cwd}, {Kind: ProjectKindProjectless}}}})
	if err != nil || got.Totals != (UsageTotals{Calls: 2, Input: 4}) || !reflect.DeepEqual(events, []string{"refresh", "one"}) {
		t.Fatalf("project filter after refresh = %+v, %v, events %v", got, err, events)
	}
}

func TestUsageRows(t *testing.T) {
	ctx := context.Background()
	tm := time.Date(2026, 10, 11, 0, 0, 0, 0, time.UTC)
	full := magpie.UsageRequest{Time: tm.Add(time.Second), Agent: "codex", AgentName: "Codex", Provider: "p", Model: "m", Effort: "high", Session: "wrapper", NativeSession: "native", Subagent: "worker", ParentAgent: "parent", Kind: "kind", Source: "log", Status: 503, Error: "upstream error", Input: 1, Output: 2, CacheRead: 3, CacheWrite: 4, CacheWrite1h: 5, Reasoning: 6, Cost: 0.5, Priced: true}
	wantKeys := [][5]string{
		{"a", "", "", "", ""}, {"a", "a", "", "", ""}, {"a", "s", "a", "", ""},
		{"a", "s", "b", "a", ""}, {"a", "s", "b", "z", "a"}, {"a", "s", "b", "z", "b"}, {"b", "", "", "", ""},
	}
	rows := []magpie.UsageRequest{full}
	for i := len(wantKeys) - 1; i >= 0; i-- {
		key := wantKeys[i]
		rows = append(rows, magpie.UsageRequest{Time: tm, Agent: key[0], Provider: "p", Session: key[1], Subagent: key[2], Model: key[3], Effort: key[4]})
	}
	rows = append(rows, magpie.UsageRequest{Provider: "p", Rejected: true, Session: "broken"}, magpie.UsageRequest{Status: 400, Session: "broken"})
	refreshes := 0
	cwd := "/first"
	var events []string
	u := &sessionUpstream{
		sessions: func(context.Context, int) (magpie.SessionsResponse, error) {
			refreshes++
			events = append(events, "refresh")
			return magpie.SessionsResponse{Sessions: []magpie.Session{{Agent: "codex", ID: "native", Cwd: cwd}}}, nil
		},
		requests: func(_ context.Context, q magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
			if q != (magpie.UsageRequestsQuery{Period: "2026-10-11..2026-10-11", Limit: 500}) {
				t.Fatalf("UsageRows requests = %+v", q)
			}
			events = append(events, "requests")
			return magpie.UsageRequestsPage{Rows: rows, Total: len(rows)}, nil
		},
		session: func(_ context.Context, key string) (magpie.Session, error) {
			if key != "a:a" && key != "a:s" {
				t.Fatalf("rejected or already matched row triggered fallback %q", key)
			}
			return magpie.Session{}, &magpie.HTTPError{StatusCode: http.StatusNotFound}
		},
	}
	s := newService(u)
	q := UsageRowsQuery{Range: DateRange{From: "2026-10-11", To: "2026-10-11"}}
	got, err := s.UsageRows(ctx, q)
	if err != nil || len(got.Rows) != len(wantKeys)+1 {
		t.Fatalf("UsageRows = %+v, %v", got, err)
	}
	for i, key := range wantKeys {
		row := got.Rows[i]
		if row.Time != tm || [5]string{row.Agent, row.Session, row.Subagent, row.Model, row.Effort} != key {
			t.Fatalf("sorted row %d = %+v, want key %v", i, row, key)
		}
	}
	if got.Rows[0] != (UsageRow{Time: tm, Agent: "a", Provider: "p", Project: ProjectRef{Kind: ProjectKindUnknown}}) || got.Rows[1].ResolvedSession != (SessionRef{Agent: "a", ID: "a"}) {
		t.Fatalf("missing facts were filled or unresolved identity lost: %+v", got.Rows[:2])
	}
	wantFull := UsageRow{Time: tm.Add(time.Second), Agent: "codex", AgentName: "Codex", Provider: "p", Model: "m", Effort: "high", Session: "wrapper", NativeSession: "native", Subagent: "worker", ParentAgent: "parent", Kind: "kind", Origin: "log", Status: 503, Error: "upstream error", ResolvedSession: SessionRef{Agent: "codex", ID: "native", Matched: true}, Project: ProjectRef{Kind: ProjectKindProject, Path: cwd, Name: "first"}, Input: 1, Output: 2, CacheRead: 3, CacheWrite: 4, CacheWrite1h: 5, Reasoning: 6, Cost: 0.5, Priced: true}
	if got.Rows[len(wantKeys)] != wantFull {
		t.Fatalf("full row = %+v, want %+v", got.Rows[len(wantKeys)], wantFull)
	}
	cwd = "/fresh"
	if _, err := s.RefreshSessions(ctx); err != nil {
		t.Fatal(err)
	}
	q.Filter = UsageFilter{Agents: []string{"codex", "", "a", "codex"}, Models: []ModelSelection{{ProviderID: "p", Model: "z"}, {ProviderID: "p", Model: "m"}, {ProviderID: "p", Model: "m"}}, Projects: []ProjectSelection{{Kind: ProjectKindProject, Path: cwd}}}
	got, err = s.UsageRows(ctx, q)
	wantFull.Project = ProjectRef{Kind: ProjectKindProject, Path: cwd, Name: "fresh"}
	if err != nil || !reflect.DeepEqual(got.Rows, []UsageRow{wantFull}) || refreshes != 2 || !reflect.DeepEqual(events, []string{"refresh", "requests", "refresh"}) {
		t.Fatalf("filtered UsageRows = %+v, %v, refreshes %d, events %v", got, err, refreshes, events)
	}
}

func TestDecodeOfFollowsMagpieRules(t *testing.T) {
	base := DecodeInput{Output: 500, Millis: 10_000, TTFTMs: 2_000}
	tests := []struct {
		name   string
		mutate func(*DecodeInput)
		want   DecodeSample
		ok     bool
	}{
		{"plain reply counts from its first token", func(*DecodeInput) {}, DecodeSample{Tokens: 500, Ms: 8_000}, true},
		{"failed reply tells no speed", func(in *DecodeInput) { in.Failed = true }, DecodeSample{}, false},
		{"untimed reply tells no speed", func(in *DecodeInput) { in.TTFTMs = 0 }, DecodeSample{}, false},
		{"empty output tells no speed", func(in *DecodeInput) { in.Output = 0 }, DecodeSample{}, false},
		{"window under 100 ms tells no speed", func(in *DecodeInput) { in.Millis = 2_050 }, DecodeSample{}, false},
		{"over 10000 tokens a second tells no speed", func(in *DecodeInput) { in.Output = 100_000 }, DecodeSample{}, false},
		{"burst held back to the end tells no speed", func(in *DecodeInput) { in.FlowMs = 1_000 }, DecodeSample{}, false},
		{"flow close to the window still counts", func(in *DecodeInput) { in.FlowMs = 6_000 }, DecodeSample{Tokens: 500, Ms: 8_000}, true},
		{"reasoning reply counts its answer from the first text", func(in *DecodeInput) {
			in.Reasoning, in.FirstTextMs = 200, 4_000
		}, DecodeSample{Tokens: 300, Ms: 6_000}, true},
		{"reasoning reply without a first text tells no speed", func(in *DecodeInput) { in.Reasoning = 200 }, DecodeSample{}, false},
		{"answer far faster than its reasoning tells no speed", func(in *DecodeInput) {
			in.Reasoning, in.FirstTextMs = 10, 9_000
			in.Output, in.Millis = 5_010, 9_500
		}, DecodeSample{}, false},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			in := base
			test.mutate(&in)
			got, ok := DecodeOf(in)
			if ok != test.ok || got != test.want {
				t.Fatalf("DecodeOf(%+v) = %+v, %v; want %+v, %v", in, got, ok, test.want, test.ok)
			}
		})
	}
}

func TestUsageTotalsCountRetriedAndSpeed(t *testing.T) {
	rows := []magpie.UsageRequest{
		{Agent: "codex", Provider: "p", Model: "m", Session: "s", Output: 500, Millis: 10_000, TTFTMs: 2_000, Priced: true},
		{Agent: "codex", Provider: "p", Model: "m", Session: "s", Output: 100, Millis: 3_000, TTFTMs: 1_000, Tries: 2, Priced: true},
		{Agent: "codex", Provider: "p", Model: "m", Session: "s", Output: 100, Millis: 3_000, TTFTMs: 1_000, Tries: 3, Status: 502, Priced: true},
		{Agent: "codex", Provider: "p", Model: "m", Session: "s", Millis: 900, Status: 429, Error: "rate", Priced: true},
	}
	u := &sessionUpstream{
		requests: func(context.Context, magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
			return magpie.UsageRequestsPage{Rows: rows, Total: len(rows)}, nil
		},
		sessions: func(context.Context, int) (magpie.SessionsResponse, error) { return magpie.SessionsResponse{}, nil },
	}
	result, err := newService(u).Usage(context.Background(), UsageQuery{Range: DateRange{From: "2026-10-11", To: "2026-10-11"}})
	if err != nil {
		t.Fatal(err)
	}
	got := result.Totals
	if got.Calls != 4 || got.Errors != 2 {
		t.Fatalf("calls/errors = %d/%d, want 4/2", got.Calls, got.Errors)
	}
	if got.Retried != 1 {
		t.Fatalf("Retried = %d, want 1: a failed retry does not count as answered on a retry", got.Retried)
	}
	if got.DecodeN != 2 || got.DecodeOut != 600 || got.DecodeMs != 10_000 {
		t.Fatalf("decode = n %d, out %d, ms %d; want 2, 600, 10000", got.DecodeN, got.DecodeOut, got.DecodeMs)
	}
}
