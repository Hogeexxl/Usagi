package query

import (
	"context"
	"database/sql"
	"math"
	"reflect"
	"testing"
	"time"
)

func TestSessionIdentityAndDatasetFilters(t *testing.T) {
	db := queryTestDB(t)
	s := newService(db, nil, time.UTC)
	rangeValue := DateRange{From: "2026-10-11", To: "2026-10-11"}
	timeAt := func(hour int) time.Time { return time.Date(2026, 10, 11, hour, 0, 0, 0, time.UTC) }

	insertDatasetSession(t, db, "claude-desktop", "shared", "/gateway", "Gateway title", timeAt(8), timeAt(15), false)
	insertDatasetSession(t, db, "claude", "shared", "", "", timeAt(7), timeAt(14), false)
	insertDatasetSession(t, db, "claude-desktop", "desktop-only", "/desktop", "Desktop only", timeAt(8), timeAt(13), false)
	insertDatasetSession(t, db, "codex", "project-one", "/repo/sub", "Project one", timeAt(8), timeAt(13), false)
	insertDatasetSession(t, db, "codex", "fallback-id", "/fallback", "Fallback metadata", timeAt(8), timeAt(13), false)
	insertDatasetSession(t, db, "codex", "gone", "/gone", "Disappeared", timeAt(6), timeAt(16), true)
	insertDatasetSession(t, db, "codex", "outside", "/outside", "Outside range", time.Date(2026, 10, 12, 8, 0, 0, 0, time.UTC), time.Date(2026, 10, 12, 13, 0, 0, 0, time.UTC), false)

	insertDatasetPrice(t, db)
	insertDatasetRequest(t, db, datasetRequest{at: timeAt(9), agent: "claude-desktop", provider: "provider-a", model: "fixture-model", session: "gateway-id", native: "shared", input: 10, output: 5, cacheRead: 2, cacheWrite: 8, cacheWrite1h: 4})
	insertDatasetRequest(t, db, datasetRequest{at: timeAt(10), agent: "claude", provider: "provider-a", model: "unlisted-model", session: "shared", input: 4, status: 503, err: "upstream failure"})
	insertDatasetRequest(t, db, datasetRequest{at: timeAt(10), agent: "claude-desktop", provider: "provider-a", model: "free-model", session: "desktop-only"})
	insertDatasetRequest(t, db, datasetRequest{at: timeAt(11), agent: "codex", provider: "provider-b", model: "other-model", session: "fallback-id", native: "native-only", input: 6})
	insertDatasetRequest(t, db, datasetRequest{at: timeAt(12), agent: "codex", provider: "provider-a", model: "fixture-model", session: "project-one", input: 3})
	insertDatasetRequest(t, db, datasetRequest{at: timeAt(12), agent: "codex", provider: "provider-a", model: "fixture-model", input: 100})
	insertDatasetRequest(t, db, datasetRequest{at: timeAt(12), agent: "codex", provider: "provider-a", model: "fixture-model", session: "project-one", input: 100, rejected: true})
	insertDatasetRequest(t, db, datasetRequest{at: timeAt(12), agent: "codex", model: "fixture-model", session: "project-one", status: 400, input: 100})

	got, err := s.SessionDataset(context.Background(), SessionQuery{Range: rangeValue})
	if err != nil {
		t.Fatalf("SessionDataset() error = %v", err)
	}
	sessions := make(map[sessionKey]SessionUsage, len(got.Sessions))
	for _, session := range got.Sessions {
		sessions[sessionKey{Agent: session.Agent, ID: session.ID}] = session
	}
	shared, ok := sessions[sessionKey{Agent: "claude", ID: "shared"}]
	if !ok || shared.Title != "" || shared.Cwd != "" || shared.Project.Kind != ProjectKindProjectless ||
		!shared.Start.Equal(timeAt(7)) || !shared.Last.Equal(timeAt(15)) || !shared.HasRows || !shared.Matched {
		t.Fatalf("merged Claude identity/metadata = %+v, want local blank fields and earliest/latest times", shared)
	}
	if shared.Totals.Calls != 2 || shared.Totals.Input != 14 || shared.Totals.Output != 5 || shared.Totals.Unpriced != 1 || math.Abs(shared.Totals.Cost-.000074) > 1e-9 {
		t.Fatalf("shared Claude rows = %+v, want failure retained and cost 0.000074", shared.Totals)
	}
	desktop := sessions[sessionKey{Agent: "claude", ID: "desktop-only"}]
	if desktop.Agent != "claude" || desktop.Title != "Desktop only" || desktop.Cwd != "/desktop" || !desktop.Matched || !desktop.HasRows {
		t.Fatalf("desktop-only identity = %+v", desktop)
	}
	if len(got.Rows) != 5 {
		t.Fatalf("SessionDataset row count = %d, want five accepted ID rows", len(got.Rows))
	}
	var zeroRow, nativeRow *UsageRow
	for i := range got.Rows {
		row := &got.Rows[i]
		if row.Model == "free-model" {
			zeroRow = row
		}
		if row.ResolvedSession.ID == "native-only" {
			nativeRow = row
		}
	}
	if zeroRow == nil || zeroRow.Priced || zeroRow.Cost != 0 {
		t.Fatalf("zero-token row must skip pricing: %+v", zeroRow)
	}
	if nativeRow == nil || nativeRow.ResolvedSession.Matched || nativeRow.Project.Kind != ProjectKindUnknown {
		t.Fatalf("native_session must win without fallback: %+v", nativeRow)
	}
	if _, exists := sessions[sessionKey{Agent: "codex", ID: "fallback-id"}]; exists {
		t.Fatal("Codex metadata without an effective request row was included")
	}
	if _, exists := sessions[sessionKey{Agent: "codex", ID: "outside"}]; exists {
		t.Fatal("out-of-range metadata session was included")
	}
	if _, exists := sessions[sessionKey{Agent: "codex", ID: "gone"}]; exists {
		t.Fatal("Codex metadata with only a differently resolved request was included")
	}
	if _, exists := sessions[sessionKey{Agent: "codex", ID: ""}]; exists {
		t.Fatal("request without a session ID produced a session")
	}

	filtered, err := s.SessionDataset(context.Background(), SessionQuery{Range: rangeValue, Filter: UsageFilter{
		Agents:   []string{"claude-desktop", "codex"},
		Models:   []ModelSelection{{ProviderID: "provider-a", Model: "fixture-model"}, {ProviderID: "provider-b", Model: "other-model"}},
		Projects: []ProjectSelection{{Kind: ProjectKindProject, Path: "/repo/sub"}},
	}})
	if err != nil {
		t.Fatalf("filtered SessionDataset() error = %v", err)
	}
	if len(filtered.Sessions) != 1 || filtered.Sessions[0].ID != "project-one" || len(filtered.Rows) != 1 || filtered.Rows[0].Input != 3 {
		t.Fatalf("multi-filter union/intersection = %+v", filtered)
	}

	options, err := s.FilterOptions(context.Background(), rangeValue)
	if err != nil {
		t.Fatalf("FilterOptions() error = %v", err)
	}
	if len(options.Agents) != 2 || options.Agents[0].Agent != "claude" || options.Agents[1].Agent != "codex" {
		t.Fatalf("Agent options from family sessions = %+v", options.Agents)
	}
	wantModels := []ModelOption{
		{Model: "fixture-model", ProviderID: "provider-a", ProviderName: "provider-a"},
		{Model: "free-model", ProviderID: "provider-a", ProviderName: "provider-a"},
		{Model: "unlisted-model", ProviderID: "provider-a", ProviderName: "provider-a"},
		{Model: "other-model", ProviderID: "provider-b", ProviderName: "provider-b"},
	}
	if !reflect.DeepEqual(options.Models, wantModels) {
		t.Fatalf("model options = %+v, want only row-backed pairs %+v", options.Models, wantModels)
	}
}

