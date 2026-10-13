package projection

import (
	"context"
	"database/sql"
	"math"
	"path/filepath"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/database"
	"github.com/Hogeexxl/Usagi/internal/query"
)

func TestSessionConsistencyUsesOneFilteredDatabaseDataset(t *testing.T) {
	db := openProjectionSessionDB(t)
	service := NewService(query.NewService(db, nil, time.UTC))
	rangeValue := query.DateRange{From: "2026-10-11", To: "2026-10-11"}
	addProjectionSession(t, db, "claude", "A", "/repo", "Claude", 7, 20, false)
	addProjectionSession(t, db, "claude", "empty", "", "Empty", 7, 18, false)
	addProjectionSession(t, db, "codex", "C", "/repo", "Codex", 7, 19, false)
	addProjectionSession(t, db, "codex", "subproject", "/repo/sub", "Nested project", 7, 18, false)
	addProjectionSession(t, db, "codex", "no-rows", "/repo", "No rows", 7, 17, true)
	addProjectionPrices(t, db)

	rows := []projectionLedgerRow{
		{at: projectionTime(9, 0), agent: "claude", provider: "provider-a", model: "claude-model", session: "A", input: 10, output: 5, cacheRead: 2, cacheWrite: 8, cacheWrite1h: 4},
		{at: projectionTime(10, 0), agent: "claude", provider: "provider-a", model: "worker-model", session: "A", subagent: "worker", input: 4, output: 2},
		{at: projectionTime(11, 0), agent: "claude", provider: "provider-a", model: "worker-model", session: "A", subagent: "worker", parent: "A", input: 2, output: 1},
		{at: projectionTime(12, 0), agent: "claude", provider: "provider-a", model: "nested-model", session: "A", subagent: "nested", parent: "worker", input: 3, output: 1},
		{at: projectionTime(9, 0), agent: "codex", provider: "provider-a", model: "codex-model", session: "C", kind: "review", input: 7, output: 2, cacheRead: 1, cacheWrite: 2},
		{at: projectionTime(10, 0), agent: "codex", provider: "provider-a", model: "spawn-model", session: "C", kind: "collab_spawn", input: 5, output: 1, cacheRead: 2, cacheWrite: 3, cacheWrite1h: 1},
		{at: projectionTime(11, 0), agent: "codex", provider: "provider-a", model: "spawn-model", session: "C", kind: "thread_spawn", input: 2, output: 1, cacheRead: 1, cacheWrite: 1},
		{at: projectionTime(12, 0), agent: "codex", provider: "provider-a", model: "spawn-model", session: "C", kind: "agent_job", input: 1},
		{at: projectionTime(13, 0), agent: "codex", provider: "provider-a", model: "other-model", session: "C", kind: "other", input: 3, output: 1},
		{at: projectionTime(14, 0), agent: "codex", provider: "provider-a", model: "failure-model", session: "virtual", status: 503, err: "upstream", input: 6},
		{at: projectionTime(14, 0), agent: "codex", provider: "provider-a", model: "subproject-model", session: "subproject", input: 99},
		{at: projectionTime(15, 0), agent: "codex", provider: "provider-a", model: "claude-model", input: 100},
		{at: projectionTime(15, 0), agent: "codex", provider: "provider-a", model: "claude-model", session: "A", rejected: true, input: 100},
		{at: projectionTime(15, 0), agent: "codex", model: "claude-model", session: "A", status: 400, input: 100},
	}
	for _, row := range rows {
		addProjectionRequest(t, db, row)
	}

	list, err := service.Sessions(context.Background(), ScopeQuery{Range: rangeValue})
	if err != nil {
		t.Fatalf("Sessions() error = %v", err)
	}
	items := make(map[ThreadKey]SessionListItemView, len(list.Items))
	for _, item := range list.Items {
		items[item.Thread.Key] = item
	}
	if len(list.Items) != 6 || list.Summary.RootSessionCount != 6 || len(items) != 6 {
		t.Fatalf("list/summary count = %d/%d, want six metadata and virtual sessions: %+v", len(list.Items), list.Summary.RootSessionCount, list.Items)
	}
	root := items[ThreadKey{Agent: "claude", SessionID: "A", ThreadID: "A"}]
	codex := items[ThreadKey{Agent: "codex", SessionID: "C", ThreadID: "C"}]
	virtual := items[ThreadKey{Agent: "codex", SessionID: "virtual", ThreadID: "virtual"}]
	noRows := items[ThreadKey{Agent: "codex", SessionID: "no-rows", ThreadID: "no-rows"}]
	if root.InclusiveUsage.Calls != 4 || root.InclusiveUsage.Input != 19 || root.InclusiveUsage.Output != 9 || root.InclusiveUsage.Unpriced != 3 || root.SubagentCount == nil || *root.SubagentCount != 2 {
		t.Fatalf("Claude inclusive rows/children = %+v", root)
	}
	if codex.InclusiveUsage.Calls != 5 || codex.InclusiveUsage.Input != 18 || codex.InclusiveUsage.Output != 5 || codex.SubagentUsage.Input != 8 || codex.SubagentUsage.Output != 2 || codex.SubagentCount != nil {
		t.Fatalf("Codex kinds/inclusive rows = %+v", codex)
	}
	if virtual.InclusiveUsage.Calls != 1 || virtual.InclusiveUsage.Input != 6 || virtual.Thread.Project.Kind != query.ProjectKindUnknown {
		t.Fatalf("unmatched failed row = %+v", virtual)
	}
	if noRows.HasUsageRows || noRows.LastActivity.Unix() != projectionTime(17, 0).Unix() || noRows.InclusiveUsage != (UsageView{}) {
		t.Fatalf("no-row metadata projection = %+v", noRows)
	}
	if list.Summary.Usage.Calls != 11 || list.Summary.Usage.Input != 142 || list.Summary.Usage.Output != 14 || list.Summary.Usage.Unpriced != 6 {
		t.Fatalf("summary did not add each accepted row once: %+v", list.Summary)
	}
	if list.Summary.Usage.CacheRead != 6 || list.Summary.Usage.CacheWrite != 14 || list.Summary.Usage.CacheWrite1h != 5 ||
		math.Abs(list.Summary.Usage.Cost-.003008) > 1e-9 || list.Summary.RootUnpricedSessionCount != 4 ||
		list.Summary.Usage.CacheHitRate == nil || math.Abs(*list.Summary.Usage.CacheHitRate-6.0/162.0) > 1e-12 {
		t.Fatalf("mixed priced/cache summary = %+v", list.Summary)
	}
	if list.Summary.SessionMedian == nil || *list.Summary.SessionMedian != 23 || list.Summary.SessionP90 == nil || *list.Summary.SessionP90 != 99 {
		t.Fatalf("nearest-rank session quantiles = %v/%v, want 23/99", list.Summary.SessionMedian, list.Summary.SessionP90)
	}

	models, err := service.ModelDistribution(context.Background(), ScopeQuery{Range: rangeValue})
	if err != nil {
		t.Fatalf("ModelDistribution() error = %v", err)
	}
	projects, err := service.ProjectDistribution(context.Background(), ScopeQuery{Range: rangeValue})
	if err != nil {
		t.Fatalf("ProjectDistribution() error = %v", err)
	}
	var itemsUsage, modelUsage, projectUsage UsageView
	unpricedItems := int64(0)
	for _, item := range list.Items {
		itemsUsage = mergeUsage(itemsUsage, item.InclusiveUsage)
		if item.InclusiveUsage.Unpriced > 0 {
			unpricedItems++
		}
	}
	for _, item := range models.Items {
		modelUsage = mergeUsage(modelUsage, item.Usage)
	}
	for _, item := range projects.Items {
		projectUsage = mergeUsage(projectUsage, item.Usage)
	}
	if itemsUsage.CacheRead != 6 || itemsUsage.Input+itemsUsage.CacheRead+itemsUsage.CacheWrite != 162 ||
		itemsUsage.CacheHitRate == nil || math.Abs(*itemsUsage.CacheHitRate-6.0/162.0) > 1e-12 {
		t.Fatalf("all-session cache fraction numerator/denominator = %d/%d rate=%v", itemsUsage.CacheRead,
			itemsUsage.Input+itemsUsage.CacheRead+itemsUsage.CacheWrite, itemsUsage.CacheHitRate)
	}
	if list.Summary.RootSessionCount != int64(len(list.Items)) || list.Summary.RootUnpricedSessionCount != unpricedItems ||
		!sameSessionUsage(list.Summary.Usage, itemsUsage) || !sameSessionUsage(list.Summary.Usage, modelUsage) || !sameSessionUsage(list.Summary.Usage, projectUsage) {
		t.Fatalf("list/summary/distributions consume different rows: summary=%+v items=%+v model=%+v project=%+v", list.Summary.Usage, itemsUsage, modelUsage, projectUsage)
	}

	filtered := ScopeQuery{Range: rangeValue, Filter: query.UsageFilter{
		Agents: []string{"claude-desktop", "codex"},
		Models: []query.ModelSelection{
			{ProviderID: "provider-a", Model: "claude-model"},
			{ProviderID: "provider-a", Model: "worker-model"},
			{ProviderID: "provider-a", Model: "spawn-model"},
			{ProviderID: "provider-a", Model: "subproject-model"},
		},
		Projects: []query.ProjectSelection{{Kind: query.ProjectKindProject, Path: "/repo"}},
	}}
	filteredList, err := service.Sessions(context.Background(), filtered)
	if err != nil {
		t.Fatalf("filtered Sessions() error = %v", err)
	}
	if len(filteredList.Items) != 2 || filteredList.Summary.Usage.Input != 24 || filteredList.Summary.Usage.Output != 10 || filteredList.Summary.Usage.Calls != 6 ||
		math.Abs(filteredList.Summary.Usage.Cost-.002528) > 1e-9 || filteredList.Summary.Usage.Unpriced != 2 || filteredList.Summary.RootUnpricedSessionCount != 1 ||
		filteredList.Summary.Usage.CacheRead != 5 || filteredList.Summary.Usage.CacheWrite != 12 || filteredList.Summary.Usage.CacheWrite1h != 5 ||
		filteredList.Summary.Usage.CacheHitRate == nil || math.Abs(*filteredList.Summary.Usage.CacheHitRate-5.0/41.0) > 1e-12 {
		t.Fatalf("multi-select family/model/path intersection = %+v", filteredList)
	}
	for _, item := range filteredList.Items {
		if item.Thread.Key.SessionID == "subproject" || item.Thread.Key.SessionID == "no-rows" || item.Thread.Key.SessionID == "virtual" {
			t.Fatalf("filter dimensions did not intersect/full-path match: %+v", filteredList.Items)
		}
	}
	filteredModels, err := service.ModelDistribution(context.Background(), filtered)
	if err != nil {
		t.Fatalf("filtered ModelDistribution() error = %v", err)
	}
	filteredProjects, err := service.ProjectDistribution(context.Background(), filtered)
	if err != nil {
		t.Fatalf("filtered ProjectDistribution() error = %v", err)
	}
	var filteredItemsUsage, filteredModelUsage, filteredProjectUsage UsageView
	for _, item := range filteredList.Items {
		filteredItemsUsage = mergeUsage(filteredItemsUsage, item.InclusiveUsage)
	}
	for _, item := range filteredModels.Items {
		filteredModelUsage = mergeUsage(filteredModelUsage, item.Usage)
	}
	for _, item := range filteredProjects.Items {
		filteredProjectUsage = mergeUsage(filteredProjectUsage, item.Usage)
	}
	if filteredItemsUsage.CacheRead != 5 || filteredItemsUsage.Input+filteredItemsUsage.CacheRead+filteredItemsUsage.CacheWrite != 41 ||
		filteredItemsUsage.CacheHitRate == nil || math.Abs(*filteredItemsUsage.CacheHitRate-5.0/41.0) > 1e-12 {
		t.Fatalf("filtered cache fraction numerator/denominator = %d/%d rate=%v", filteredItemsUsage.CacheRead,
			filteredItemsUsage.Input+filteredItemsUsage.CacheRead+filteredItemsUsage.CacheWrite, filteredItemsUsage.CacheHitRate)
	}
	if filteredList.Summary.RootSessionCount != int64(len(filteredList.Items)) ||
		!sameSessionUsage(filteredList.Summary.Usage, filteredItemsUsage) || !sameSessionUsage(filteredList.Summary.Usage, filteredModelUsage) ||
		!sameSessionUsage(filteredList.Summary.Usage, filteredProjectUsage) {
		t.Fatalf("filtered list/summary/distributions differ: summary=%+v items=%+v model=%+v project=%+v",
			filteredList.Summary.Usage, filteredItemsUsage, filteredModelUsage, filteredProjectUsage)
	}
}

