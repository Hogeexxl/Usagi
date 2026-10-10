package codex

import (
	"context"
	"database/sql"
	"fmt"
	"path/filepath"
	"strings"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/codex/rollout"
	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

func TestMetadataStreamingAccumulatorUsesOwningSessionAndTurnContext(t *testing.T) {
	const threadID = "01900000-0000-7000-8000-000000000000"
	accumulator := NewMetadataAccumulator(11, 1, MetadataParserVersion, 40, threadID, rollout.OwningThreadCandidates{}, nil)
	meta := rollout.Record{SourceFileID: 11, Generation: 1, LogicalStartOffset: 0, LogicalEndOffset: 80,
		JSON: []byte(`{"timestamp":"2026-01-02T03:04:05Z","type":"session_meta","payload":{"id":"` + threadID + `","source":"cli","agent_role":"main"}}`)}
	if got := accumulator.ObserveRecord(meta); got.Ownership.Kind != rollout.OwnershipOwning {
		t.Fatalf("session_meta ownership: %#v", got)
	}
	turn := rollout.Record{SourceFileID: 11, Generation: 1, LogicalStartOffset: 80, LogicalEndOffset: 160,
		JSON: []byte(`{"timestamp":"2026-01-03T04:05:06Z","type":"turn_context","payload":{"turn_id":"01900000-0001-7000-8000-000000000000","model":"gpt-5","cwd":"/workspace/project"}}`)}
	if got := accumulator.ObserveRecord(turn); got.Ownership.Kind != rollout.OwnershipOwning {
		t.Fatalf("turn_context ownership: %#v", got)
	}
	fact := accumulator.Snapshot(160, 50)
	if fact.AgentRoleHint == nil || fact.AgentRoleHint.Value != "main" || fact.LatestContextModel == nil || *fact.LatestContextModel != "gpt-5" ||
		fact.CWD == nil || fact.CWD.Value != "/workspace/project" || fact.LatestContextAtMS == nil || *fact.LatestContextAtMS != 1767413106000 {
		t.Fatalf("streamed owning metadata missing: %#v", fact)
	}
}

func TestMetadataAccumulatorOrdersLatestContextByUUIDv7Time(t *testing.T) {
	const threadID = "01900000-0000-7000-8000-000000000000"
	accumulator := NewMetadataAccumulator(11, 1, MetadataParserVersion, 40, threadID, rollout.OwningThreadCandidates{}, nil)
	meta := rollout.Record{SourceFileID: 11, Generation: 1, LogicalStartOffset: 0, LogicalEndOffset: 40,
		JSON: []byte(`{"type":"session_meta","payload":{"id":"` + threadID + `"}}`)}
	if got := accumulator.ObserveRecord(meta); got.Ownership.Kind != rollout.OwnershipOwning {
		t.Fatalf("session_meta ownership: %#v", got)
	}
	for _, record := range []rollout.Record{
		{SourceFileID: 11, Generation: 1, LogicalStartOffset: 40, LogicalEndOffset: 80,
			JSON: []byte(`{"timestamp":"2026-01-03T04:05:06Z","type":"turn_context","payload":{"turn_id":"01900000-0002-7000-8000-000000000000","model":"uuidv7-later"}}`)},
		{SourceFileID: 11, Generation: 1, LogicalStartOffset: 80, LogicalEndOffset: 120,
			JSON: []byte(`{"timestamp":"2027-01-03T04:05:06Z","type":"turn_context","payload":{"turn_id":"01900000-0001-7000-8000-000000000000","model":"envelope-later"}}`)},
	} {
		if got := accumulator.ObserveRecord(record); got.Ownership.Kind != rollout.OwnershipOwning {
			t.Fatalf("turn_context ownership: %#v", got)
		}
	}
	fact := accumulator.Snapshot(120, 50)
	if fact.LatestContextModel == nil || *fact.LatestContextModel != "uuidv7-later" {
		t.Fatalf("envelope timestamp incorrectly outranked UUIDv7 timestamp: %#v", fact)
	}
}

func TestMetadataResolverKeepsFieldsWithReaderDiagnostics(t *testing.T) {
	statePath := filepath.Join(t.TempDir(), "state.sqlite")
	db, err := sql.Open("sqlite", statePath)
	if err != nil {
		t.Fatal(err)
	}
	_, err = db.Exec(`CREATE TABLE threads(id TEXT,name TEXT,title TEXT,cwd TEXT,model TEXT,created_at_ms INTEGER,updated_at_ms INTEGER,agent_role TEXT,archived INTEGER);
		CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT);
		INSERT INTO threads VALUES
		('thread-invalid',char(1),char(1),'relative',char(1),-1,-1,'main',0),
		('thread-session-time',NULL,NULL,NULL,NULL,NULL,NULL,'main',0)`)
	if err != nil {
		db.Close()
		t.Fatal(err)
	}
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
	state := ReadStateSnapshot(statePath)
	if state.Status != StateSourceComplete || state.SpawnEdgesStatus != StateSourceComplete || !state.ThreadColumns["name"] || !state.ThreadColumns["updated_at_ms"] {
		t.Fatalf("state reader did not retain trusted field availability: %#v", state)
	}
	sessionInput := strings.Join([]string{
		`{"id":"thread-invalid","thread_name":"\u0001","updated_at":"2026-01-01T00:00:00Z"}`,
		`{"id":"thread-session-time","thread_name":"session title","updated_at":"not-a-time"}`,
	}, "\n") + "\n"
	session := readSessionSnapshot(strings.NewReader(sessionInput))
	if session.Status != SessionSourceComplete || !sessionHasDiagnostic(session, "thread-invalid", "thread_name") || !sessionHasDiagnostic(session, "thread-session-time", "updated_at") {
		t.Fatalf("session reader diagnostics missing: %#v", session)
	}
	oldInvalid := existingMetadataThread("thread-invalid")
	oldSessionTime := existingMetadataThread("thread-session-time")
	result := ResolveThreadView(MetadataResolveInput{
		State: state, Session: session, Global: GlobalStateSnapshot{Status: GlobalStateUnreadable},
		RolloutFacts: []MetadataEvidence{completeMetadataEvidence("thread-invalid", 11), completeMetadataEvidence("thread-session-time", 12)},
		Sources:      []rollout.SourceObservation{metadataObservation(11, "thread-invalid", rollout.AreaSessions), metadataObservation(12, "thread-session-time", rollout.AreaSessions)},
		Existing:     []domain.Thread{oldInvalid, oldSessionTime}, ResolvedAtMS: 60,
	})
	invalidPatch := metadataPatchByID(t, result, "thread-invalid")
	for name, kind := range map[string]domain.PatchKind{
		"title": invalidPatch.Title.Kind(), "project_name": invalidPatch.ProjectName.Kind(), "project_path": invalidPatch.ProjectPath.Kind(),
		"model": invalidPatch.MetadataModel.Kind(), "created": invalidPatch.CreatedAtMS.Kind(), "updated": invalidPatch.UpdatedAtMS.Kind(),
	} {
		if kind != domain.PatchKeep {
			t.Errorf("invalid state/session %s must Keep existing value, got patch kind %v", name, kind)
		}
	}
	if invalidPatch.FullResolution {
		t.Fatal("field-invalid evidence must not cause a Clear")
	}
	timePatch := metadataPatchByID(t, result, "thread-session-time")
	if got, ok := timePatch.Title.Value(); !ok || got != "session title" {
		t.Fatalf("valid session title was lost: %#v", timePatch.Title)
	}
	if timePatch.UpdatedAtMS.Kind() != domain.PatchKeep {
		t.Fatalf("invalid session updated_at must Keep old value, got %v", timePatch.UpdatedAtMS.Kind())
	}
}

func TestMetadataClearNeedsTrustworthyPerFieldEvidence(t *testing.T) {
	id := "thread-clear"
	state := completeStateSnapshot(StateThreadFact{ThreadID: id, AgentRoleHint: stringPointer("main"), Archived: boolPointer(false)})
	old := existingMetadataThread(id)
	fact := completeMetadataEvidence(id, 11)
	result := ResolveThreadView(MetadataResolveInput{
		State: state, Session: completeSessionSnapshot(), Global: GlobalStateSnapshot{Status: GlobalStateUnreadable},
		RolloutFacts: []MetadataEvidence{fact}, Sources: []rollout.SourceObservation{metadataObservation(11, id, rollout.AreaSessions)},
		Existing: []domain.Thread{old}, ResolvedAtMS: 70,
	})
	patch := metadataPatchByID(t, result, id)
	for name, kind := range map[string]domain.PatchKind{
		"title": patch.Title.Kind(), "project_name": patch.ProjectName.Kind(), "project_path": patch.ProjectPath.Kind(),
		"model": patch.MetadataModel.Kind(), "created": patch.CreatedAtMS.Kind(), "updated": patch.UpdatedAtMS.Kind(),
	} {
		if kind != domain.PatchClear {
			t.Errorf("complete trusted empty %s must Clear, got %v", name, kind)
		}
	}
	if !patch.FullResolution {
		t.Fatal("Clear patch must set FullResolution")
	}
	if err := patch.Validate(); err != nil {
		t.Fatalf("valid field-specific Clear patch rejected: %v", err)
	}
	if got, ok := patch.RootSessionID.Value(); !ok || got != id {
		t.Fatalf("Set root even when unchanged must affirm it as the current resolved root: %q (%v)", got, ok)
	}
	withoutFullResolution := patch
	withoutFullResolution.FullResolution = false
	if err := withoutFullResolution.Validate(); err == nil {
		t.Fatal("Clear with FullResolution=false was accepted")
	}
	identity, err := domain.NewSessionIdentity(id, domain.SourceCodex, id)
	if err != nil {
		t.Fatal(err)
	}
	for name, clear := range map[string]func(*domain.ResolvedThreadPatch){
		"agent_role":   func(p *domain.ResolvedThreadPatch) { p.AgentRole = domain.Clear[domain.AgentRole]() },
		"project_kind": func(p *domain.ResolvedThreadPatch) { p.ProjectKind = domain.Clear[domain.ProjectKind]() },
		"archived":     func(p *domain.ResolvedThreadPatch) { p.Archived = domain.Clear[bool]() },
	} {
		invalid, err := domain.NewResolvedThreadPatch(identity, 70)
		if err != nil {
			t.Fatal(err)
		}
		invalid.FullResolution = true
		clear(&invalid)
		if err := invalid.Validate(); err == nil {
			t.Errorf("non-nullable %s Clear was accepted", name)
		}
	}
}

func TestMetadataPartialAndConflictedOwnershipCannotClearExistingFields(t *testing.T) {
	for _, quality := range []string{"partial", "conflict"} {
		t.Run(quality, func(t *testing.T) {
			id := "thread-" + quality
			fact := completeMetadataEvidence(id, 11)
			fact.QualityStatus = quality
			fact.HasConflict = quality == "conflict"
			state := completeStateSnapshot(StateThreadFact{ThreadID: id, AgentRoleHint: stringPointer("main")})
			result := ResolveThreadView(MetadataResolveInput{
				State: state, Session: completeSessionSnapshot(), Global: GlobalStateSnapshot{Status: GlobalStateUnreadable},
				RolloutFacts: []MetadataEvidence{fact}, Sources: []rollout.SourceObservation{metadataObservation(11, id, rollout.AreaSessions)},
				Existing: []domain.Thread{existingMetadataThread(id)}, ResolvedAtMS: 80,
			})
			patch := metadataPatchByID(t, result, id)
			for name, kind := range map[string]domain.PatchKind{
				"title": patch.Title.Kind(), "project_name": patch.ProjectName.Kind(), "project_path": patch.ProjectPath.Kind(),
				"model": patch.MetadataModel.Kind(), "created": patch.CreatedAtMS.Kind(), "updated": patch.UpdatedAtMS.Kind(),
			} {
				if kind != domain.PatchKeep {
					t.Errorf("%s ownership fact must Keep %s, got %v", quality, name, kind)
				}
			}
			if patch.FullResolution || patch.MetadataQualityStatus != domain.MetadataQualityStatus(quality) {
				t.Fatalf("wrong uncertainty result: %#v", patch)
			}
		})
	}
}

func TestMetadataResolverUncertaintyKeepsExistingRelationshipAndRoot(t *testing.T) {
	id := "thread-uncertain"
	fact := completeMetadataEvidence(id, 11)
	fact.OwnershipConfidence = "unresolved"
	fact.QualityStatus = "partial"
	result := ResolveThreadView(MetadataResolveInput{
		State:        StateSnapshot{Status: StateSourceUnavailable, SpawnEdgesStatus: StateSourceUnavailable},
		Session:      SessionNameSnapshot{Status: SessionSourceUnavailable},
		Global:       GlobalStateSnapshot{Status: GlobalStateMalformed},
		RolloutFacts: []MetadataEvidence{fact}, Sources: []rollout.SourceObservation{metadataObservation(11, id, rollout.AreaSessions)},
		Existing: []domain.Thread{existingMetadataSubagent(id, "parent-old", "root-old")}, ResolvedAtMS: 90,
	})
	patch := metadataPatchByID(t, result, id)
	if patch.ParentThreadID.Kind() != domain.PatchKeep || patch.RootSessionID.Kind() != domain.PatchKeep || patch.AgentRole.Kind() != domain.PatchKeep ||
		patch.Title.Kind() != domain.PatchKeep || patch.ProjectPath.Kind() != domain.PatchKeep || patch.MetadataModel.Kind() != domain.PatchKeep ||
		patch.CreatedAtMS.Kind() != domain.PatchKeep || patch.UpdatedAtMS.Kind() != domain.PatchKeep || patch.FullResolution {
		t.Fatalf("uncertain evidence changed existing relationship or fields: %#v", patch)
	}
}

func TestMetadataParentPrecedenceAndSameTierConflictKeepOldRelationship(t *testing.T) {
	t.Run("state edge wins lower conflict", func(t *testing.T) {
		child, stateParent, directParent := "child", "parent-state", "parent-direct"
		fact := completeMetadataEvidence(child, 11)
		fact.ParentThreadIDHint = &rollout.MetadataStringCandidate{Value: directParent, Provenance: "session_meta_parent", Offset: 10}
		fact.RelationshipConflict = true
		fact.QualityStatus = "conflict"
		state := completeStateSnapshotWithEdges([]StateThreadFact{
			{ThreadID: child}, {ThreadID: stateParent, AgentRoleHint: stringPointer("main")}, {ThreadID: directParent, AgentRoleHint: stringPointer("main")},
		}, []SpawnEdgeFact{{ParentThreadID: stateParent, ChildThreadID: child, Source: SpawnEdgeFromState}})
		old := existingMetadataSubagent(child, stateParent, stateParent)
		result := ResolveThreadView(MetadataResolveInput{State: state, Session: completeSessionSnapshot(), Global: GlobalStateSnapshot{Status: GlobalStateUnreadable},
			RolloutFacts: []MetadataEvidence{fact}, Sources: []rollout.SourceObservation{metadataObservation(11, child, rollout.AreaSessions)},
			Existing: []domain.Thread{old}, ResolvedAtMS: 100})
		patch := metadataPatchByID(t, result, child)
		assertRelationshipKept(t, patch)
		if patch.MetadataQualityStatus != "conflict" {
			t.Fatalf("lower tier conflict must be visible in quality: %s", patch.MetadataQualityStatus)
		}
	})

	t.Run("same tier is unresolved", func(t *testing.T) {
		child, parentA, parentB := "child", "parent-a", "parent-b"
		factA := completeMetadataEvidence(child, 11)
		factA.ParentThreadIDHint = &rollout.MetadataStringCandidate{Value: parentA, Provenance: "session_meta_parent", Offset: 10}
		factB := completeMetadataEvidence(child, 12)
		factB.ParentThreadIDHint = &rollout.MetadataStringCandidate{Value: parentB, Provenance: "session_meta_parent", Offset: 20}
		state := completeStateSnapshot(StateThreadFact{ThreadID: child}, StateThreadFact{ThreadID: parentA, AgentRoleHint: stringPointer("main")}, StateThreadFact{ThreadID: parentB, AgentRoleHint: stringPointer("main")})
		old := existingMetadataSubagent(child, parentA, parentA)
		result := ResolveThreadView(MetadataResolveInput{State: state, Session: completeSessionSnapshot(), Global: GlobalStateSnapshot{Status: GlobalStateUnreadable},
			RolloutFacts: []MetadataEvidence{factA, factB}, Sources: []rollout.SourceObservation{
				metadataObservation(11, child, rollout.AreaSessions), metadataObservation(12, child, rollout.AreaSessions),
			}, Existing: []domain.Thread{old}, ResolvedAtMS: 100})
		patch := metadataPatchByID(t, result, child)
		assertRelationshipKept(t, patch)
		if patch.MetadataQualityStatus != "conflict" {
			t.Fatalf("same-tier conflict quality: %s", patch.MetadataQualityStatus)
		}
	})
}

func TestMetadataMissingParentKeepsExistingRelationshipAsUnknown(t *testing.T) {
	const child = "child"
	fact := completeMetadataEvidence(child, 11)
	state := completeStateSnapshotWithEdges([]StateThreadFact{{ThreadID: child}}, []SpawnEdgeFact{{ParentThreadID: "missing-parent", ChildThreadID: child, Source: SpawnEdgeFromState}})
	result := ResolveThreadView(MetadataResolveInput{State: state, Session: completeSessionSnapshot(), Global: GlobalStateSnapshot{Status: GlobalStateUnreadable},
		RolloutFacts: []MetadataEvidence{fact}, Sources: []rollout.SourceObservation{metadataObservation(11, child, rollout.AreaSessions)},
		Existing: []domain.Thread{existingMetadataSubagent(child, "old-parent", "old-root")}, ResolvedAtMS: 110})
	patch := metadataPatchByID(t, result, child)
	assertRelationshipKept(t, patch)
	if patch.MetadataQualityStatus != "partial" {
		t.Fatalf("missing parent must mark unresolved current relationship partial: %#v", patch)
	}
	if !hasMetadataDiagnostic(result, "root_unresolved", child, "root_session_id") || !containsString(result.AffectedThreadIDs, child) {
		t.Fatalf("unresolved current root must be exposed to Phase4: diagnostics=%#v affected=%v", result.Diagnostics, result.AffectedThreadIDs)
	}
}

func TestMetadataCommitUnresolvedRelationshipPreservesHistoricalTuple(t *testing.T) {
	for _, scenario := range []string{"missing parent", "cycle", "missing ancestor"} {
		for _, existingRole := range []string{"main", "subagent", "new"} {
			t.Run(scenario+"/"+existingRole, func(t *testing.T) {
				env := newMetadataTestEnvironment(t)
				var threadSchema string
				if err := env.source.PrivateRead(func(reader storage.PrivateReader) error {
					return reader.QueryRow("SELECT sql FROM sqlite_master WHERE type='table' AND name='threads'").Scan(&threadSchema)
				}); err != nil || !strings.Contains(threadSchema, "agent_role = 'unknown' AND root_session_id IS NULL") {
					t.Fatalf("real SQLite must retain the v14 unknown-role CHECK: err=%v schema=%s", err, threadSchema)
				}
				const child = "child"
				switch existingRole {
				case "main":
					seedMetadataThread(t, env.source, metadataMainPatch(t, child, 1))
				case "subagent":
					seedMetadataThread(t, env.source, metadataMainPatch(t, "historical-root", 1))
					seedMetadataThread(t, env.source, metadataSubagentPatch(t, child, "historical-root", "historical-root", 1))
				}
				old, hasOld, err := env.db.GetThreadByID(context.Background(), child)
				if err != nil {
					t.Fatal(err)
				}
				stateFacts := []StateThreadFact{{ThreadID: child}}
				edges := []SpawnEdgeFact{{ParentThreadID: "missing-parent", ChildThreadID: child, Source: SpawnEdgeFromState}}
				facts := []MetadataEvidence{completeMetadataEvidence(child, 11)}
				sources := []rollout.SourceObservation{metadataObservation(11, child, rollout.AreaSessions)}
				if scenario != "missing parent" {
					stateFacts = append(stateFacts, StateThreadFact{ThreadID: "parent"})
					facts = append(facts, completeMetadataEvidence("parent", 12))
					sources = append(sources, metadataObservation(12, "parent", rollout.AreaSessions))
					ancestor := "missing-parent"
					if scenario == "cycle" {
						ancestor = child
					}
					edges = []SpawnEdgeFact{
						{ParentThreadID: "parent", ChildThreadID: child, Source: SpawnEdgeFromState},
						{ParentThreadID: ancestor, ChildThreadID: "parent", Source: SpawnEdgeFromState},
					}
				}
				input := MetadataResolveInput{
					State: completeStateSnapshotWithEdges(stateFacts, edges), Session: completeSessionSnapshot(),
					Global: GlobalStateSnapshot{Status: GlobalStateUnreadable}, RolloutFacts: facts, Sources: sources, ResolvedAtMS: 20,
				}
				if hasOld {
					input.Existing = []domain.Thread{old}
				}
				result := ResolveThreadView(input)
				patch := metadataPatchByID(t, result, child)
				if !hasMetadataDiagnostic(result, "root_unresolved", child, "root_session_id") || !containsString(result.AffectedThreadIDs, child) {
					t.Fatalf("unresolved current root must remain exposed: %#v", result)
				}
				outcome, err := CommitMetadata(env.source, MetadataCommitBatch{Threads: []MetadataThreadCommit{{Patch: &patch}}},
					MetadataCommitDeps{ProjectActiveCompaction: func(*source.WriteTx) ([]byte, error) { return nil, nil }}, 20)
				if err != nil || outcome.CommittedThreads != 1 {
					t.Fatalf("unresolved metadata commit: outcome=%#v err=%v", outcome, err)
				}
				if hasOld {
					assertRelationshipKept(t, patch)
				}
				stored, found, err := env.db.GetThreadByID(context.Background(), child)
				if err != nil || !found {
					t.Fatalf("committed thread missing: found=%v err=%v", found, err)
				}
				if hasOld {
					if stored.AgentRole != old.AgentRole || !sameOptionalString(stored.ParentThreadID, old.ParentThreadID) || !sameOptionalString(stored.RootSessionID, old.RootSessionID) {
						t.Fatalf("historical relationship tuple changed: old=%#v stored=%#v", old, stored)
					}
				} else if stored.AgentRole != "unknown" || stored.RootSessionID != nil {
					t.Fatalf("new unresolved thread must remain unknown with NULL root: %#v", stored)
				}
				if stored.MetadataQualityStatus != patch.MetadataQualityStatus || stored.MetadataResolvedAtMS != 20 {
					t.Fatalf("metadata update was not persisted: %#v", stored)
				}
			})
		}
	}
}

func TestMetadataUnresolvedRootDiagnosticSurvivesNoOpPatch(t *testing.T) {
	const child = "thread-root-unresolved-noop"
	old := existingMetadataSubagent(child, "historical-parent", "historical-root")
	old.MetadataQualityStatus = "partial"
	result := ResolveThreadView(MetadataResolveInput{
		State:   StateSnapshot{Status: StateSourceUnavailable, SpawnEdgesStatus: StateSourceUnavailable},
		Session: SessionNameSnapshot{Status: SessionSourceUnavailable}, Global: GlobalStateSnapshot{Status: GlobalStateMalformed},
		Sources: []rollout.SourceObservation{metadataObservation(11, child, rollout.AreaSessions)}, Existing: []domain.Thread{old}, ResolvedAtMS: 111,
	})
	if len(result.Patches) != 0 {
		t.Fatalf("unchanged partial metadata should remain a no-op: %#v", result.Patches)
	}
	if !hasMetadataDiagnostic(result, "root_unresolved", child, "root_session_id") || !containsString(result.AffectedThreadIDs, child) {
		t.Fatalf("no-op unresolved root was hidden: diagnostics=%#v affected=%v", result.Diagnostics, result.AffectedThreadIDs)
	}
}

func TestMetadataCompleteMainEvidenceClearsExistingParent(t *testing.T) {
	const child = "thread-proven-main"
	state := completeStateSnapshot(StateThreadFact{ThreadID: child, AgentRoleHint: stringPointer("main")})
	fact := completeMetadataEvidence(child, 11)
	result := ResolveThreadView(MetadataResolveInput{
		State: state, Session: completeSessionSnapshot(), Global: GlobalStateSnapshot{Status: GlobalStateUnreadable},
		RolloutFacts: []MetadataEvidence{fact}, Sources: []rollout.SourceObservation{metadataObservation(11, child, rollout.AreaSessions)},
		Existing: []domain.Thread{existingMetadataSubagent(child, "old-parent", "old-root")}, ResolvedAtMS: 112,
	})
	patch := metadataPatchByID(t, result, child)
	if patch.ParentThreadID.Kind() != domain.PatchClear || !patch.FullResolution {
		t.Fatalf("complete proven no-parent main relation must Clear old parent: %#v", patch)
	}
	if got, ok := patch.RootSessionID.Value(); !ok || got != child {
		t.Fatalf("complete main relation must affirm current root %q: got %q (%v)", child, got, ok)
	}
}

func TestMetadataCycleAndDepth257DoNotReplaceHistoricalRoot(t *testing.T) {
	t.Run("cycle", func(t *testing.T) {
		factA := completeMetadataEvidence("a", 11)
		factA.ParentThreadIDHint = &rollout.MetadataStringCandidate{Value: "b", Provenance: "session_meta_parent", Offset: 10}
		factB := completeMetadataEvidence("b", 12)
		factB.ParentThreadIDHint = &rollout.MetadataStringCandidate{Value: "a", Provenance: "session_meta_parent", Offset: 20}
		state := completeStateSnapshot(StateThreadFact{ThreadID: "a"}, StateThreadFact{ThreadID: "b"})
		old := existingMetadataSubagent("a", "b", "historical-root")
		result := ResolveThreadView(MetadataResolveInput{State: state, Session: completeSessionSnapshot(), Global: GlobalStateSnapshot{Status: GlobalStateUnreadable},
			RolloutFacts: []MetadataEvidence{factA, factB}, Sources: []rollout.SourceObservation{
				metadataObservation(11, "a", rollout.AreaSessions), metadataObservation(12, "b", rollout.AreaSessions),
			}, Existing: []domain.Thread{old}, ResolvedAtMS: 120})
		patch := metadataPatchByID(t, result, "a")
		assertRelationshipKept(t, patch)
		if patch.MetadataQualityStatus != "conflict" {
			t.Fatalf("cycle quality: %s", patch.MetadataQualityStatus)
		}
	})

	t.Run("depth 257", func(t *testing.T) {
		const edgeCount = 257
		facts := make([]MetadataEvidence, 0, edgeCount)
		sources := make([]rollout.SourceObservation, 0, edgeCount)
		stateFacts := make([]StateThreadFact, 0, edgeCount+1)
		for i := 0; i <= edgeCount; i++ {
			id := fmt.Sprintf("node-%03d", i)
			stateFact := StateThreadFact{ThreadID: id}
			if i == edgeCount {
				stateFact.AgentRoleHint = stringPointer("main")
			}
			stateFacts = append(stateFacts, stateFact)
			if i == edgeCount {
				continue
			}
			fact := completeMetadataEvidence(id, int64(i+1))
			fact.ParentThreadIDHint = &rollout.MetadataStringCandidate{Value: fmt.Sprintf("node-%03d", i+1), Provenance: "session_meta_parent", Offset: int64(i)}
			facts = append(facts, fact)
			sources = append(sources, metadataObservation(int64(i+1), id, rollout.AreaSessions))
		}
		old := existingMetadataSubagent("node-000", "node-001", "node-257")
		result := ResolveThreadView(MetadataResolveInput{State: completeStateSnapshot(stateFacts...), Session: completeSessionSnapshot(), Global: GlobalStateSnapshot{Status: GlobalStateUnreadable},
			RolloutFacts: facts, Sources: sources, Existing: []domain.Thread{old}, ResolvedAtMS: 120})
		patch := metadataPatchByID(t, result, "node-000")
		assertRelationshipKept(t, patch)
		if patch.MetadataQualityStatus != "conflict" {
			t.Fatalf("depth 257 quality: %s", patch.MetadataQualityStatus)
		}
	})
}

func TestMetadataTitlePrecedence(t *testing.T) {
	cases := []struct {
		name      string
		state     StateThreadFact
		session   *SessionNameFact
		fact      MetadataEvidence
		wantTitle string
	}{
		{name: "state name", state: StateThreadFact{ThreadID: "title-thread", Name: stringPointer("name wins"), Title: stringPointer("title")}, session: sessionName("session"), wantTitle: "name wins"},
		{name: "state title", state: StateThreadFact{ThreadID: "title-thread", Title: stringPointer("title wins")}, session: sessionName("session"), wantTitle: "title wins"},
		{name: "session name", state: StateThreadFact{ThreadID: "title-thread"}, session: sessionName("session wins"), wantTitle: "session wins"},
		{name: "subagent path fallback", state: StateThreadFact{ThreadID: "title-thread"}, fact: metadataPathFact("title-thread", 11, "subagent"), wantTitle: "Sub Agent"},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			id := "title-thread"
			state := completeStateSnapshot(test.state, StateThreadFact{ThreadID: "parent", AgentRoleHint: stringPointer("main")})
			facts := []MetadataEvidence{completeMetadataEvidence(id, 11)}
			facts[0].AgentRoleHint = &rollout.MetadataStringCandidate{Value: "subagent", Provenance: "session_meta_role", Offset: 1}
			if test.fact.SourceFileID != 0 {
				facts[0] = test.fact
				facts[0].ParentThreadIDHint = &rollout.MetadataStringCandidate{Value: "parent", Provenance: "session_meta_parent", Offset: 1}
			}
			session := completeSessionSnapshot()
			if test.session != nil {
				session = sessionSnapshotWith(*test.session)
			}
			result := ResolveThreadView(MetadataResolveInput{State: state, Session: session, Global: GlobalStateSnapshot{Status: GlobalStateUnreadable},
				RolloutFacts: facts, Sources: []rollout.SourceObservation{metadataObservation(11, id, rollout.AreaSessions)}, ResolvedAtMS: 130})
			patch := metadataPatchByID(t, result, id)
			if got, ok := patch.Title.Value(); !ok || got != test.wantTitle {
				t.Fatalf("title: got %q (%v), want %q", got, ok, test.wantTitle)
			}
		})
	}
}