func TestCodexSessionsRequireEffectiveRequestRows(t *testing.T) {
	db := queryTestDB(t)
	s := newService(db, nil, time.UTC)
	day := time.Date(2026, 10, 11, 9, 0, 0, 0, time.UTC)
	rangeValue := DateRange{From: day.Format(time.DateOnly), To: day.Format(time.DateOnly)}

	insertDatasetSession(t, db, "codex", "orphan", "/orphan", "Orphan", day, day.Add(time.Hour), false)
	insertIdentityDatasetSession(t, db, "codex", "unknown-orphan", "/unknown", "Unknown orphan", day, day.Add(time.Hour), "unknown", "", "", false)
	insertDatasetSession(t, db, "claude", "empty", "", "Claude empty", day, day.Add(time.Hour), false)
	insertDatasetSession(t, db, "codex", "zero", "/zero", "Zero request", day.Add(-24*time.Hour), day.Add(-23*time.Hour), false)
	insertDatasetRequest(t, db, datasetRequest{at: day, agent: "codex", provider: "provider-a", model: "zero-model", session: "zero"})

	got, err := s.SessionDataset(context.Background(), SessionQuery{Range: rangeValue})
	if err != nil {
		t.Fatalf("SessionDataset() error = %v", err)
	}
	sessions := make(map[sessionKey]SessionUsage, len(got.Sessions))
	for _, session := range got.Sessions {
		sessions[sessionKey{Agent: session.Agent, ID: session.ID}] = session
	}
	zero := sessions[sessionKey{Agent: "codex", ID: "zero"}]
	if len(got.Rows) != 1 || zero.ID != "zero" || zero.Title != "Zero request" || !zero.HasRows || zero.Totals.Calls != 1 || zero.Totals.Input != 0 || zero.Totals.Output != 0 {
		t.Fatalf("effective zero-token Codex request = sessions %+v, rows %+v", got.Sessions, got.Rows)
	}
	if empty, ok := sessions[sessionKey{Agent: "claude", ID: "empty"}]; !ok || empty.HasRows {
		t.Fatalf("metadata-only Claude session should remain: %+v, exists=%t", empty, ok)
	}
	for _, id := range []string{"orphan", "unknown-orphan"} {
		if _, exists := sessions[sessionKey{Agent: "codex", ID: id}]; exists {
			t.Fatalf("metadata-only Codex session %q was included", id)
		}
	}

	filtered, err := s.SessionDataset(context.Background(), SessionQuery{Range: rangeValue, Filter: UsageFilter{
		Projects: []ProjectSelection{{Kind: ProjectKindProject, Path: "/orphan"}},
	}})
	if err != nil || len(filtered.Sessions) != 0 || len(filtered.Rows) != 0 {
		t.Fatalf("Codex metadata must not replace a request filtered by project: sessions=%+v rows=%+v err=%v", filtered.Sessions, filtered.Rows, err)
	}

	options, err := s.FilterOptions(context.Background(), rangeValue)
	if err != nil {
		t.Fatalf("FilterOptions() error = %v", err)
	}
	for _, project := range options.Projects {
		if project.Path == "/orphan" || project.Path == "/unknown" {
			t.Fatalf("metadata-only Codex project entered filter options: %+v", options.Projects)
		}
	}
	if len(options.Models) != 1 || options.Models[0].Model != "zero-model" || len(options.Agents) != 2 {
		t.Fatalf("filter options must follow request-backed Codex membership: %+v", options)
	}
}