func TestSessionFieldsAndQuantiles(t *testing.T) {
	db := openProjectionSessionDB(t)
	service := NewService(query.NewService(db, nil, time.UTC))
	rangeValue := query.DateRange{From: "2026-10-11", To: "2026-10-11"}
	addProjectionSession(t, db, "claude", "A", "/claude", "Claude A", 7, 17, false)
	addProjectionSession(t, db, "codex", "C", "/codex", "Codex C", 7, 17, false)
	addProjectionSession(t, db, "gemini", "G", "/gemini", "Gemini G", 7, 17, false)
	addProjectionSession(t, db, "codex", "D", "/codex", "Codex D", 7, 17, false)
	addProjectionSession(t, db, "claude", "empty", "", "Empty", 7, 18, false)
	addProjectionSession(t, db, "claude", "subonly", "/subonly", "Subagent only", 7, 18, false)

	rows := []projectionLedgerRow{
		{at: projectionTime(9, 0), agent: "claude", provider: "a", model: "beta", effort: "low", session: "A", input: 5, output: 5},
		{at: projectionTime(10, 0), agent: "claude", provider: "a", model: "alpha", effort: "high", session: "A", input: 5, output: 5, millis: 2000, ttftMS: 500},
		{at: projectionTime(10, 0), agent: "claude", provider: "a", model: "alpha", effort: "low", session: "A"},
		{at: projectionTime(11, 0), agent: "claude", provider: "a", model: "worker", session: "A", subagent: "B", input: 20},
		{at: projectionTime(12, 0), agent: "claude", provider: "a", model: "worker", session: "A", subagent: "C", input: 0, output: 0},
		{at: projectionTime(9, 0), agent: "codex", provider: "a", model: "review", session: "C", kind: "review", input: 6, output: 2},
		{at: projectionTime(10, 0), agent: "codex", provider: "a", model: "spawn", session: "C", kind: "collab_spawn", input: 3},
		{at: projectionTime(11, 0), agent: "codex", provider: "a", model: "spawn", session: "C", kind: "thread_spawn", input: 4},
		{at: projectionTime(12, 0), agent: "codex", provider: "a", model: "spawn", session: "C", kind: "agent_job", input: 5},
		{at: projectionTime(13, 0), agent: "codex", provider: "a", model: "spawn", session: "C", kind: "other", input: 0, output: 0},
		{at: projectionTime(9, 0), agent: "gemini", provider: "a", model: "gemini", session: "G", kind: "subagent", output: 4},
		{at: projectionTime(9, 0), agent: "codex", provider: "a", model: "codex", session: "D", input: 10},
		{at: projectionTime(10, 0), agent: "claude", provider: "a", model: "worker-only", session: "subonly", subagent: "worker", input: 5},
	}
	for _, row := range rows {
		addProjectionRequest(t, db, row)
	}

	list, err := service.Sessions(context.Background(), ScopeQuery{Range: rangeValue})
	if err != nil {
		t.Fatalf("Sessions() error = %v", err)
	}
	byID := make(map[string]SessionListItemView, len(list.Items))
	for _, item := range list.Items {
		byID[item.Thread.Key.SessionID] = item
	}
	claude := byID["A"]
	if !claude.HasUsageRows || claude.Thread.LastUsage != projectionTime(10, 0) || claude.LastActivity != projectionTime(12, 0) || claude.SubagentCount == nil || *claude.SubagentCount != 2 {
		t.Fatalf("Claude thread time/count fields = %+v", claude)
	}
	models := claude.Thread.ModelEffort
	if len(models) != 3 || models[0].Model != "alpha" || models[0].Effort != "high" || models[1].Model != "beta" || models[1].Effort != "low" ||
		models[2].Model != "alpha" || models[2].Effort != "low" || models[2].Usage.Input != 0 || models[2].Usage.Output != 0 ||
		claude.SelfUsage.OutputSpeed == nil || math.Abs(*claude.SelfUsage.OutputSpeed-5.0*1000.0/1500.0) > 1e-12 ||
		claude.SelfUsage.DecodeN != 1 || claude.SelfUsage.DecodeOut != 5 || claude.SelfUsage.DecodeMs != 1500 {
		t.Fatalf("main model/effort order and zero combination = %+v", models)
	}
	for _, model := range models {
		if model.Model == "worker" {
			t.Fatalf("subagent-only model entered main model list: %+v", models)
		}
	}
	codex := byID["C"]
	if codex.SubagentCount != nil || codex.SelfUsage.Input != 6 || codex.SubagentUsage.Input != 12 || codex.InclusiveUsage.Input != 18 ||
		len(codex.Thread.ModelEffort) != 2 || codex.Thread.ModelEffort[0].Model != "review" || codex.Thread.ModelEffort[1].Model != "spawn" ||
		codex.Thread.LastUsage != projectionTime(13, 0) || codex.LastActivity != projectionTime(13, 0) {
		t.Fatalf("Codex kind fields = %+v", codex)
	}
	gemini := byID["G"]
	if gemini.SubagentCount == nil || *gemini.SubagentCount != 0 || gemini.SelfUsage.Output != 4 || gemini.SubagentUsage != (UsageView{}) {
		t.Fatalf("other-agent self/count fields = %+v", gemini)
	}
	empty := byID["empty"]
	if empty.HasUsageRows || !empty.LastActivity.Equal(projectionTime(18, 0)) || empty.SubagentCount == nil || *empty.SubagentCount != 0 || empty.InclusiveUsage != (UsageView{}) {
		t.Fatalf("no-row fields = %+v", empty)
	}
	if list.Summary.RootSessionCount != 6 || list.Summary.SessionMedian == nil || *list.Summary.SessionMedian != 10 || list.Summary.SessionP90 == nil || *list.Summary.SessionP90 != 40 {
		t.Fatalf("nearest-rank median/P90 with zero session excluded = %+v", list.Summary)
	}
	detail, err := service.SessionDetail(context.Background(), ScopeQuery{Range: rangeValue}, empty.Thread.Key)
	if err != nil || detail.Thread.Key.SessionID != "empty" || detail.SelfUsage != (UsageView{}) || detail.InclusiveUsage != (UsageView{}) || len(detail.Children) != 0 {
		t.Fatalf("no-row detail = %+v, %v", detail, err)
	}
	rootDetail, err := service.SessionDetail(context.Background(), ScopeQuery{Range: rangeValue}, claude.Thread.Key)
	if err != nil || rootDetail.SelfUsage.OutputSpeed == nil || math.Abs(*rootDetail.SelfUsage.OutputSpeed-5.0*1000.0/1500.0) > 1e-12 {
		t.Fatalf("root detail lost output speed = %+v, %v", rootDetail, err)
	}
	subagentOnly, err := service.Sessions(context.Background(), ScopeQuery{Range: rangeValue, Filter: query.UsageFilter{
		Projects: []query.ProjectSelection{{Kind: query.ProjectKindProject, Path: "/subonly"}},
	}})
	if err != nil || len(subagentOnly.Items) != 1 || !subagentOnly.Items[0].HasUsageRows || subagentOnly.Items[0].InclusiveUsage.Input != 5 || len(subagentOnly.Items[0].Thread.ModelEffort) != 0 {
		t.Fatalf("subagent-only session main models = %+v, %v", subagentOnly, err)
	}
	projectless, err := service.Sessions(context.Background(), ScopeQuery{Range: rangeValue, Filter: query.UsageFilter{
		Agents: []string{"claude"}, Projects: []query.ProjectSelection{{Kind: query.ProjectKindProjectless}},
	}})
	if err != nil || len(projectless.Items) != 1 || projectless.Items[0].Thread.Key.SessionID != "empty" ||
		projectless.Summary.SessionMedian != nil || projectless.Summary.SessionP90 != nil {
		t.Fatalf("empty effective quantile set = %+v, %v", projectless, err)
	}
}

