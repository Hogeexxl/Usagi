package projection

import (
	"context"
	"errors"
	"reflect"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/query"
)

func TestSessionProjectionClaudeThreads(t *testing.T) {
	project := query.ProjectRef{Kind: query.ProjectKindProject, Path: "/workspace/project", Name: "project"}
	mainTime := testTime(12, 0)
	bTime := testTime(11, 0)
	cTime := testTime(10, 0)
	rows := []query.UsageRow{
		testUsageRow("claude", "A", "", "", mainTime, "main-model", "high", 10, 2, 0, 0, "", ""),
		testUsageRow("claude", "A", "B", "", bTime, "worker", "low", 4, 1, 0, 0, "", ""),
		testUsageRow("claude", "A", "B", "", bTime.Add(time.Minute), "worker", "low", 6, 3, 0, 0, "", ""),
		testUsageRow("claude", "A", "C", "B", cTime, "nested", "medium", 3, 2, 0, 0, "", ""),
	}
	for i := range rows {
		rows[i].Project = project
	}
	session := query.SessionUsage{
		Agent: "claude", ID: "A", Title: "Claude session", Project: project,
		HasRows: true, Matched: true,
		FirstUsage: cTime, LastUsage: mainTime,
		Totals:      query.UsageTotals{Calls: 4, Input: 23, Output: 8, Cost: 1.05},
		ModelEffort: []query.ModelEffortUsage{{Model: "main-model", Effort: "high", Totals: query.UsageTotals{Input: 23}}},
	}
	service := newService(&fakeQuery{dataset: query.SessionDatasetResult{Sessions: []query.SessionUsage{session}, Rows: rows}})

	list, err := service.Sessions(context.Background(), ScopeQuery{})
	if err != nil {
		t.Fatalf("Sessions() error = %v", err)
	}
	if len(list.Items) != 1 {
		t.Fatalf("main session item count = %d, want 1: %+v", len(list.Items), list.Items)
	}
	if list.Items[0].Thread.Type != ThreadMain || list.Items[0].SubagentCount == nil || *list.Items[0].SubagentCount != 2 || list.Items[0].SelfUsage.Input != 10 || list.Items[0].SubagentUsage.Input != 13 || list.Items[0].InclusiveUsage.Total != 31 {
		t.Fatalf("main session list rollup = %+v", list.Items[0])
	}
	threadsProjection, _, _, err := service.loadThreadProjection(context.Background(), ScopeQuery{})
	if err != nil {
		t.Fatalf("loadThreadProjection() error = %v", err)
	}
	projectedThreads := threadsProjection.Threads
	if len(projectedThreads) != 3 {
		t.Fatalf("projected thread count = %d, want 3: %+v", len(projectedThreads), projectedThreads)
	}
	wantOrder := []string{"A", "B", "C"}
	threads := make(map[string]SessionThreadView, len(projectedThreads))
	for i, thread := range projectedThreads {
		if thread.Key.ThreadID != wantOrder[i] {
			t.Fatalf("thread %d ID = %q, want %q", i, thread.Key.ThreadID, wantOrder[i])
		}
		threads[thread.Key.ThreadID] = thread
	}
	main, worker, nested := threads["A"], threads["B"], threads["C"]
	if main.Type != ThreadMain || worker.Type != ThreadSubagent || nested.Type != ThreadSubagent {
		t.Fatalf("thread types = %q, %q, %q", main.Type, worker.Type, nested.Type)
	}
	if worker.ParentThreadID != "" || nested.ParentThreadID != "B" {
		t.Fatalf("parents = %q, %q; want empty and B", worker.ParentThreadID, nested.ParentThreadID)
	}
	if worker.Title != "" || worker.Project != project {
		t.Fatalf("subagent title/project = %q / %+v", worker.Title, worker.Project)
	}
	if main.Usage.Input != 10 || main.Usage.Output != 2 || main.Usage.Calls != 1 {
		t.Fatalf("main self usage = %+v", main.Usage)
	}
	if len(main.ModelEffort) != 1 || main.ModelEffort[0].Model != "main-model" || main.ModelEffort[0].Usage.Input != 10 {
		t.Fatalf("main self model usage = %+v", main.ModelEffort)
	}
	if len(worker.ModelEffort) != 1 || worker.ModelEffort[0].Model != "worker" || worker.ModelEffort[0].Usage.Calls != 2 || worker.ModelEffort[0].Usage.Input != 10 {
		t.Fatalf("subagent model usage = %+v", worker.ModelEffort)
	}
	for _, thread := range projectedThreads {
		for _, modelEffort := range thread.ModelEffort {
			if modelEffort.CompactionTokens != nil {
				t.Fatalf("Claude compaction tokens = %d, want nil", *modelEffort.CompactionTokens)
			}
		}
		compaction := "nil"
		if len(thread.ModelEffort) > 0 && thread.ModelEffort[0].CompactionTokens != nil {
			compaction = "known"
		}
		t.Logf("Claude list agent=%s session=%s thread=%s type=%q self_total=%d parent=%q compaction=%s", thread.Key.Agent, thread.Key.SessionID, thread.Key.ThreadID, thread.Type, thread.Usage.Total, thread.ParentThreadID, compaction)
	}

	mainDetail, err := service.SessionDetail(context.Background(), ScopeQuery{}, main.Key)
	if err != nil {
		t.Fatalf("main SessionDetail() error = %v", err)
	}
	if len(mainDetail.Children) != 2 || mainDetail.Children[0].Key.ThreadID != "B" || mainDetail.Children[1].Key.ThreadID != "C" {
		t.Fatalf("main children = %+v", mainDetail.Children)
	}
	if mainDetail.SelfUsage.Input != 10 || mainDetail.SubagentUsage.Input != 13 || mainDetail.SubagentUsage.Output != 6 || mainDetail.InclusiveUsage.Total != 31 {
		t.Fatalf("main detail usage = self %+v, subagents %+v, inclusive %+v", mainDetail.SelfUsage, mainDetail.SubagentUsage, mainDetail.InclusiveUsage)
	}
	mainChildIDs := make([]string, len(mainDetail.Children))
	for i, child := range mainDetail.Children {
		mainChildIDs[i] = child.Key.ThreadID
	}
	t.Logf("Claude detail thread=%s self_total=%d subagent_total=%d inclusive_total=%d children=%v", mainDetail.Thread.Key.ThreadID, mainDetail.SelfUsage.Total, mainDetail.SubagentUsage.Total, mainDetail.InclusiveUsage.Total, mainChildIDs)

	workerDetail, err := service.SessionDetail(context.Background(), ScopeQuery{}, worker.Key)
	if err != nil {
		t.Fatalf("subagent SessionDetail() error = %v", err)
	}
	if len(workerDetail.Children) != 1 || workerDetail.Children[0].Key.ThreadID != "C" || workerDetail.SubagentUsage.Input != 3 || workerDetail.InclusiveUsage.Total != 19 {
		t.Fatalf("subagent detail = %+v", workerDetail)
	}
}