func TestCodexSessionIdentityKeepsCanonicalRootAndOwnRows(t *testing.T) {
	db := queryTestDB(t)
	s := newService(db, nil, time.UTC)
	dayOne := time.Date(2026, 10, 11, 9, 0, 0, 0, time.UTC)
	dayTwo := dayOne.AddDate(0, 0, 1)
	rangeValue := DateRange{From: dayOne.Format(time.DateOnly), To: dayTwo.Format(time.DateOnly)}
	insertIdentityDatasetSession(t, db, "codex", "A", "/repo", "Main A", dayOne, dayTwo, "main", "", "A", false)
	insertIdentityDatasetSession(t, db, "codex", "B", "/repo", "Child B", dayOne, dayTwo, "subagent", "A", "A", false)
	insertIdentityDatasetSession(t, db, "codex", "C", "/repo", "Child C", dayOne, dayOne, "subagent", "A", "A", false)
	insertIdentityDatasetSession(t, db, "codex", "D", "/repo", "Nested D", dayTwo, dayTwo, "subagent", "B", "A", false)
	insertIdentityDatasetSession(t, db, "codex", "E", "/orphan", "No root", dayTwo, dayTwo, "subagent", "", "", false)
	insertIdentityDatasetSession(t, db, "codex", "M", "/repo", "Metadata only", dayTwo, dayTwo, "subagent", "A", "A", false)
	insertIdentityDatasetSession(t, db, "codex", "U", "/unknown", "Unknown metadata", dayTwo, dayTwo, "unknown", "", "", false)
	insertIdentityDatasetSession(t, db, "codex", "gone", "/legacy", "Legacy", dayOne, dayOne, "", "", "", false)
	insertDatasetPrice(t, db)

	requests := []datasetRequest{
		{at: dayOne, agent: "codex", provider: "provider-a", model: "fixture-model", effort: "high", session: "A", sessionRole: "main", native: "wrong-native", kind: "collab_spawn", input: 10, output: 5, cacheRead: 2, cacheWrite: 8, cacheWrite1h: 4},
		{at: dayOne.Add(time.Hour), agent: "codex", provider: "provider-a", model: "model-b", effort: "low", session: "A", sessionRole: "subagent", native: "wrong-native", subagent: "B", parent: "A", input: 4, output: 2},
		{at: dayOne.Add(2 * time.Hour), agent: "codex", provider: "provider-a", model: "model-c", effort: "high", session: "A", sessionRole: "subagent", subagent: "C", parent: "A", input: 5, output: 1},
		{at: dayTwo, agent: "codex", provider: "provider-a", model: "model-d", effort: "medium", session: "A", sessionRole: "subagent", subagent: "D", parent: "B", input: 6, output: 3},
		{at: dayTwo, agent: "codex", provider: "provider-a", model: "orphan-model", effort: "low", sessionRole: "subagent", subagent: "E", input: 7},
		{at: dayOne, agent: "codex", provider: "provider-a", model: "unknown-model", session: "A", sessionRole: "unknown", native: "must-not-win", subagent: "wrong-child", parent: "wrong-parent", kind: "collab_spawn", input: 8},
		{at: dayTwo, agent: "codex", provider: "provider-a", model: "missing-child-model", session: "unclassified-root", sessionRole: "subagent", native: "must-not-win", kind: "collab_spawn", input: 2},
		{at: dayTwo, agent: "codex", provider: "provider-a", model: "unknown-model", sessionRole: "unknown", input: 3},
		{at: dayOne, agent: "codex", provider: "provider-a", model: "fixture-model", session: "gone", native: "legacy-native", input: 10, output: 5, cacheRead: 2, cacheWrite: 8, cacheWrite1h: 4},
	}
	for _, request := range requests {
		insertDatasetRequest(t, db, request)
	}

	got, err := s.SessionDataset(context.Background(), SessionQuery{Range: rangeValue})
	if err != nil {
		t.Fatalf("SessionDataset() error = %v", err)
	}
	sessions := make(map[sessionKey]SessionUsage, len(got.Sessions))
	for _, session := range got.Sessions {
		sessions[sessionKey{Agent: session.Agent, ID: session.ID}] = session
	}
	main := sessions[sessionKey{Agent: "codex", ID: "A"}]
	if main.Role != "main" || main.RootSession != "A" || main.Totals.Input != 10 || len(main.ModelEffort) != 1 || main.ModelEffort[0].Model != "fixture-model" || math.Abs(main.Totals.Cost-.000074) > 1e-9 {
		t.Fatalf("main own identity/usage = %+v", main)
	}
	child := sessions[sessionKey{Agent: "codex", ID: "B"}]
	if child.Role != "subagent" || child.ParentSession != "A" || child.RootSession != "A" || child.Totals.Input != 4 || len(child.ModelEffort) != 1 || child.ModelEffort[0].Model != "model-b" {
		t.Fatalf("child own identity/usage = %+v", child)
	}
	if nested := sessions[sessionKey{Agent: "codex", ID: "D"}]; nested.Role != "subagent" || nested.ParentSession != "B" || nested.RootSession != "A" || nested.Totals.Input != 6 {
		t.Fatalf("nested child identity/usage = %+v", nested)
	}
	if orphan := sessions[sessionKey{Agent: "codex", ID: "E"}]; orphan.Role != "subagent" || orphan.RootSession != "" || orphan.ParentSession != "" || orphan.Totals.Input != 7 {
		t.Fatalf("child with unknown root = %+v", orphan)
	}
	if _, exists := sessions[sessionKey{Agent: "codex", ID: "M"}]; exists {
		t.Fatal("metadata-only Codex child was included")
	}
	if _, exists := sessions[sessionKey{Agent: "codex", ID: "U"}]; exists {
		t.Fatal("metadata-only Codex unknown session was included")
	}
	if unresolved := sessions[sessionKey{Agent: "codex", ID: "unclassified-root"}]; unresolved.Role != "unknown" || !unresolved.HasRows || unresolved.Totals.Input != 0 || len(unresolved.ModelEffort) != 0 {
		t.Fatalf("subagent request without a child ID must retain its raw session as unknown: %+v", unresolved)
	}
	if legacy := sessions[sessionKey{Agent: "codex", ID: "legacy-native"}]; legacy.Role != "" || legacy.Totals.Input != 10 {
		t.Fatalf("legacy native session compatibility = %+v", legacy)
	}
	if len(got.Rows) != len(requests)-1 {
		t.Fatalf("request row count = %d, want %d (the request without a session ID is excluded)", len(got.Rows), len(requests)-1)
	}
	var childRow, unknownAttached *UsageRow
	for i := range got.Rows {
		row := &got.Rows[i]
		switch row.Model {
		case "model-b":
			childRow = row
		case "unknown-model":
			if row.Session == "A" {
				unknownAttached = row
			}
		case "missing-child-model":
			if row.ResolvedSession.ID != "unclassified-root" || row.SessionRole != "unknown" {
				t.Fatalf("unclassified request identity = %+v", row)
			}
		}
	}
	if childRow == nil || childRow.ResolvedSession.ID != "A" || childRow.SessionRole != "subagent" || childRow.NativeSession != "wrong-native" {
		t.Fatalf("confirmed child row lost canonical root: %+v", childRow)
	}
	if unknownAttached == nil || unknownAttached.ResolvedSession.ID != "A" || unknownAttached.SessionRole != "unknown" {
		t.Fatalf("unknown row association = %+v", unknownAttached)
	}
	for _, row := range got.Rows {
		if row.Model == "unknown-model" && row.Session == "" {
			t.Fatalf("unknown request without a session ID entered session dataset: %+v", row)
		}
	}
	if legacyRow := findUsageRow(got.Rows, "legacy-native"); legacyRow == nil || legacyRow.ResolvedSession.ID != "legacy-native" {
		t.Fatalf("legacy native_session fallback = %+v", legacyRow)
	}

	filtered, err := s.SessionDataset(context.Background(), SessionQuery{Range: rangeValue, Filter: UsageFilter{
		Models: []ModelSelection{{ProviderID: "provider-a", Model: "model-d"}},
	}})
	if err != nil {
		t.Fatalf("filtered SessionDataset() error = %v", err)
	}
	if len(filtered.Rows) != 1 || filtered.Rows[0].Model != "model-d" || filtered.Rows[0].ResolvedSession.ID != "A" || len(filtered.Sessions) != 2 {
		t.Fatalf("range/model filtered child ownership = sessions %+v rows %+v", filtered.Sessions, filtered.Rows)
	}
}