func TestMetadataProjectModelTimeAndArchivePrecedence(t *testing.T) {
	id := "thread-priority"
	fact := completeMetadataEvidence(id, 11)
	fact.CWD = &rollout.MetadataStringCandidate{Value: "/rollout/project", Provenance: "turn_context", Offset: 10}
	fact.CreatedAtMS = int64Pointer(20)
	fact.LatestContextModel = stringPointer("rollout-model")
	fact.LatestContextAtMS = int64Pointer(120)
	session := sessionSnapshotWith(SessionNameFact{ThreadID: id, ThreadName: "session", UpdatedAtMS: int64Pointer(150)})
	stateFact := StateThreadFact{ThreadID: id, Name: stringPointer("state name"), CWD: stringPointer("/state/project"), MetadataModel: stringPointer("state-model"),
		CreatedAtMS: int64Pointer(80), UpdatedAtMS: int64Pointer(100), Archived: boolPointer(false), AgentRoleHint: stringPointer("main")}
	state := completeStateSnapshot(stateFact)
	global := GlobalStateSnapshot{Status: GlobalStateComplete, ThreadProjectAssignments: map[string]struct{}{id: {}}}
	result := ResolveThreadView(MetadataResolveInput{State: state, Session: session, Global: global,
		RolloutFacts: []MetadataEvidence{fact}, Sources: []rollout.SourceObservation{metadataObservation(11, id, rollout.AreaArchived)}, ResolvedAtMS: 140})
	patch := metadataPatchByID(t, result, id)
	if got, ok := patch.ProjectPath.Value(); !ok || got != "/rollout/project" {
		t.Fatalf("rollout cwd precedence: %q (%v)", got, ok)
	}
	if got, ok := patch.MetadataModel.Value(); !ok || got != "state-model" {
		t.Fatalf("state model precedence: %q (%v)", got, ok)
	}
	if got, ok := patch.CreatedAtMS.Value(); !ok || got != 80 {
		t.Fatalf("state created precedence: %d (%v)", got, ok)
	}
	if got, ok := patch.UpdatedAtMS.Value(); !ok || got != 150 {
		t.Fatalf("updated must be maximum of all three sources: %d (%v)", got, ok)
	}
	if got, ok := patch.Archived.Value(); !ok || got {
		t.Fatalf("state archived=false must beat archived area: %v (%v)", got, ok)
	}
	if got, ok := patch.ProjectKind.Value(); !ok || got != "project" {
		t.Fatalf("project assignment must imply project without path: %q (%v)", got, ok)
	}
}