func TestSessionProjectionClaudeWithoutMainRows(t *testing.T) {
	project := query.ProjectRef{Kind: query.ProjectKindProject, Path: "/workspace/project", Name: "project"}
	first, last := testTime(8, 0), testTime(11, 0)
	service := newService(&fakeQuery{
		dataset: query.SessionDatasetResult{Sessions: []query.SessionUsage{{
			Agent: "claude", ID: "A", Title: "Claude session", Project: project,
			Start: first, Last: last, HasRows: true, Matched: true,
		}}, Rows: []query.UsageRow{
			testUsageRow("claude", "A", "B", "", testTime(11, 0), "worker", "low", 5, 1, 0, 0, "", ""),
		}},
	})

	list, err := service.Sessions(context.Background(), ScopeQuery{})
	if err != nil {
		t.Fatalf("Sessions() error = %v", err)
	}
	if len(list.Items) != 1 {
		t.Fatalf("main session item count = %d, want 1", len(list.Items))
	}
	main := list.Items[0].Thread
	if main.Key.ThreadID != "A" || main.Type != ThreadMain || main.Usage != (UsageView{}) {
		t.Fatalf("main without self rows = %+v", main)
	}
	if !main.FirstUsage.IsZero() || !main.LastUsage.IsZero() || !list.Items[0].LastActivity.Equal(testTime(11, 0)) {
		t.Fatalf("main own thread activity = %v..%v, session last activity = %v", main.FirstUsage, main.LastUsage, list.Items[0].LastActivity)
	}
	if list.Items[0].SubagentCount == nil || *list.Items[0].SubagentCount != 1 || list.Items[0].SubagentUsage.Input != 5 || list.Items[0].InclusiveUsage.Input != 5 {
		t.Fatalf("subagent rollup = %+v", list.Items[0])
	}
}