func TestSessionParentConflictsAreIsolatedAndFlat(t *testing.T) {
	db := openProjectionSessionDB(t)
	service := NewService(query.NewService(db, nil, time.UTC))
	rangeValue := query.DateRange{From: "2026-10-11", To: "2026-10-11"}
	addProjectionSession(t, db, "claude", "A", "/repo", "Claude", 7, 17, false)
	for _, row := range []projectionLedgerRow{
		{at: projectionTime(9, 0), agent: "claude", provider: "a", model: "worker", session: "A", subagent: "B", parent: "P", input: 1},
		{at: projectionTime(10, 0), agent: "claude", provider: "a", model: "worker", session: "A", subagent: "B", parent: "Q", input: 2},
		{at: projectionTime(11, 0), agent: "claude", provider: "a", model: "worker", session: "A", subagent: "B", input: 3},
		{at: projectionTime(12, 0), agent: "claude", provider: "a", model: "worker", session: "A", subagent: "C", parent: "P", input: 4},
	} {
		addProjectionRequest(t, db, row)
	}

	list, err := service.Sessions(context.Background(), ScopeQuery{Range: rangeValue})
	if err != nil || len(list.Items) != 1 || list.Items[0].InclusiveUsage.Input != 10 || list.Summary.Usage.Input != 10 {
		t.Fatalf("parent conflict affected flat totals: list=%+v err=%v", list, err)
	}
	projection, _, _, err := service.loadThreadProjection(context.Background(), ScopeQuery{Range: rangeValue})
	if err != nil {
		t.Fatalf("loadThreadProjection() error = %v", err)
	}
	threads := make(map[string]SessionThreadView, len(projection.Threads))
	for _, thread := range projection.Threads {
		threads[thread.Key.ThreadID] = thread
	}
	if threads["B"].ParentThreadID != "" || threads["C"].ParentThreadID != "P" {
		t.Fatalf("parent isolation = B:%q C:%q", threads["B"].ParentThreadID, threads["C"].ParentThreadID)
	}
	rootDetail, err := service.SessionDetail(context.Background(), ScopeQuery{Range: rangeValue}, ThreadKey{Agent: "claude", SessionID: "A", ThreadID: "A"})
	if err != nil || rootDetail.InclusiveUsage.Input != 10 || rootDetail.SelfUsage.Input != 0 || len(rootDetail.Children) != 2 {
		t.Fatalf("root detail recursively counted children: %+v, %v", rootDetail, err)
	}
}