func TestMetadataProjectAssignmentWithoutPathIsProject(t *testing.T) {
	id := "thread-assigned-project"
	fact := completeMetadataEvidence(id, 11)
	state := completeStateSnapshot(StateThreadFact{ThreadID: id, AgentRoleHint: stringPointer("main")})
	global := GlobalStateSnapshot{Status: GlobalStateComplete, ThreadProjectAssignments: map[string]struct{}{id: {}}}
	result := ResolveThreadView(MetadataResolveInput{State: state, Session: completeSessionSnapshot(), Global: global,
		RolloutFacts: []MetadataEvidence{fact}, Sources: []rollout.SourceObservation{metadataObservation(11, id, rollout.AreaSessions)}, ResolvedAtMS: 145})
	patch := metadataPatchByID(t, result, id)
	if got, ok := patch.ProjectKind.Value(); !ok || got != "project" {
		t.Fatalf("complete assignment with no path: %q (%v)", got, ok)
	}
}

func TestMetadataAffectedThreadIDsIncludeBindingEndpointsInStableOrder(t *testing.T) {
	oldOwner, newOwner := "owner-old", "owner-new"
	fact := completeMetadataEvidence(newOwner, 11)
	observation := metadataObservation(11, oldOwner, rollout.AreaSessions)
	state := completeStateSnapshot(StateThreadFact{ThreadID: oldOwner, AgentRoleHint: stringPointer("main")}, StateThreadFact{ThreadID: newOwner, AgentRoleHint: stringPointer("main")})
	result := ResolveThreadView(MetadataResolveInput{State: state, Session: completeSessionSnapshot(), Global: GlobalStateSnapshot{Status: GlobalStateUnreadable},
		RolloutFacts: []MetadataEvidence{fact, fact}, Sources: []rollout.SourceObservation{observation, observation}, ResolvedAtMS: 150})
	if got, want := result.AffectedThreadIDs, []string{newOwner, oldOwner}; !equalStrings(got, want) {
		t.Fatalf("affected thread IDs: got %v, want sorted unique %v", got, want)
	}
}