func TestSessionFieldsCompactionTokens(t *testing.T) {
	project := query.ProjectRef{Kind: query.ProjectKindProject, Path: "/workspace/codex", Name: "codex"}
	session := query.SessionUsage{
		Agent: "codex", ID: "X", Title: "Codex session", Project: project,
		HasRows: true, Matched: true,
		FirstUsage: testTime(9, 0), LastUsage: testTime(10, 0),
		Totals: query.UsageTotals{Calls: 6, Input: 21, Output: 7, CacheRead: 1, CacheWrite: 4, Cost: 1.5},
		ModelEffort: []query.ModelEffortUsage{
			{Model: "gpt", Effort: "high", Totals: query.UsageTotals{Calls: 2, Input: 12, Output: 4, CacheWrite: 4, Cost: 0.75}},
			{Model: "gpt", Effort: "low", Totals: query.UsageTotals{Calls: 1, Input: 3, Output: 1, Cost: 0.125}},
			{Model: "o3", Effort: "high", Totals: query.UsageTotals{Calls: 3, Input: 6, Output: 2, CacheRead: 1, Cost: 0.625}},
		},
	}
	rows := []query.UsageRow{
		testUsageRow("codex", "X", "", "", testTime(9, 0), "gpt", "high", 10, 3, 0, 0, "", "gateway"),
		testUsageRow("codex", "X", "", "", testTime(9, 10), "gpt", "high", 2, 1, 0, 4, "compact", "gateway"),
		testUsageRow("codex", "X", "", "", testTime(9, 20), "gpt", "low", 3, 1, 0, 0, "", "gateway"),
		testUsageRow("codex", "X", "", "", testTime(9, 30), "o3", "high", 4, 1, 0, 0, "", "gateway"),
		testUsageRow("codex", "X", "", "", testTime(9, 40), "o3", "high", 1, 1, 1, 0, "compact", "gateway"),
		testUsageRow("codex", "X", "", "", testTime(9, 50), "o3", "high", 1, 0, 0, 0, "", "log"),
	}
	rows[0].Cost = 0.25
	rows[1].Cost = 0.5
	rows[2].Cost = 0.125
	rows[3].Cost = 0.25
	rows[4].Cost = 0.125
	rows[5].Cost = 0.25
	service := newService(&fakeQuery{dataset: query.SessionDatasetResult{Sessions: []query.SessionUsage{session}, Rows: rows}})
	list, err := service.Sessions(context.Background(), ScopeQuery{})
	if err != nil {
		t.Fatalf("Sessions() error = %v", err)
	}
	if len(list.Items) != 1 {
		t.Fatalf("session item count = %d, want 1", len(list.Items))
	}
	thread := list.Items[0].Thread
	wantUsage := usageFromTotals(query.UsageTotals{Calls: 6, Input: 21, Output: 7, CacheRead: 1, CacheWrite: 4, Cost: 1.5})
	if thread.Type != ThreadMain || thread.ParentThreadID != "" || !reflect.DeepEqual(thread.Usage, wantUsage) || thread.Usage.Total != 33 || thread.Usage.Cost != 1.5 {
		t.Fatalf("Codex main-thread usage = %+v, want %+v", thread.Usage, wantUsage)
	}
	if len(thread.ModelEffort) != 3 {
		t.Fatalf("ModelEffort count = %d, want 3: %+v", len(thread.ModelEffort), thread.ModelEffort)
	}
	if thread.ModelEffort[0].Model != "gpt" || thread.ModelEffort[0].Effort != "high" || !reflect.DeepEqual(thread.ModelEffort[0].Usage, usageFromTotals(session.ModelEffort[0].Totals)) || thread.ModelEffort[0].CompactionTokens == nil || *thread.ModelEffort[0].CompactionTokens != 7 {
		t.Fatalf("known compaction group = %+v", thread.ModelEffort[0])
	}
	if thread.ModelEffort[1].Model != "o3" || thread.ModelEffort[1].Effort != "high" || !reflect.DeepEqual(thread.ModelEffort[1].Usage, usageFromTotals(session.ModelEffort[2].Totals)) || thread.ModelEffort[1].CompactionTokens != nil {
		t.Fatalf("unavailable compaction group = %+v", thread.ModelEffort[1])
	}
	if thread.ModelEffort[2].Model != "gpt" || thread.ModelEffort[2].Effort != "low" || !reflect.DeepEqual(thread.ModelEffort[2].Usage, usageFromTotals(session.ModelEffort[1].Totals)) || thread.ModelEffort[2].CompactionTokens == nil || *thread.ModelEffort[2].CompactionTokens != 0 {
		t.Fatalf("known zero compaction group = %+v", thread.ModelEffort[2])
	}
	for _, modelEffort := range thread.ModelEffort {
		if modelEffort.CompactionTokens == nil {
			t.Logf("Codex list model=%s effort=%s usage_total=%d compaction=nil", modelEffort.Model, modelEffort.Effort, modelEffort.Usage.Total)
		} else {
			t.Logf("Codex list model=%s effort=%s usage_total=%d compaction=%d", modelEffort.Model, modelEffort.Effort, modelEffort.Usage.Total, *modelEffort.CompactionTokens)
		}
	}
	t.Logf("Codex list agent=%s session=%s thread=%s type=%q self_total=%d", thread.Key.Agent, thread.Key.SessionID, thread.Key.ThreadID, thread.Type, thread.Usage.Total)
	detail, err := service.SessionDetail(context.Background(), ScopeQuery{}, thread.Key)
	if err != nil {
		t.Fatalf("SessionDetail() error = %v", err)
	}
	if len(detail.Children) != 0 || !reflect.DeepEqual(detail.InclusiveUsage, detail.SelfUsage) {
		t.Fatalf("Codex flat detail = %+v", detail)
	}
	t.Logf("Codex detail thread=%s type=%q self_total=%d inclusive_total=%d children=%v", detail.Thread.Key.ThreadID, detail.Thread.Type, detail.SelfUsage.Total, detail.InclusiveUsage.Total, detail.Children)
}

