package api

import (
	"database/sql"
	"encoding/json"
	"net/http"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"testing/fstest"

	"github.com/Hogeexxl/Usagi/internal/api/testsupport"
	"github.com/Hogeexxl/Usagi/internal/database"
	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/projection"
	"github.com/Hogeexxl/Usagi/internal/query"
)

func TestDatabaseReadEndpointsUseInitialSyncGateAndSessionDTO(t *testing.T) {
	t.Setenv("TZ", "UTC")
	db, err := database.Open(filepath.Join(t.TempDir(), "session-api.sqlite"))
	if err != nil {
		t.Fatalf("open API test database: %v", err)
	}
	defer db.Close()
	location, err := time.LoadLocation("UTC")
	if err != nil {
		t.Fatal(err)
	}
	service := query.NewService(db, nil, location)
	now := time.Now().In(location).Truncate(time.Second)
	insertAPISession(t, db, "codex", "codex-1", "/repo", "Codex", now)
	insertAPISession(t, db, "claude", "claude-1", "/repo", "Claude", now)
	insertAPISession(t, db, "claude", "no-rows", "", "Empty Claude", now)
	insertAPIRequest(t, db, now, "codex", "codex-1", "", "review-model", 5, 3)
	insertAPIRequest(t, db, now.Add(time.Second), "claude", "claude-1", "worker", "claude-model", 2, 1)

	config := DefaultConfig()
	config.StaticFS = fstest.MapFS{"index.html": &fstest.MapFile{Data: []byte("index")}}
	runtime := testsupport.NewFakeRuntime(magpie.RuntimeStatusStarting)
	server, err := NewServer(config, Dependencies{
		Projection: projection.NewService(service), Query: service, Revisions: NewRevisionService(0),
		RuntimeStatus: runtime, QuotaSnapshot: &QuotaSnapshot{}, ShutdownRequester: func() {},
	})
	if err != nil {
		t.Fatalf("NewServer() error = %v", err)
	}

	loading := request(server, http.MethodGet, "/api/usage/summary?range=today")
	if loading.Code != http.StatusServiceUnavailable || !strings.Contains(loading.Body.String(), ErrorDataLoading) {
		t.Fatalf("library read before initial sync = %d %s", loading.Code, loading.Body.String())
	}
	runtime.SetStatus(magpie.RuntimeStatusUnavailable)
	if _, err := db.Exec("UPDATE app_meta SET value = '1' WHERE key = 'initial_sync_complete'"); err != nil {
		t.Fatalf("mark initial sync complete: %v", err)
	}

	for _, target := range []string{
		"/api/usage/summary?range=today",
		"/api/usage/model-distribution?range=today",
		"/api/usage/projects?range=today",
		"/api/usage/filter-options?range=today",
		"/api/usage/sessions?range=today",
		"/api/usage/session-detail?range=today&agent=claude&session_id=no-rows&thread_id=no-rows",
		publicV1SummaryPath + "?range=today",
	} {
		response := request(server, http.MethodGet, target)
		if response.Code != http.StatusOK {
			t.Fatalf("library read with completed database and starting runtime, GET %s = %d %s", target, response.Code, response.Body.String())
		}
	}

	response := request(server, http.MethodGet, "/api/usage/sessions?range=today")
	var sessions sessionsResponseDTO
	if err := json.Unmarshal(response.Body.Bytes(), &sessions); err != nil {
		t.Fatalf("decode session list: %v", err)
	}
	if sessions.Summary.SessionCount != 3 || sessions.Summary.Usage.Calls != 2 || sessions.Summary.Usage.Input != 7 ||
		sessions.Summary.Usage.Output != 4 || sessions.Summary.SessionMedian == nil || *sessions.Summary.SessionMedian != 3 ||
		sessions.Summary.SessionP90 == nil || *sessions.Summary.SessionP90 != 8 {
		t.Fatalf("embedded session summary = %+v", sessions.Summary)
	}
	byID := make(map[string]sessionListItemDTO, len(sessions.Items))
	for _, item := range sessions.Items {
		byID[item.Thread.Key.SessionID] = item
	}
	codex := byID["codex-1"]
	claude := byID["claude-1"]
	noRows := byID["no-rows"]
	if len(sessions.Items) != 3 || !codex.HasUsageRows || codex.SubagentCount != nil || codex.LastActivityMS == nil ||
		!claude.HasUsageRows || claude.SubagentCount == nil || *claude.SubagentCount != 1 || claude.LastActivityMS == nil ||
		noRows.HasUsageRows || noRows.SubagentCount == nil || *noRows.SubagentCount != 0 || noRows.LastActivityMS == nil ||
		noRows.InclusiveUsage != (usageDTO{}) {
		t.Fatalf("session DTO nullable/count and no-row fields = %+v", sessions.Items)
	}

	publicResponse := request(server, http.MethodGet, publicV1SummaryPath+"?range=today")
	var public publicV1SummaryDTO
	if err := json.Unmarshal(publicResponse.Body.Bytes(), &public); err != nil {
		t.Fatalf("decode Public v1 summary: %v", err)
	}
	if public.Usage.SessionCount != 1 || public.Usage.InputTokens != 5 || public.Usage.OutputTokens != 3 || public.Usage.TotalTokens != 8 ||
		strings.Contains(publicResponse.Body.String(), "session_median") || strings.Contains(publicResponse.Body.String(), `"calls"`) {
		t.Fatalf("Public v1 leaked internal summary or included another agent: %s", publicResponse.Body.String())
	}
}

func insertAPISession(t *testing.T, db *sql.DB, agent, id, cwd, title string, last time.Time) {
	t.Helper()
	_, err := db.Exec(`INSERT INTO sessions (agent,id,cwd,title,start_ns,last_ns,first_seen_ns,last_seen_ns)
		VALUES (?,?,?,?,?,?,?,?)`, agent, id, cwd, title, last.UnixNano(), last.UnixNano(), last.UnixNano(), last.UnixNano())
	if err != nil {
		t.Fatalf("insert API session %s/%s: %v", agent, id, err)
	}
}

func insertAPIRequest(t *testing.T, db *sql.DB, at time.Time, agent, session, subagent, model string, input, output int) {
	t.Helper()
	_, err := db.Exec(`INSERT INTO requests (t_ns,local_day,agent,provider,model,session,subagent,status,input,output)
		VALUES (?,?,?,?,?,?,?,200,?,?)`, at.UnixNano(), at.Format(time.DateOnly), agent, "provider-a", model, session, subagent, input, output)
	if err != nil {
		t.Fatalf("insert API request %s/%s: %v", agent, session, err)
	}
}