func TestMetadataCommitRootRebindUsesOneOuterTransactionAndRevision(t *testing.T) {
	env := newMetadataTestEnvironment(t)
	ctx := context.Background()
	seedMetadataThread(t, env.source, metadataMainPatch(t, "main-a", 1))
	seedMetadataThread(t, env.source, metadataMainPatch(t, "main-b", 1))
	seedMetadataThread(t, env.source, metadataSubagentPatch(t, "child", "main-a", "main-a", 1))
	beforeRevision := env.db.CurrentRevision().DataRevision

	patch := metadataSubagentPatch(t, "child", "main-b", "main-b", 2)
	var projectionRoots []string
	probe := func(tx *source.WriteTx) ([]byte, error) {
		root, err := metadataRootInTx(tx, "child")
		if err != nil {
			return nil, err
		}
		projectionRoots = append(projectionRoots, root)
		return []byte(root), nil
	}
	reconciled := false
	reconciler := func(tx *source.WriteTx, threadID string, previousRoot, nextRoot *string, changed []int64, committedAtMS int64) (bool, bool, []int64, []string, error) {
		reconciled = true
		if threadID != "child" || !sameOptionalString(previousRoot, stringPointer("main-a")) || !sameOptionalString(nextRoot, stringPointer("main-b")) || len(changed) != 0 {
			return false, false, nil, nil, fmt.Errorf("unexpected fake reconciliation input")
		}
		root, err := metadataRootInTx(tx, threadID)
		if err != nil {
			return false, false, nil, nil, err
		}
		if root != "main-b" {
			return false, false, nil, nil, fmt.Errorf("callback did not observe root mutation: %s", root)
		}
		return false, false, nil, nil, nil
	}
	identity, err := domain.NewSessionIdentity("child", domain.SourceCodex, "child")
	if err != nil {
		t.Fatal(err)
	}
	patch.RootSessionID = domain.Set("main-b")
	patch.ParentThreadID = domain.Set("main-b")
	patch.ThreadID, patch.Source, patch.NativeSessionID = identity.ThreadID, identity.Source, identity.NativeSessionID
	outcome, err := CommitMetadata(env.source, MetadataCommitBatch{Threads: []MetadataThreadCommit{{Patch: &patch}}},
		MetadataCommitDeps{ReconcileUsageBinding: reconciler, ProjectActiveCompaction: probe}, 2)
	if err != nil {
		t.Fatal(err)
	}
	if !reconciled || len(projectionRoots) != 2 || projectionRoots[0] != "main-a" || projectionRoots[1] != "main-b" {
		t.Fatalf("root projection/callback order: roots=%v reconciled=%v", projectionRoots, reconciled)
	}
	if !outcome.VisibleChanged || env.db.CurrentRevision().DataRevision != beforeRevision+1 {
		t.Fatalf("root rebind must cause one visible revision: outcome=%#v before=%d after=%d", outcome, beforeRevision, env.db.CurrentRevision().DataRevision)
	}
	thread, ok, err := env.db.GetThreadByID(ctx, "child")
	if err != nil || !ok || thread.RootSessionID == nil || *thread.RootSessionID != "main-b" {
		t.Fatalf("committed root: thread=%#v ok=%v err=%v", thread, ok, err)
	}
}

