package query

import (
	"context"
	"reflect"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

func (*sessionUpstream) Usage(context.Context, string) (magpie.UsageSummary, error) {
	return magpie.UsageSummary{}, nil
}

func TestRefreshSessionsReturnsSortedFactsFromSinglePublishedSnapshot(t *testing.T) {
	start := time.Date(2026, 10, 1, 1, 2, 3, 0, time.UTC)
	last := start.Add(time.Hour)
	response := magpie.SessionsResponse{Sessions: []magpie.Session{
		{Agent: "z-agent", ID: "same", Title: "Z", Cwd: "/z", Start: start, Last: last},
		{Agent: "a-agent", ID: "z", Title: "AZ", Cwd: "/az", Start: start, Last: last},
		{Agent: "a-agent", ID: "a", Title: "AA", Cwd: "/aa", Start: start, Last: last},
	}}
	calls := 0
	u := &sessionUpstream{sessions: func(_ context.Context, limit int) (magpie.SessionsResponse, error) {
		calls++
		if limit != 0 {
			t.Fatalf("Sessions limit = %d, want 0", limit)
		}
		return response, nil
	}}
	service := newService(queryTestDB(t), u, time.Local)

	facts, err := service.RefreshSessions(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	want := []SessionRefreshFact{
		{Agent: "a-agent", ID: "a", Title: "AA", Cwd: "/aa", Start: start, Last: last},
		{Agent: "a-agent", ID: "z", Title: "AZ", Cwd: "/az", Start: start, Last: last},
		{Agent: "z-agent", ID: "same", Title: "Z", Cwd: "/z", Start: start, Last: last},
	}
	if !reflect.DeepEqual(facts, want) {
		t.Fatalf("RefreshSessions facts = %+v, want %+v", facts, want)
	}
	if calls != 1 {
		t.Fatalf("Magpie Sessions calls = %d, want exactly one", calls)
	}
	snapshot := service.newWorkspace().snapshot
	for _, fact := range facts {
		got, ok := snapshot.sessions[sessionKey{Agent: fact.Agent, ID: fact.ID}]
		if !ok || got.Title != fact.Title || got.Cwd != fact.Cwd || !got.Start.Equal(fact.Start) || !got.Last.Equal(fact.Last) {
			t.Fatalf("published session for %s/%s = %+v, present %t", fact.Agent, fact.ID, got, ok)
		}
	}
}

func TestUsageSnapshotAlwaysRequestsPeriodAll(t *testing.T) {
	want := magpie.UsageSummary{UsageTotals: magpie.UsageTotals{Calls: 3, Input: 24, Cost: 0.75}}
	var periods []string
	u := &snapshotUpstream{usage: func(_ context.Context, period string) (magpie.UsageSummary, error) {
		periods = append(periods, period)
		return want, nil
	}}
	service := newService(queryTestDB(t), u, time.Local)
	got, err := service.UsageSnapshot(context.Background())
	if err != nil || !reflect.DeepEqual(got, want) {
		t.Fatalf("UsageSnapshot() = %+v, %v; want %+v", got, err, want)
	}
	if !reflect.DeepEqual(periods, []string{"all"}) {
		t.Fatalf("usage periods = %v, want [all]", periods)
	}
}

type snapshotUpstream struct {
	upstream
	usage func(context.Context, string) (magpie.UsageSummary, error)
}

func (u *snapshotUpstream) Usage(ctx context.Context, period string) (magpie.UsageSummary, error) {
	return u.usage(ctx, period)
}
