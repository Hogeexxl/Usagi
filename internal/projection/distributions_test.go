package projection

import (
	"context"
	"reflect"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/query"
)

func TestDistribution(t *testing.T) {
	scope := ScopeQuery{
		Range:  query.DateRange{From: "2026-10-01", To: "2026-10-11"},
		Filter: query.UsageFilter{Agents: []string{"codex"}, Models: []string{"gpt"}},
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