func TestMetadataCommitLoadsDurableFactAfterRestartAndKeepsLogicalOffsetSeparate(t *testing.T) {
	env := newMetadataTestEnvironment(t)
	id := "thread-restart"
	seedMetadataThread(t, env.source, metadataMainPatch(t, id, 1))
	observation := metadataObservation(11, id, rollout.AreaSessions)
	observation.Compressed = true
	seedMetadataSourceFile(t, env.source, observation, id)
	fact := completeMetadataEvidence(id, 11)
	fact.ResolvedThroughOffset = 900
	fact.CWD = &rollout.MetadataStringCandidate{Value: "/persisted/project", Provenance: "session_meta", Offset: 700}
	fact.LatestContextModel = stringPointer("model-before-restart")
	fact.LatestContextAtMS = int64Pointer(100)
	fact.LatestContextTurnID = stringPointer("01900000-0001-7000-8000-000000000000")
	commit := MetadataSourceCommit{Observation: observation, SafeFact: fact, PlainCheckpointOffset: 900, GuardHash: []byte("physical-guard")}
	if _, err := CommitMetadata(env.source, MetadataCommitBatch{Threads: []MetadataThreadCommit{{Sources: []MetadataSourceCommit{commit}}}}, MetadataCommitDeps{}, 10); err != nil {
		t.Fatalf("first metadata commit: %v", err)
	}
	if err := env.db.Close(); err != nil {
		t.Fatal(err)
	}
	env = reopenMetadataTestEnvironment(t, env.path)
	var restored MetadataEvidence
	if err := env.source.PrivateRead(func(reader storage.PrivateReader) error {
		var found bool
		var err error
		restored, found, err = LoadMetadataEvidence(reader, 11)
		if err == nil && !found {
			err = fmt.Errorf("persisted metadata fact is absent")
		}
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if restored.ResolvedThroughOffset != 900 || restored.CWD == nil || restored.CWD.Value != "/persisted/project" || restored.LatestContextModel == nil || *restored.LatestContextModel != "model-before-restart" ||
		restored.LatestContextRecordOffset == nil || *restored.LatestContextRecordOffset != 899 {
		t.Fatalf("durable metadata load: %#v", restored)
	}
	accumulator := NewMetadataAccumulator(11, 1, MetadataParserVersion, 20, id, rollout.OwningThreadCandidates{}, &restored)
	classification := accumulator.ObserveRecord(rollout.Record{SourceFileID: 11, Generation: 1, LogicalStartOffset: 900, LogicalEndOffset: 1000,
		JSON: []byte(`{"timestamp":"2026-01-04T00:00:00Z","type":"turn_context","payload":{"turn_id":"01900000-0001-7000-8000-000000000000","model":"model-after-restart"}}`)})
	if classification.Ownership.Kind != rollout.OwnershipOwning {
		t.Fatalf("restarted ownership state: %#v", classification)
	}
	restored = accumulator.Snapshot(1000, 20)
	if restored.CWD == nil || restored.CWD.Value != "/persisted/project" || restored.LatestContextModel == nil || *restored.LatestContextModel != "model-after-restart" {
		t.Fatalf("incremental restart did not preserve/update safe fact: %#v", restored)
	}
	commit.SafeFact = restored
	commit.PlainCheckpointOffset = 1000
	if _, err := CommitMetadata(env.source, MetadataCommitBatch{Threads: []MetadataThreadCommit{{Sources: []MetadataSourceCommit{commit}}}}, MetadataCommitDeps{}, 20); err != nil {
		t.Fatalf("incremental metadata commit: %v", err)
	}
	var physicalCheckpoint, usageCheckpointCount int64
	if err := env.source.PrivateRead(func(reader storage.PrivateReader) error {
		if err := reader.QueryRow("SELECT committed_offset FROM codex_source_checkpoints WHERE source_file_id=? AND consumer_kind='metadata'", 11).Scan(&physicalCheckpoint); err != nil {
			return err
		}
		return reader.QueryRow("SELECT count(*) FROM codex_source_checkpoints WHERE source_file_id=? AND consumer_kind='usage'", 11).Scan(&usageCheckpointCount)
	}); err != nil {
		t.Fatal(err)
	}
	if physicalCheckpoint != 100 || usageCheckpointCount != 0 {
		t.Fatalf("zstd checkpoint must stay at physical accepted size; got offset=%d usage checkpoints=%d", physicalCheckpoint, usageCheckpointCount)
	}
}

func TestMetadataCommitAcceptsOnlyFrozenAppendAndBumpsOnce(t *testing.T) {
	env := newMetadataTestEnvironment(t)
	id := "thread-append"
	seedMetadataThread(t, env.source, metadataMainPatch(t, id, 1))
	observation := metadataObservation(11, id, rollout.AreaSessions)
	observation.Compressed = true
	observation.DiscoveryObservedSize = 200
	observation.DiscoveryObservedMTimeNS = 20
	seedMetadataSourceFile(t, env.source, observation, id)
	fact := completeMetadataEvidence(id, 11)
	fact.ResolvedThroughOffset = 500
	proof := rollout.AcceptedSourceProof{SourceFileID: 11, Generation: 1, Identity: observation.Identity,
		ObservedSize: 201, ObservedMTimeNS: 20, FileStatus: rollout.SourceFilePresent}
	commit := MetadataSourceCommit{Observation: observation, SafeFact: fact, AcceptedProof: &proof, GuardHash: []byte("append-proof")}
	var projectionSizes []int64
	probe := func(tx *source.WriteTx) ([]byte, error) {
		var size int64
		err := tx.Private(func(private storage.PrivateTx) error {
			return private.QueryRow("SELECT observed_size FROM codex_source_files WHERE source_file_id=11").Scan(&size)
		})
		if err == nil {
			projectionSizes = append(projectionSizes, size)
		}
		return []byte(fmt.Sprint(size)), err
	}
	beforeRevision := env.db.CurrentRevision().DataRevision
	if _, err := CommitMetadata(env.source, MetadataCommitBatch{Threads: []MetadataThreadCommit{{Sources: []MetadataSourceCommit{commit}}}}, MetadataCommitDeps{ProjectActiveCompaction: probe}, 30); err == nil {
		t.Fatal("proof differing from frozen discovery was accepted")
	}
	if len(projectionSizes) != 0 || env.db.CurrentRevision().DataRevision != beforeRevision {
		t.Fatalf("rejected proof had visible side effects: projections=%v revision=%d", projectionSizes, env.db.CurrentRevision().DataRevision)
	}
	proof.ObservedSize = 200
	commit.AcceptedProof = &proof
	outcome, err := CommitMetadata(env.source, MetadataCommitBatch{Threads: []MetadataThreadCommit{{Sources: []MetadataSourceCommit{commit}}}}, MetadataCommitDeps{ProjectActiveCompaction: probe}, 30)
	if err != nil {
		t.Fatal(err)
	}
	if !outcome.VisibleChanged || env.db.CurrentRevision().DataRevision != beforeRevision+1 || len(projectionSizes) != 2 || projectionSizes[0] != 100 || projectionSizes[1] != 200 {
		t.Fatalf("append proof visibility boundary: outcome=%#v projections=%v revision=%d", outcome, projectionSizes, env.db.CurrentRevision().DataRevision)
	}
	var acceptedSize, checkpoint int64
	if err := env.source.PrivateRead(func(reader storage.PrivateReader) error {
		if err := reader.QueryRow("SELECT observed_size FROM codex_source_files WHERE source_file_id=11").Scan(&acceptedSize); err != nil {
			return err
		}
		return reader.QueryRow("SELECT committed_offset FROM codex_source_checkpoints WHERE source_file_id=11 AND consumer_kind='metadata'").Scan(&checkpoint)
	}); err != nil {
		t.Fatal(err)
	}
	if acceptedSize != 200 || checkpoint != 200 {
		t.Fatalf("accepted append not atomically checkpointed: size=%d checkpoint=%d", acceptedSize, checkpoint)
	}
}

type metadataTestEnvironment struct {
	path   string
	db     *storage.DB
	source *source.Storage
}

func newMetadataTestEnvironment(t *testing.T) metadataTestEnvironment {
	t.Helper()
	return openMetadataTestEnvironment(t, filepath.Join(t.TempDir(), "metadata.sqlite"))
}

func reopenMetadataTestEnvironment(t *testing.T, path string) metadataTestEnvironment {
	t.Helper()
	return openMetadataTestEnvironment(t, path)
}

func openMetadataTestEnvironment(t *testing.T, path string) metadataTestEnvironment {
	t.Helper()
	db, err := storage.Open(context.Background(), storage.Config{Path: path})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = db.Close() })
	descriptor, err := source.NewDescriptor(domain.SourceCodex, "Codex")
	if err != nil {
		t.Fatal(err)
	}
	run, err := source.NewStorageFactory(db).Context(context.Background(), "phase2b-metadata-test", descriptor)
	if err != nil {
		t.Fatal(err)
	}
	return metadataTestEnvironment{path: path, db: db, source: run.Storage()}
}

