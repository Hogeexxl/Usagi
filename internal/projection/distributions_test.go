package projection

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"reflect"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/query"
)

func TestModelDistributionSeparatesSameModelAcrossProviders(t *testing.T) {
	rows := []magpie.UsageRequest{
		{Agent: "codex", Provider: "provider-a", Model: "gpt-x", Input: 1, Priced: true},
		{Agent: "codex", Provider: "provider-b", Model: "gpt-x", Input: 10, Priced: true},
	}
	var requests []magpie.UsageRequestsQuery
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/api/usage/requests" {
			t.Errorf("unexpected Magpie path %q", r.URL.Path)
			http.NotFound(w, r)
			return
		}
		provider, model := r.URL.Query().Get("provider"), r.URL.Query().Get("model")
		requests = append(requests, magpie.UsageRequestsQuery{
			Provider: provider, Model: model, Period: r.URL.Query().Get("period"),
		})
		var matching []magpie.UsageRequest
		for _, row := range rows {
			if row.Provider == provider && row.Model == model {
				matching = append(matching, row)
			}
		}
		if err := json.NewEncoder(w).Encode(magpie.UsageRequestsPage{Rows: matching, Total: len(matching)}); err != nil {
			t.Errorf("encode usage page: %v", err)
		}
	}))
	defer upstream.Close()

	client := magpie.NewClient(upstream.URL, upstream.Client())
	service := NewService(query.NewService(client))
	rangeValue := query.DateRange{From: "2026-10-01", To: "2026-10-11"}
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
	if len(requests) != 2 {
		t.Fatalf("UsageRequests query count = %d, want 2", len(requests))
	}
	for index, provider := range []string{"provider-a", "provider-b"} {
		if requests[index].Provider != provider || requests[index].Model != "gpt-x" || requests[index].Period != "2026-10-01..2026-10-11" {
			t.Fatalf("UsageRequests query %d = %+v", index, requests[index])
		}
	}
}

func TestDistribution(t *testing.T) {
	scope := ScopeQuery{
		Range:  query.DateRange{From: "2026-10-01", To: "2026-10-11"},
		Filter: query.UsageFilter{Agents: []string{"codex"}, Models: []query.ModelSelection{{ProviderID: "openai", Model: "gpt"}}},
	}
	call := 0
	fake := &fakeQuery{onUsage: func(_ context.Context, got query.UsageQuery) (query.UsageResult, error) {
		want := query.UsageQuery{Range: scope.Range, Filter: scope.Filter}
		call++
		switch call {
		case 1:
			want.GroupBy = query.GroupModel
			if !reflect.DeepEqual(got, want) {
				t.Fatalf("model query = %+v, want %+v", got, want)
			}
			return query.UsageResult{Groups: []query.UsageGroup{
				{Model: "zeta", Totals: query.UsageTotals{Input: 20, Output: 5, Cost: 0.25}},
				{Model: "mid", Totals: query.UsageTotals{Input: 10}},
				{Model: "alpha", Totals: query.UsageTotals{Input: 20, Output: 5, Unpriced: 1}},
			}}, nil
		case 2:
			want.GroupBy = query.GroupProject
			if !reflect.DeepEqual(got, want) {
				t.Fatalf("project query = %+v, want %+v", got, want)
			}
			return query.UsageResult{Groups: []query.UsageGroup{
				{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/z", Name: "z"}, Totals: query.UsageTotals{Input: 5}},
				{Project: query.ProjectRef{Kind: query.ProjectKindUnknown}, Totals: query.UsageTotals{Input: 5}},
				{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/large", Name: "large"}, Totals: query.UsageTotals{Input: 8}},
				{Project: query.ProjectRef{Kind: query.ProjectKindProjectless}, Totals: query.UsageTotals{Input: 5}},
				{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/a", Name: "a"}, Totals: query.UsageTotals{Input: 5}},
			}}, nil
		default:
			t.Fatalf("unexpected Usage call %d", call)
			return query.UsageResult{}, nil
		}
	}}

	service := newService(fake)
	models, err := service.ModelDistribution(context.Background(), scope)
	if err != nil {
		t.Fatalf("ModelDistribution() error = %v", err)
	}
	wantModels := ModelDistributionView{Items: []ModelDistributionItem{
		{Model: "alpha", Usage: usageFromTotals(query.UsageTotals{Input: 20, Output: 5, Unpriced: 1})},
		{Model: "zeta", Usage: usageFromTotals(query.UsageTotals{Input: 20, Output: 5, Cost: 0.25})},
		{Model: "mid", Usage: usageFromTotals(query.UsageTotals{Input: 10})},
	}}
	if !reflect.DeepEqual(models, wantModels) {
		t.Fatalf("ModelDistribution() = %+v, want %+v", models, wantModels)
	}

	projects, err := service.ProjectDistribution(context.Background(), scope)
	if err != nil {
		t.Fatalf("ProjectDistribution() error = %v", err)
	}
	wantProjects := ProjectDistributionView{Items: []ProjectDistributionItem{
		{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/large", Name: "large"}, Usage: usageFromTotals(query.UsageTotals{Input: 8})},
		{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/a", Name: "a"}, Usage: usageFromTotals(query.UsageTotals{Input: 5})},
		{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/z", Name: "z"}, Usage: usageFromTotals(query.UsageTotals{Input: 5})},
		{Project: query.ProjectRef{Kind: query.ProjectKindProjectless}, Usage: usageFromTotals(query.UsageTotals{Input: 5})},
		{Project: query.ProjectRef{Kind: query.ProjectKindUnknown}, Usage: usageFromTotals(query.UsageTotals{Input: 5})},
	}}
	if !reflect.DeepEqual(projects, wantProjects) {
		t.Fatalf("ProjectDistribution() = %+v, want %+v", projects, wantProjects)
	}
	if call != 2 {
		t.Fatalf("Usage calls = %d, want 2", call)
	}
}
