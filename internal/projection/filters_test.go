package projection

import (
	"context"
	"reflect"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/query"
)

func TestFilterOptions(t *testing.T) {
	calls := 0
	results := []query.FilterOptionsResult{
		{
			Agents: []query.AgentOption{{Agent: "z", DisplayName: "Z"}, {Agent: "a", DisplayName: "A"}},
			Models: []query.ModelOption{{Model: "z-model", ProviderID: "z", ProviderName: "Z"}, {Model: "a-model", ProviderID: "a", ProviderName: "A"}},
			Projects: []query.ProjectRef{
				{Kind: query.ProjectKindProject, Path: "/z", Name: "z"},
				{Kind: query.ProjectKindProjectless},
				{Kind: query.ProjectKindProject, Path: "/a", Name: "a"},
				{Kind: query.ProjectKindProject, Path: "/a", Name: "duplicate"},
				{Kind: query.ProjectKindUnknown},
				{Kind: query.ProjectKindUnknown},
			},
		},
		{Projects: []query.ProjectRef{{Kind: query.ProjectKindProject, Path: "/only", Name: "only"}}},
	}
	fake := &fakeQuery{onFilterOptions: func(context.Context) (query.FilterOptionsResult, error) {
		calls++
		if calls > len(results) {
			t.Fatalf("unexpected FilterOptions query %d", calls)
		}
		return results[calls-1], nil
	}}

	got, err := newService(fake).FilterOptions(context.Background())
	if err != nil {
		t.Fatalf("FilterOptions() error = %v", err)
	}
	want := FilterOptionsView{
		Agents: []query.AgentOption{{Agent: "z", DisplayName: "Z"}, {Agent: "a", DisplayName: "A"}},
		Models: []query.ModelOption{{Model: "z-model", ProviderID: "z", ProviderName: "Z"}, {Model: "a-model", ProviderID: "a", ProviderName: "A"}},
		Projects: []query.ProjectRef{
			{Kind: query.ProjectKindProject, Path: "/a", Name: "a"},
			{Kind: query.ProjectKindProject, Path: "/z", Name: "z"},
			{Kind: query.ProjectKindProjectless},
			{Kind: query.ProjectKindUnknown},
		},
	}
	if !reflect.DeepEqual(got, want) || calls != 1 {
		t.Fatalf("FilterOptions() = %+v, want %+v", got, want)
	}

	got, err = newService(fake).FilterOptions(context.Background())
	if err != nil {
		t.Fatalf("FilterOptions() concrete-only error = %v", err)
	}
	wantConcreteOnly := FilterOptionsView{Projects: []query.ProjectRef{
		{Kind: query.ProjectKindProject, Path: "/only", Name: "only"},
		{Kind: query.ProjectKindProjectless},
		{Kind: query.ProjectKindUnknown},
	}}
	if !reflect.DeepEqual(got, wantConcreteOnly) || calls != 2 {
		t.Fatalf("FilterOptions() concrete-only = %+v, want %+v; calls = %d", got, wantConcreteOnly, calls)
	}
}