func seedMetadataThread(t *testing.T, target *source.Storage, patch domain.ResolvedThreadPatch) {
	t.Helper()
	identity := domain.SessionIdentity{ThreadID: patch.ThreadID, Source: patch.Source, NativeSessionID: patch.NativeSessionID}
	if err := target.Write(func(tx *source.WriteTx) error {
		_, err := tx.UpsertThreadNoRevision(identity, patch)
		return err
	}); err != nil {
		t.Fatal(err)
	}
}

func seedMetadataSourceFile(t *testing.T, target *source.Storage, observation rollout.SourceObservation, threadID string) {
	t.Helper()
	area := "sessions"
	if observation.Area == rollout.AreaArchived {
		area = "archived_sessions"
	}
	if err := target.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec("INSERT INTO codex_source_files(source_file_id,thread_id,current_path,source_area,device_id,inode,file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms) VALUES(?,?,?,?,?,?,?,?,?,'present',?)",
				observation.SourceFileID, threadID, observation.CurrentPath, area, observation.Identity.DeviceID, observation.Identity.Inode,
				observation.Generation, observation.AcceptedObservedSize, observation.AcceptedObservedMTimeNS, int64(1))
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
}

func metadataMainPatch(t *testing.T, id string, resolvedAt int64) domain.ResolvedThreadPatch {
	t.Helper()
	identity, err := domain.NewSessionIdentity(id, domain.SourceCodex, id)
	if err != nil {
		t.Fatal(err)
	}
	patch, err := domain.NewResolvedThreadPatch(identity, resolvedAt)
	if err != nil {
		t.Fatal(err)
	}
	patch.AgentRole = domain.Set(domain.AgentRole("main"))
	patch.RootSessionID = domain.Set(id)
	patch.ProjectKind = domain.Set(domain.ProjectKind("unknown"))
	patch.Archived = domain.Set(false)
	patch.MetadataQualityStatus = "complete"
	return patch
}

