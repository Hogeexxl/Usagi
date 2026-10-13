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
	fallbackMetadata := sessions[sessionKey{Agent: "codex", ID: "fallback-id"}]
	if !fallbackMetadata.Matched || fallbackMetadata.HasRows {
		t.Fatalf("native-only row was incorrectly assigned to fallback metadata: %+v", fallbackMetadata)
	}
	if _, exists := sessions[sessionKey{Agent: "codex", ID: "outside"}]; exists {
		t.Fatal("out-of-range metadata session was included")
	}
	gone, exists := sessions[sessionKey{Agent: "codex", ID: "gone"}]
	if !exists || gone.HasRows || !gone.Matched || gone.Project.Path != "/gone" {
		t.Fatalf("disappeared metadata session = %+v, exists=%t", gone, exists)
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

type datasetRequest struct {
	at                     time.Time
	agent, provider, model string
	session, native        string
	status                 int
	err                    string
	rejected               bool
	input, output          int
	cacheRead, cacheWrite  int
	cacheWrite1h           int
}

func insertDatasetSession(t *testing.T, db *sql.DB, agent, id, cwd, title string, start, last time.Time, disappeared bool) {
	t.Helper()
	if _, err := db.Exec(`INSERT INTO sessions (agent,id,cwd,title,start_ns,last_ns,first_seen_ns,last_seen_ns,disappeared)
		VALUES (?,?,?,?,?,?,?,?,?)`, agent, id, cwd, title, start.UnixNano(), last.UnixNano(), start.UnixNano(), last.UnixNano(), boolInt(disappeared)); err != nil {
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
	if _, err := db.Exec(`INSERT INTO requests (t_ns,local_day,agent,provider,model,session,native_session,status,rejected,error,
		input,output,cache_read,cache_write,cache_write_1h)
		VALUES (?,'2026-10-11',?,?,?,?,?,?,?,?,?,?,?,?,?)`,
		request.at.UnixNano(), request.agent, request.provider, request.model, request.session, request.native,
		request.status, boolInt(request.rejected), request.err, request.input, request.output,
		request.cacheRead, request.cacheWrite, request.cacheWrite1h); err != nil {
		t.Fatalf("insert request %s/%s: %v", request.agent, request.session, err)
	}
}