func TestSessionProjectionRangeOrphan(t *testing.T) {
	session := query.SessionUsage{Agent: "claude", ID: "A", Last: testTime(12, 0), HasRows: true, Matched: true}
	service := newService(&fakeQuery{dataset: query.SessionDatasetResult{
		Sessions: []query.SessionUsage{session},
		Rows: []query.UsageRow{
			testUsageRow("claude", "A", "P", "A", testTime(11, 0), "worker", "low", 2, 0, 0, 0, "", ""),
			testUsageRow("claude", "A", "C", "B", testTime(10, 0), "nested", "medium", 3, 0, 0, 0, "", ""),
		},
	}})
	list, err := service.Sessions(context.Background(), ScopeQuery{})
	if err != nil {
		t.Fatalf("Sessions() error = %v", err)
	}
	if len(list.Items) != 1 || list.Items[0].Thread.Key.ThreadID != "A" || list.Items[0].SubagentCount == nil || *list.Items[0].SubagentCount != 2 {
		t.Fatalf("root list item must include all real subagents: %+v", list.Items)
	}
	projected, _, _, err := service.loadThreadProjection(context.Background(), ScopeQuery{})
	if err != nil {
		t.Fatalf("loadThreadProjection() error = %v", err)
	}
	if len(projected.Threads) != 3 || projected.Threads[2].Key.ThreadID != "C" || projected.Threads[2].ParentThreadID != "B" {
		t.Fatalf("orphan C must retain its known parent B: %+v", projected.Threads)
	}
	parent, err := service.SessionDetail(context.Background(), ScopeQuery{}, ThreadKey{Agent: "claude", SessionID: "A", ThreadID: "P"})
	if err != nil {
		t.Fatalf("P SessionDetail() error = %v", err)
	}
	if len(parent.Children) != 0 || parent.SubagentUsage.Total != 0 || !reflect.DeepEqual(parent.InclusiveUsage, parent.SelfUsage) {
		t.Fatalf("orphan C was guessed as P descendant: %+v", parent)
	}
}