func metadataSubagentPatch(t *testing.T, id, parent, root string, resolvedAt int64) domain.ResolvedThreadPatch {
	t.Helper()
	identity, err := domain.NewSessionIdentity(id, domain.SourceCodex, id)
	if err != nil {
		t.Fatal(err)
	}
	patch, err := domain.NewResolvedThreadPatch(identity, resolvedAt)
	if err != nil {
		t.Fatal(err)
	}
	patch.AgentRole = domain.Set(domain.AgentRole("subagent"))
	patch.ParentThreadID = domain.Set(parent)
	patch.RootSessionID = domain.Set(root)
	patch.ProjectKind = domain.Set(domain.ProjectKind("unknown"))
	patch.Archived = domain.Set(false)
	patch.MetadataQualityStatus = "complete"
	return patch
}

func metadataRootInTx(tx *source.WriteTx, threadID string) (string, error) {
	var root sql.NullString
	err := tx.Private(func(private storage.PrivateTx) error {
		return private.QueryRow("SELECT root_session_id FROM threads WHERE thread_id=?", threadID).Scan(&root)
	})
	if err != nil {
		return "", err
	}
	if !root.Valid {
		return "", nil
	}
	return root.String, nil
}

func completeStateSnapshot(facts ...StateThreadFact) StateSnapshot {
	return completeStateSnapshotWithEdges(facts, nil)
}