func findUsageRow(rows []UsageRow, sessionID string) *UsageRow {
	for index := range rows {
		if rows[index].ResolvedSession.ID == sessionID {
			return &rows[index]
		}
	}
	return nil
}

type datasetRequest struct {
	at                       time.Time
	agent, provider, model   string
	effort                   string
	session, sessionRole     string
	native, subagent, parent string
	kind                     string
	status                   int
	err                      string
	rejected                 bool
	input, output            int
	cacheRead, cacheWrite    int
	cacheWrite1h             int
}

func insertDatasetSession(t *testing.T, db *sql.DB, agent, id, cwd, title string, start, last time.Time, disappeared bool) {
	insertIdentityDatasetSession(t, db, agent, id, cwd, title, start, last, "", "", "", disappeared)
}

func insertIdentityDatasetSession(t *testing.T, db *sql.DB, agent, id, cwd, title string, start, last time.Time, role, parent, root string, disappeared bool) {
	t.Helper()
	if _, err := db.Exec(`INSERT INTO sessions (agent,id,cwd,title,start_ns,last_ns,first_seen_ns,last_seen_ns,disappeared,role,parent_session,root_session)
		VALUES (?,?,?,?,?,?,?,?,?,?,?,?)`, agent, id, cwd, title, start.UnixNano(), last.UnixNano(), start.UnixNano(), last.UnixNano(), boolInt(disappeared), role, parent, root); err != nil {
		t.Fatalf("insert session %s/%s: %v", agent, id, err)
	}
}