func TestThreadIdentityAndSort(t *testing.T) {
	at := testTime(10, 0)
	service := newService(&fakeQuery{dataset: query.SessionDatasetResult{
		Sessions: []query.SessionUsage{
			{Agent: "claude", ID: "A2", FirstUsage: at, LastUsage: at},
			{Agent: "codex", ID: "B", FirstUsage: at, LastUsage: at},
			{Agent: "claude", ID: "A1", FirstUsage: at, LastUsage: at},
		},
		Rows: []query.UsageRow{
			testUsageRow("claude", "A2", "B", "", at, "worker", "low", 1, 0, 0, 0, "", ""),
			testUsageRow("claude", "A1", "B", "", at, "worker", "low", 1, 0, 0, 0, "", ""),
		},
	}})
	list, err := service.Sessions(context.Background(), ScopeQuery{})
	if err != nil {
		t.Fatalf("Sessions() error = %v", err)
	}
	want := []ThreadKey{
		{Agent: "claude", SessionID: "A1", ThreadID: "A1"},
		{Agent: "claude", SessionID: "A2", ThreadID: "A2"},
		{Agent: "codex", SessionID: "B", ThreadID: "B"},
	}
	got := make([]ThreadKey, len(list.Items))
	unique := make(map[ThreadKey]struct{}, len(list.Items))
	for i, item := range list.Items {
		got[i] = item.Thread.Key
		unique[item.Thread.Key] = struct{}{}
	}
	if !reflect.DeepEqual(got, want) || len(unique) != len(want) {
		t.Fatalf("sorted unique thread identities = %+v, want %+v", got, want)
	}
}

func TestSessionProjectionErrors(t *testing.T) {
	key := ThreadKey{Agent: "claude", SessionID: "missing", ThreadID: "missing"}
	if _, err := newService(&fakeQuery{}).SessionDetail(context.Background(), ScopeQuery{}, key); !errors.Is(err, ErrThreadNotFound) {
		t.Fatalf("missing thread error = %v, want %v", err, ErrThreadNotFound)
	}

	wantErr := errors.New("session dataset query failed")
	service := newService(&fakeQuery{onSessionDataset: func(context.Context, query.SessionQuery) (query.SessionDatasetResult, error) {
		return query.SessionDatasetResult{}, wantErr
	}})
	if _, err := service.Sessions(context.Background(), ScopeQuery{}); !errors.Is(err, wantErr) {
		t.Fatalf("wrapped SessionDataset error = %v, want chain to %v", err, wantErr)
	}
}

func TestThreadCodexKindsStayFlat(t *testing.T) {
	service := newService(&fakeQuery{dataset: query.SessionDatasetResult{
		Sessions: []query.SessionUsage{{
			Agent: "codex", ID: "X", Totals: query.UsageTotals{Calls: 2, Input: 3},
			ModelEffort: []query.ModelEffortUsage{{Model: "gpt", Effort: "high", Totals: query.UsageTotals{Calls: 2, Input: 3}}},
		}},
		Rows: []query.UsageRow{
			testUsageRow("codex", "X", "", "", testTime(9, 0), "gpt", "high", 1, 0, 0, 0, "review", "gateway"),
			testUsageRow("codex", "X", "", "", testTime(10, 0), "gpt", "high", 2, 0, 0, 0, "collab_spawn", "gateway"),
		},
	}})

	list, err := service.Sessions(context.Background(), ScopeQuery{})
	if err != nil {
		t.Fatalf("Sessions() error = %v", err)
	}
	if len(list.Items) != 1 || list.Items[0].Thread.Key.ThreadID != "X" || list.Items[0].Thread.Type != ThreadMain || list.Items[0].Thread.Usage.Input != 1 || list.Items[0].SelfUsage.Input != 1 || list.Items[0].SubagentUsage.Input != 2 || list.Items[0].InclusiveUsage.Input != 3 || list.Items[0].SubagentCount != nil {
		t.Fatalf("Codex kind rows produced non-flat root item: %+v", list.Items)
	}
}