func completeStateSnapshotWithEdges(facts []StateThreadFact, edges []SpawnEdgeFact) StateSnapshot {
	return StateSnapshot{Status: StateSourceComplete, Threads: facts, SpawnEdges: edges, SpawnEdgesStatus: StateSourceComplete, ThreadColumns: stateThreadColumns(map[string]bool{
		"id": true, "rollout_path": true, "created_at": true, "created_at_ms": true, "updated_at": true, "updated_at_ms": true,
		"archived": true, "cwd": true, "title": true, "name": true, "model": true, "agent_role": true, "agent_path": true,
	})}
}

func completeMetadataEvidence(threadID string, sourceFileID int64) MetadataEvidence {
	return MetadataEvidence{SourceFileID: sourceFileID, FileGeneration: 1, MetadataParserVersion: MetadataParserVersion,
		ResolvedThroughOffset: 100, OwningThreadID: threadID, ContinuationState: "owning_live", UpdatedAtMS: 10,
		OwnershipConfidence: "confirmed", QualityStatus: "complete"}
}

func metadataObservation(sourceFileID int64, threadID string, area rollout.Area) rollout.SourceObservation {
	owner := threadID
	return rollout.SourceObservation{SourceFileID: sourceFileID, Generation: 1,
		CurrentPath: fmt.Sprintf("/tmp/metadata-source-%d.jsonl", sourceFileID), Area: area,
		Identity: rollout.PhysicalIdentity{DeviceID: 1, Inode: sourceFileID + 100}, BoundThreadID: &owner,
		AcceptedObservedSize: 100, AcceptedObservedMTimeNS: 10, DiscoveryObservedSize: 100, DiscoveryObservedMTimeNS: 10}
}

func existingMetadataThread(threadID string) domain.Thread {
	root := threadID
	return domain.Thread{ThreadID: threadID, Source: domain.SourceCodex, NativeSessionID: threadID,
		RootSessionID: &root, AgentRole: "main", Title: stringPointer("old title"), ProjectName: stringPointer("old project"),
		ProjectPath: stringPointer("/old/project"), ProjectKind: "unknown", MetadataModel: stringPointer("old model"),
		CreatedAtMS: int64Pointer(100), UpdatedAtMS: int64Pointer(200), MetadataQualityStatus: "complete"}
}

func existingMetadataSubagent(threadID, parent, root string) domain.Thread {
	thread := existingMetadataThread(threadID)
	thread.ParentThreadID = stringPointer(parent)
	thread.RootSessionID = stringPointer(root)
	thread.AgentRole = "subagent"
	return thread
}

func metadataPatchByID(t *testing.T, result MetadataResolveResult, threadID string) domain.ResolvedThreadPatch {
	t.Helper()
	for _, patch := range result.Patches {
		if patch.ThreadID == threadID {
			return patch
		}
	}
	t.Fatalf("resolver did not return patch for %q; affected=%v diagnostics=%#v", threadID, result.AffectedThreadIDs, result.Diagnostics)
	return domain.ResolvedThreadPatch{}
}

func hasMetadataDiagnostic(result MetadataResolveResult, code, threadID, field string) bool {
	for _, diagnostic := range result.Diagnostics {
		if diagnostic.Code == code && diagnostic.ThreadID == threadID && diagnostic.Field == field {
			return true
		}
	}
	return false
}

func assertRelationshipKept(t *testing.T, patch domain.ResolvedThreadPatch) {
	t.Helper()
	if patch.ParentThreadID.Kind() != domain.PatchKeep || patch.RootSessionID.Kind() != domain.PatchKeep || patch.AgentRole.Kind() != domain.PatchKeep {
		t.Fatalf("unresolved relationship must preserve all historical fields: %#v", patch)
	}
}

func completeSessionSnapshot() SessionNameSnapshot {
	return SessionNameSnapshot{Names: map[string]SessionNameFact{}, Status: SessionSourceComplete}
}

func sessionSnapshotWith(fact SessionNameFact) SessionNameSnapshot {
	snapshot := completeSessionSnapshot()
	snapshot.Names[fact.ThreadID] = fact
	snapshot.Facts = []SessionNameFact{fact}
	return snapshot
}

func sessionName(value string) *SessionNameFact {
	return &SessionNameFact{ThreadID: "title-thread", ThreadName: value}
}

func metadataPathFact(threadID string, sourceFileID int64, role string) MetadataEvidence {
	fact := completeMetadataEvidence(threadID, sourceFileID)
	fact.AgentRoleHint = &rollout.MetadataStringCandidate{Value: role, Provenance: "session_meta_role", Offset: 1}
	fact.AgentPath = &rollout.MetadataStringCandidate{Value: "sub_agent", Provenance: "session_meta", Offset: 2}
	return fact
}

func stringPointer(value string) *string { return &value }

func int64Pointer(value int64) *int64 { return &value }

func boolPointer(value bool) *bool { return &value }

func equalStrings(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}
