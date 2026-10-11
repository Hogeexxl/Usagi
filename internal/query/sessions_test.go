package query

import (
	"context"
	"errors"
	"net/http"
	"reflect"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type sessionUpstream struct {
	upstream
	sessions func(context.Context, int) (magpie.SessionsResponse, error)
	session  func(context.Context, string) (magpie.Session, error)
	requests func(context.Context, magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error)
	stats    func(context.Context, int) (magpie.SessionStats, error)
	quotas   func(context.Context, bool) (magpie.QuotasResponse, error)
	overview func(context.Context, magpie.SessionOverviewQuery) (magpie.SessionOverview, error)
}

func (u *sessionUpstream) SessionStats(ctx context.Context, days int) (magpie.SessionStats, error) {
	return u.stats(ctx, days)
}

func (u *sessionUpstream) Quotas(ctx context.Context, asked bool) (magpie.QuotasResponse, error) {
	return u.quotas(ctx, asked)
}

func (u *sessionUpstream) SessionOverview(ctx context.Context, q magpie.SessionOverviewQuery) (magpie.SessionOverview, error) {
	return u.overview(ctx, q)
}

func (u *sessionUpstream) UsageRequests(ctx context.Context, q magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
	return u.requests(ctx, q)
}

func (u *sessionUpstream) Sessions(ctx context.Context, limit int) (magpie.SessionsResponse, error) {
	return u.sessions(ctx, limit)
}

func (u *sessionUpstream) Session(ctx context.Context, key string) (magpie.Session, error) {
	return u.session(ctx, key)
}

func TestRefresh(t *testing.T) {
	ctx := context.Background()
	failure := errors.New("sessions unavailable")
	calls := 0
	u := &sessionUpstream{sessions: func(_ context.Context, limit int) (magpie.SessionsResponse, error) {
		if limit != 0 {
			t.Fatalf("Sessions limit = %d, want 0", limit)
		}
		calls++
		switch calls {
		case 1:
			return magpie.SessionsResponse{Sessions: []magpie.Session{
				{Agent: "codex", ID: "same", Cwd: "/old"},
				{Agent: "codex", ID: "removed"},
			}}, nil
		case 2:
			return magpie.SessionsResponse{Sessions: []magpie.Session{
				{Agent: "codex", ID: "same", Cwd: "/new"},
				{Agent: "claude", ID: "same", Cwd: "/claude"},
			}}, nil
		default:
			return magpie.SessionsResponse{}, failure
		}
	}}
	s := newService(u)
	if err := s.Refresh(ctx); err != nil {
		t.Fatal(err)
	}
	old := s.newWorkspace()
	if err := s.Refresh(ctx); err != nil {
		t.Fatal(err)
	}
	current := s.newWorkspace()
	want := map[sessionKey]magpie.Session{
		{Agent: "codex", ID: "same"}:  {Agent: "codex", ID: "same", Cwd: "/new"},
		{Agent: "claude", ID: "same"}: {Agent: "claude", ID: "same", Cwd: "/claude"},
	}
	if !reflect.DeepEqual(current.snapshot.sessions, want) {
		t.Fatalf("replacement snapshot = %+v, want %+v", current.snapshot.sessions, want)
	}
	if old.snapshot == current.snapshot || old.snapshot.sessions[sessionKey{Agent: "codex", ID: "same"}].Cwd != "/old" || len(old.snapshot.sessions) != 2 {
		t.Fatal("old workspace snapshot changed during refresh")
	}
	if err := s.Refresh(ctx); !errors.Is(err, failure) {
		t.Fatalf("Refresh error = %v, want wrapped upstream error", err)
	}
	if s.newWorkspace().snapshot != current.snapshot {
		t.Fatal("failed refresh replaced the snapshot")
	}
}

func TestSessionLookup(t *testing.T) {
	ctx := context.Background()
	var calls []string
	failure := &magpie.HTTPError{StatusCode: http.StatusInternalServerError}
	u := &sessionUpstream{
		sessions: func(context.Context, int) (magpie.SessionsResponse, error) {
			return magpie.SessionsResponse{Sessions: []magpie.Session{
				{Agent: "codex", ID: "wrapper", Cwd: "/wrapper"},
				{Agent: "codex", ID: "shared", Cwd: "/codex"},
			}}, nil
		},
		session: func(_ context.Context, key string) (magpie.Session, error) {
			calls = append(calls, key)
			switch key {
			case "codex:native":
				return magpie.Session{Agent: "codex", ID: "native", Cwd: "/native"}, nil
			case "claude:shared":
				return magpie.Session{Agent: "claude", ID: "shared", Cwd: "/claude"}, nil
			case "codex:broken":
				return magpie.Session{}, failure
			default:
				return magpie.Session{}, &magpie.HTTPError{StatusCode: http.StatusNotFound}
			}
		},
	}
	s := newService(u)
	if err := s.Refresh(ctx); err != nil {
		t.Fatal(err)
	}
	w := s.newWorkspace()
	checks := []struct {
		row magpie.UsageRequest
		ref SessionRef
		cwd string
	}{
		{magpie.UsageRequest{Agent: "codex", NativeSession: "native", Session: "wrapper"}, SessionRef{Agent: "codex", ID: "native", Matched: true}, "/native"},
		{magpie.UsageRequest{Agent: "codex", NativeSession: "missing", Session: "wrapper"}, SessionRef{Agent: "codex", ID: "wrapper", Matched: true}, "/wrapper"},
		{magpie.UsageRequest{Agent: "codex", NativeSession: "missing", Session: "missing"}, SessionRef{Agent: "codex", ID: "missing"}, ""},
		{magpie.UsageRequest{Agent: "codex", NativeSession: "missing", Session: "also-missing"}, SessionRef{Agent: "codex", ID: "missing"}, ""},
		{magpie.UsageRequest{Agent: "codex", Session: "shared"}, SessionRef{Agent: "codex", ID: "shared", Matched: true}, "/codex"},
		{magpie.UsageRequest{Agent: "claude", Session: "shared"}, SessionRef{Agent: "claude", ID: "shared", Matched: true}, "/claude"},
		{magpie.UsageRequest{Agent: "codex"}, SessionRef{}, ""},
	}
	for _, check := range checks {
		ref, session, err := w.resolveSession(ctx, check.row)
		if err != nil || ref != check.ref || session.Cwd != check.cwd {
			t.Fatalf("resolve %+v = %+v, %+v, %v; want %+v, cwd %q", check.row, ref, session, err, check.ref, check.cwd)
		}
	}
	_, _, err := w.resolveSession(ctx, magpie.UsageRequest{Agent: "codex", NativeSession: "broken", Session: "wrapper"})
	if !errors.Is(err, failure) {
		t.Fatalf("non-404 error = %v, want wrapped upstream error", err)
	}
	wantCalls := []string{"codex:native", "codex:missing", "codex:also-missing", "claude:shared", "codex:broken"}
	if !reflect.DeepEqual(calls, wantCalls) {
		t.Fatalf("lookup calls = %v, want %v", calls, wantCalls)
	}
	if len(w.snapshot.sessions) != 2 {
		t.Fatal("fallback patched the global snapshot")
	}
}

func TestProject(t *testing.T) {
	ctx := context.Background()
	const cwd = "/workspace/notes/../Usagi/"
	calls := map[string]int{}
	u := &sessionUpstream{
		sessions: func(context.Context, int) (magpie.SessionsResponse, error) {
			return magpie.SessionsResponse{Sessions: []magpie.Session{
				{Agent: "codex", ID: "known", Cwd: cwd},
				{Agent: "codex", ID: "projectless"},
			}}, nil
		},
		session: func(_ context.Context, key string) (magpie.Session, error) {
			calls[key]++
			if key == "codex:historical" {
				return magpie.Session{Agent: "codex", ID: "historical", Cwd: "/history"}, nil
			}
			return magpie.Session{}, &magpie.HTTPError{StatusCode: http.StatusNotFound}
		},
	}
	s := newService(u)
	if err := s.Refresh(ctx); err != nil {
		t.Fatal(err)
	}
	w := s.newWorkspace()
	checks := []struct {
		id      string
		project ProjectRef
	}{
		{"known", ProjectRef{Kind: ProjectKindProject, Path: cwd, Name: "Usagi"}},
		{"projectless", ProjectRef{Kind: ProjectKindProjectless}},
		{"historical", ProjectRef{Kind: ProjectKindProject, Path: "/history", Name: "history"}},
		{"missing", ProjectRef{Kind: ProjectKindUnknown}},
		{"historical", ProjectRef{Kind: ProjectKindProject, Path: "/history", Name: "history"}},
	}
	selections := []ProjectSelection{{Kind: ProjectKindProject, Path: cwd}, {Kind: ProjectKindProjectless}, {Kind: ProjectKindUnknown}}
	for _, check := range checks {
		ref, session, err := w.resolveSession(ctx, magpie.UsageRequest{Agent: "codex", Session: check.id})
		if err != nil {
			t.Fatal(err)
		}
		project := projectOf(session, ref.Matched)
		if project != check.project {
			t.Fatalf("project %s = %+v, want %+v", check.id, project, check.project)
		}
		if !matchesProjects(project, nil) || matchesProjects(project, selections) != (check.id != "historical") {
			t.Fatalf("project filter did not apply OR matching to %+v", project)
		}
	}
	if matchesProjects(checks[0].project, []ProjectSelection{{Kind: ProjectKindProject, Path: "/workspace/Usagi"}}) || matchesProjects(checks[1].project, []ProjectSelection{{Kind: ProjectKindUnknown}}) {
		t.Fatal("project filter used a cleaned path or merged special kinds")
	}
	if calls["codex:historical"] != 1 || calls["codex:missing"] != 1 || len(calls) != 2 {
		t.Fatalf("fallback calls = %v, want one per historical/missing identity", calls)
	}
	if len(s.newWorkspace().snapshot.sessions) != 2 {
		t.Fatal("historical lookup changed the published snapshot")
	}
	if _, _, err := s.newWorkspace().resolveSession(ctx, magpie.UsageRequest{Agent: "codex", Session: "historical"}); err != nil {
		t.Fatal(err)
	}
	if calls["codex:historical"] != 2 {
		t.Fatal("new workspace reused an earlier query's historical memo")
	}
}

func TestSessions(t *testing.T) {
	ctx := context.Background()
	tm := time.Date(2026, 10, 11, 0, 0, 0, 0, time.UTC)
	t1, t2, t3 := tm.Add(time.Hour), tm.Add(2*time.Hour), tm.Add(3*time.Hour)
	refreshes := 0
	var events []string
	u := &sessionUpstream{
		sessions: func(_ context.Context, limit int) (magpie.SessionsResponse, error) {
			if limit != 0 {
				t.Fatalf("Sessions limit = %d, want 0", limit)
			}
			refreshes++
			events = append(events, "refresh")
			cwd, title := "/old", "old"
			if refreshes == 2 {
				cwd, title = "/new", "new"
			}
			return magpie.SessionsResponse{Sessions: []magpie.Session{
				{Agent: "codex", ID: "a", Cwd: cwd, Title: title, Start: tm, Last: t3, Cost: 999, SessionTokens: magpie.SessionTokens{Input: 999}, Models: []magpie.SessionModel{{Model: "all-time", Cost: 999}}},
				{Agent: "codex", ID: "b"}, {Agent: "claude", ID: "a"},
			}}, nil
		},
		requests: func(_ context.Context, q magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
			events = append(events, "requests")
			if q != (magpie.UsageRequestsQuery{Period: "2026-10-11..2026-10-11", Limit: 500}) {
				t.Fatalf("Sessions Requests query = %+v", q)
			}
			return magpie.UsageRequestsPage{Total: 9, Rows: []magpie.UsageRequest{
				{Time: t3, Agent: "codex", Session: "a", Model: "z", Effort: "low", Input: 1, Priced: true},
				{Time: t1, Agent: "codex", NativeSession: "a", Session: "wrapper", Model: "a", Effort: "high", Input: 2, Priced: true},
				{Time: t2, Agent: "codex", Session: "a", Model: "a", Effort: "low", Input: 3, Priced: true},
				{Time: t3, Agent: "codex", Session: "b", Model: "m", Input: 4, Priced: true},
				{Time: t3, Agent: "claude", Session: "a", Model: "m", Input: 5, Priced: true},
				{Time: t2, Agent: "codex", NativeSession: "missing", Model: "m", Input: 6, Priced: true},
				{Time: t3, Agent: "codex", Input: 100, Priced: true},
				{Time: t3, Agent: "codex", Session: "broken", Provider: "p", Rejected: true, Input: 100},
				{Time: t3, Agent: "codex", Session: "broken", Status: 400, Input: 100},
			}}, nil
		},
		session: func(_ context.Context, key string) (magpie.Session, error) {
			if key != "codex:missing" {
				t.Fatalf("rejected or known Session reached fallback: %q", key)
			}
			return magpie.Session{}, &magpie.HTTPError{StatusCode: http.StatusNotFound}
		},
	}
	s := newService(u)
	q := SessionQuery{Range: DateRange{From: "2026-10-11", To: "2026-10-11"}}
	known := SessionUsage{Agent: "codex", ID: "a", Title: "old", Cwd: "/old", Project: ProjectRef{Kind: ProjectKindProject, Path: "/old", Name: "old"}, Start: tm, Last: t3, FirstUsage: t1, LastUsage: t3,
		ModelEffort: []ModelEffortUsage{{Model: "a", Effort: "high", Totals: UsageTotals{Calls: 1, Input: 2}}, {Model: "a", Effort: "low", Totals: UsageTotals{Calls: 1, Input: 3}}, {Model: "z", Effort: "low", Totals: UsageTotals{Calls: 1, Input: 1}}}, Totals: UsageTotals{Calls: 3, Input: 6}}
	unknown := SessionUsage{Agent: "codex", ID: "missing", Project: ProjectRef{Kind: ProjectKindUnknown}, FirstUsage: t2, LastUsage: t2, ModelEffort: []ModelEffortUsage{{Model: "m", Totals: UsageTotals{Calls: 1, Input: 6}}}, Totals: UsageTotals{Calls: 1, Input: 6}}
	want := []SessionUsage{
		{Agent: "claude", ID: "a", Project: ProjectRef{Kind: ProjectKindProjectless}, FirstUsage: t3, LastUsage: t3, ModelEffort: []ModelEffortUsage{{Model: "m", Totals: UsageTotals{Calls: 1, Input: 5}}}, Totals: UsageTotals{Calls: 1, Input: 5}},
		known,
		{Agent: "codex", ID: "b", Project: ProjectRef{Kind: ProjectKindProjectless}, FirstUsage: t3, LastUsage: t3, ModelEffort: []ModelEffortUsage{{Model: "m", Totals: UsageTotals{Calls: 1, Input: 4}}}, Totals: UsageTotals{Calls: 1, Input: 4}},
		unknown,
	}
	got, err := s.Sessions(ctx, q)
	if err != nil || !reflect.DeepEqual(got.Sessions, want) {
		t.Fatalf("Sessions = %+v, %v; want %+v", got, err, want)
	}
	q.Filter.Projects = []ProjectSelection{{Kind: ProjectKindProject, Path: "/new"}, {Kind: ProjectKindUnknown}}
	known.Title, known.Cwd = "new", "/new"
	known.Project = ProjectRef{Kind: ProjectKindProject, Path: "/new", Name: "new"}
	got, err = s.Sessions(ctx, q)
	if err != nil || !reflect.DeepEqual(got.Sessions, []SessionUsage{known, unknown}) || refreshes != 2 || !reflect.DeepEqual(events, []string{"refresh", "requests", "refresh", "requests"}) {
		t.Fatalf("Sessions after refresh/filter = %+v, %v, refreshes %d, events %v", got, err, refreshes, events)
	}
}

func TestSessionsInputAndLookupErrors(t *testing.T) {
	ctx := context.Background()
	r := DateRange{From: "2026-10-11", To: "2026-10-11"}
	s := newService(&sessionUpstream{})
	if _, err := s.Sessions(ctx, SessionQuery{Range: DateRange{From: r.From, To: "bad"}}); !errors.Is(err, ErrInvalidRange) {
		t.Fatalf("Sessions range error = %v", err)
	}
	if _, err := s.Sessions(ctx, SessionQuery{Range: r, Filter: UsageFilter{Projects: []ProjectSelection{{Kind: ProjectKindUnknown, Path: "/bad"}}}}); !errors.Is(err, ErrInvalidFilter) {
		t.Fatalf("Sessions filter error = %v", err)
	}
	failures := []error{&magpie.HTTPError{StatusCode: http.StatusInternalServerError}, errors.New("session transport failed")}
	for i, failure := range failures {
		u := &sessionUpstream{
			sessions: func(context.Context, int) (magpie.SessionsResponse, error) {
				return magpie.SessionsResponse{Sessions: []magpie.Session{{Agent: "codex", ID: "known"}}}, nil
			},
			requests: func(context.Context, magpie.UsageRequestsQuery) (magpie.UsageRequestsPage, error) {
				return magpie.UsageRequestsPage{Total: 1, Rows: []magpie.UsageRequest{{Agent: "codex", NativeSession: "broken", Session: "known"}}}, nil
			},
			session: func(_ context.Context, key string) (magpie.Session, error) {
				if key != "codex:broken" {
					t.Fatalf("lookup continued after non-404: %q", key)
				}
				return magpie.Session{}, failure
			},
		}
		s = newService(u)
		if i == 0 {
			got, err := s.Sessions(ctx, SessionQuery{Range: r})
			if !errors.Is(err, failure) || !reflect.DeepEqual(got, SessionResult{}) {
				t.Fatalf("Sessions 500 = %+v, %v; want wrapped failure", got, err)
			}
		} else {
			got, err := s.UsageRows(ctx, UsageRowsQuery{Range: r})
			if !errors.Is(err, failure) || !reflect.DeepEqual(got, UsageRowsResult{}) {
				t.Fatalf("UsageRows transport error = %+v, %v; want wrapped failure", got, err)
			}
		}
	}
}