func openProjectionSessionDB(t *testing.T) *sql.DB {
	t.Helper()
	db, err := database.Open(filepath.Join(t.TempDir(), "session-projection.sqlite"))
	if err != nil {
		t.Fatalf("open projection database: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return db
}

func addProjectionSession(t *testing.T, db *sql.DB, agent, id, cwd, title string, startHour, lastHour int, disappeared bool) {
	t.Helper()
	start, last := projectionTime(startHour, 0), projectionTime(lastHour, 0)
	if _, err := db.Exec(`INSERT INTO sessions (agent,id,cwd,title,start_ns,last_ns,first_seen_ns,last_seen_ns,disappeared)
		VALUES (?,?,?,?,?,?,?,?,?)`, agent, id, cwd, title, start.UnixNano(), last.UnixNano(), start.UnixNano(), last.UnixNano(), projectionBoolInt(disappeared)); err != nil {
		t.Fatalf("insert projection session %s/%s: %v", agent, id, err)
	}
}

type projectionLedgerRow struct {
	at                     time.Time
	agent, provider, model string
	effort, session        string
	subagent, parent       string
	kind, err              string
	status                 int
	rejected               bool
	input, output          int
	cacheRead, cacheWrite  int
	cacheWrite1h           int
	millis, ttftMS         int
	firstTextMS, flowMS    int
}

func addProjectionRequest(t *testing.T, db *sql.DB, row projectionLedgerRow) {
	t.Helper()
	if _, err := db.Exec(`INSERT INTO requests (t_ns,local_day,agent,provider,model,effort,session,subagent,parent_agent,kind,status,rejected,error,input,output,cache_read,cache_write,cache_write_1h,ms,ttft_ms,first_text_ms,flow_ms)
		VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`, row.at.UnixNano(), row.at.Format(time.DateOnly), row.agent, row.provider,
		row.model, row.effort, row.session, row.subagent, row.parent, row.kind, row.status, projectionBoolInt(row.rejected), row.err,
		row.input, row.output, row.cacheRead, row.cacheWrite, row.cacheWrite1h, row.millis, row.ttftMS, row.firstTextMS, row.flowMS); err != nil {
		t.Fatalf("insert projection request %s/%s/%s: %v", row.agent, row.session, row.model, err)
	}
}

func projectionBoolInt(value bool) int {
	if value {
		return 1
	}
	return 0
}

func projectionTime(hour, minute int) time.Time {
	return time.Date(2026, 10, 11, hour, minute, 0, 0, time.UTC)
}

func addProjectionPrices(t *testing.T, db *sql.DB) {
	t.Helper()
	result, err := db.Exec(`INSERT INTO price_versions (source,fetched_at_ns,effective_at_ns,provider_catalogs_json,maker_catalogs_json)
		VALUES ('fixture',0,0,'{"provider-a":["openai"]}','[]')`)
	if err != nil {
		t.Fatalf("insert projection price version: %v", err)
	}
	version, err := result.LastInsertId()
	if err != nil {
		t.Fatalf("read projection price version: %v", err)
	}
	for _, price := range []struct {
		model                                string
		input, output, cacheRead, cacheWrite int
		cacheWrite1h                         int
	}{
		{model: "claude-model", input: 100, output: 200, cacheRead: 50, cacheWrite: 25, cacheWrite1h: 10},
		{model: "codex-model", input: 40, output: 80, cacheRead: 20, cacheWrite: 10, cacheWrite1h: 5},
		{model: "spawn-model", input: 20, output: 40, cacheRead: 10, cacheWrite: 5, cacheWrite1h: 3},
	} {
		if _, err := db.Exec(`INSERT INTO prices (version_id,catalog_id,model_key,canonical_model_id,input,output,cache_read,cache_write,cache_write_1h)
			VALUES (?,'openai',?,'',?,?,?,?,?)`, version, price.model, price.input, price.output, price.cacheRead, price.cacheWrite, price.cacheWrite1h); err != nil {
			t.Fatalf("insert projection price %s: %v", price.model, err)
		}
	}
}

func sameSessionUsage(a, b UsageView) bool {
	if a.Calls != b.Calls || a.Errors != b.Errors || a.Input != b.Input || a.Output != b.Output || a.CacheRead != b.CacheRead ||
		a.CacheWrite != b.CacheWrite || a.CacheWrite1h != b.CacheWrite1h || a.Reasoning != b.Reasoning || a.Total != b.Total ||
		a.Unpriced != b.Unpriced || a.Retried != b.Retried || a.DecodeMs != b.DecodeMs || a.DecodeOut != b.DecodeOut || a.DecodeN != b.DecodeN ||
		math.Abs(a.Cost-b.Cost) > 1e-9 {
		return false
	}
	if a.CacheHitRate == nil || b.CacheHitRate == nil {
		return a.CacheHitRate == nil && b.CacheHitRate == nil
	}
	return math.Abs(*a.CacheHitRate-*b.CacheHitRate) <= 1e-12
}