func TestThreadParentConflict(t *testing.T) {
	session := query.SessionUsage{Agent: "claude", ID: "A", HasRows: true, Matched: true}
	conflicting := newService(&fakeQuery{dataset: query.SessionDatasetResult{
		Sessions: []query.SessionUsage{session},
		Rows: []query.UsageRow{
			testUsageRow("claude", "A", "B", "P", testTime(9, 0), "m", "", 1, 0, 0, 0, "", ""),
			testUsageRow("claude", "A", "B", "Q", testTime(10, 0), "m", "", 1, 0, 0, 0, "", ""),
		},
	}})
	list, err := conflicting.Sessions(context.Background(), ScopeQuery{})
	if err != nil || len(list.Items) != 1 || list.Items[0].InclusiveUsage.Input != 2 {
		t.Fatalf("conflicting parent rows must remain in flat session totals: %+v, %v", list, err)
	}
	projection, _, _, err := conflicting.loadThreadProjection(context.Background(), ScopeQuery{})
	if err != nil {
		t.Fatalf("loadThreadProjection() error = %v", err)
	}
	for _, thread := range projection.Threads {
		if thread.Key.ThreadID == "B" && thread.ParentThreadID != "" {
			t.Fatalf("conflicting parent values selected %q, want empty", thread.ParentThreadID)
		}
	}

	missingThenKnown := newService(&fakeQuery{dataset: query.SessionDatasetResult{
		Sessions: []query.SessionUsage{session},
		Rows: []query.UsageRow{
			testUsageRow("claude", "A", "B", "", testTime(9, 0), "m", "", 1, 0, 0, 0, "", ""),
			testUsageRow("claude", "A", "B", "P", testTime(10, 0), "m", "", 1, 0, 0, 0, "", ""),
		},
	}})
	list, err = missingThenKnown.Sessions(context.Background(), ScopeQuery{})
	if err != nil {
		t.Fatalf("empty and known parent error = %v", err)
	}
	projected, _, _, err := missingThenKnown.loadThreadProjection(context.Background(), ScopeQuery{})
	if err != nil {
		t.Fatalf("loadThreadProjection() error = %v", err)
	}
	threads := make(map[string]SessionThreadView, len(projected.Threads))
	for _, thread := range projected.Threads {
		threads[thread.Key.ThreadID] = thread
	}
	if len(list.Items) != 1 || len(projected.Threads) != 2 || threads["B"].ParentThreadID != "P" || threads["A"].Type != ThreadMain {
		t.Fatalf("parent after missing value = %+v", projected.Threads)
	}
}

func testTime(hour, minute int) time.Time {
	return time.Date(2026, 10, 11, hour, minute, 0, 0, time.UTC)
}

func testUsageRow(agent, sessionID, subagent, parent string, at time.Time, model, effort string, input, output, cacheRead, cacheWrite int64, kind, origin string) query.UsageRow {
	return query.UsageRow{
		Time: at, Agent: agent, Model: model, Effort: effort,
		Session: sessionID, Subagent: subagent, ParentAgent: parent, Kind: kind, Origin: origin,
		ResolvedSession: query.SessionRef{Agent: agent, ID: sessionID, Matched: true},
		Input:           input, Output: output, CacheRead: cacheRead, CacheWrite: cacheWrite, Priced: true,
	}
}

func TestSessionFieldsCompactionOnUnknownModel(t *testing.T) {
	session := query.SessionUsage{
		Agent: "codex", ID: "A", LastUsage: testTime(12, 0),
		ModelEffort: []query.ModelEffortUsage{{Model: query.UnknownModel, Totals: query.UsageTotals{Calls: 2, Input: 5}}},
	}
	service := newService(&fakeQuery{dataset: query.SessionDatasetResult{
		Sessions: []query.SessionUsage{session},
		Rows: []query.UsageRow{
			testUsageRow("codex", "A", "", "", testTime(10, 0), "", "", 3, 0, 0, 0, "", "gateway"),
			testUsageRow("codex", "A", "", "", testTime(11, 0), "", "", 2, 0, 0, 0, "compact", "gateway"),
		},
	}})
	list, err := service.Sessions(context.Background(), ScopeQuery{})
	if err != nil || len(list.Items) != 1 {
		t.Fatalf("Sessions() = %+v, %v", list, err)
	}
	groups := list.Items[0].Thread.ModelEffort
	if len(groups) != 1 || groups[0].Model != query.UnknownModel || groups[0].CompactionTokens == nil || *groups[0].CompactionTokens != 2 {
		t.Fatalf("unknown model group = %+v, want compaction matched from model-less rows", groups)
	}
}