func insertDatasetPrice(t *testing.T, db *sql.DB) {
	t.Helper()
	result, err := db.Exec(`INSERT INTO price_versions (source,fetched_at_ns,effective_at_ns,provider_catalogs_json,maker_catalogs_json)
		VALUES ('fixture',0,0,'{"provider-a":["openai"],"provider-b":["openai"]}','[]')`)
	if err != nil {
		t.Fatalf("insert price version: %v", err)
	}
	version, err := result.LastInsertId()
	if err != nil {
		t.Fatalf("read fixture price version: %v", err)
	}
	if _, err := db.Exec(`INSERT INTO prices (version_id,catalog_id,model_key,canonical_model_id,input,output,cache_read,cache_write,cache_write_1h)
		VALUES (?, 'openai', 'fixture-model', '', 2, 4, 1, 3, 5)`, version); err != nil {
		t.Fatalf("insert fixture price: %v", err)
	}
}

func insertDatasetRequest(t *testing.T, db *sql.DB, request datasetRequest) {
	t.Helper()
	if _, err := db.Exec(`INSERT INTO requests (t_ns,local_day,agent,provider,model,effort,kind,session,session_role,native_session,subagent,parent_agent,status,rejected,error,
		input,output,cache_read,cache_write,cache_write_1h)
		VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
		request.at.UnixNano(), request.at.Format(time.DateOnly), request.agent, request.provider, request.model, request.effort, request.kind,
		request.session, request.sessionRole, request.native, request.subagent, request.parent,
		request.status, boolInt(request.rejected), request.err, request.input, request.output,
		request.cacheRead, request.cacheWrite, request.cacheWrite1h); err != nil {
		t.Fatalf("insert request %s/%s: %v", request.agent, request.session, err)
	}
}
