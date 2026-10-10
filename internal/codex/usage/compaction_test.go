package usage

import (
	"database/sql"
	"errors"
	"fmt"
	"strings"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

func modernProjectionBatch(t *testing.T, sourceFileID int64, unknown string) ProcessBatch {
	t.Helper()
	owner := "owner"
	state := processorTestState()
	state.Source.SourceFileID = sourceFileID
	state.Source.OwningThreadID = owner
	state.Source.RootSessionID = owner
	state.Source.PreviousTotal = usagePointer(testUsage(t, 50, 0, nil, 0, 0))
	state.Source.PreviousTotalOffset = int64Pointer(900)
	model := "model"
	state.Source.ActiveModel = &model
	state.Source.ActiveModelOffset = int64Pointer(0)
	windowStart := uint64(10)
	state.Carry.OpenWindowStartOffset = &windowStart

	legacy := tokenCountRecord(testUsage(t, 80, 0, nil, 0, 0), UsageValue{State: UsageValueMissing}, 1000)
	legacy.Parsed.StartOffset, legacy.Parsed.EndOffset = 10, 20
	modernTotal := testUsage(t, 100, 0, nil, 0, 0)
	normal := processorRecord(RawResponseUsage, 20, 30, 2000)
	normal.Parsed.TimestampMS = int64Pointer(110)
	normal.Parsed.Response = &ResponseEvidence{
		ResponseID: "normal-r", ThreadID: owner,
		Usage: validUsage(testUsage(t, 30, 0, nil, 0, 0)), ThreadTokenUsage: validUsage(modernTotal),
	}
	compactionUsage := validUsage(testUsage(t, 20, 0, nil, 0, 0))
	session := ""
	if unknown == "domain" {
		session = "other-session"
	}
	compactionResponse := &ResponseEvidence{
		ResponseID: "compact-r", ThreadID: owner, SessionID: session,
		TurnID: "turn", Usage: compactionUsage, ThreadTokenUsage: validUsage(modernTotal),
	}
	if unknown == "usage" {
		compactionResponse.Usage = UsageValue{State: UsageValueMissing}
	}
	compacted := processorRecord(RawCompacted, 30, 40, 3000)
	compacted.Parsed.TimestampMS = int64Pointer(120)
	compacted.Parsed.Compaction = &CompactionEvidence{ResponseID: "compact-r", Latest: compactionResponse}
	batch, err := ProcessRecords(state, []OwnedRecord{legacy, normal, compacted}, 40)
	if err != nil {
		t.Fatal(err)
	}
	for index := range batch.Candidates {
		if batch.Candidates[index].EvidenceKind == EvidenceLegacy {
			batch.Candidates[index].CurrentTotal = nil
		}
	}
	return batch
}

func TestReconcileUsesModernTotalMinusCompactionForWindowCoverage(t *testing.T) {
	owner := "owner"
	for _, unknown := range []string{"", "usage", "domain"} {
		name := "known modern projection"
		if unknown != "" {
			name = "unknown compaction " + unknown
		}
		t.Run(name, func(t *testing.T) {
			run, epoch, ids := setupReconcileStorage(t, []string{owner})
			batch := modernProjectionBatch(t, ids[0], unknown)
			result := reconcileRootForTest(t, run, epoch, []ProcessBatch{batch})[0]
			if result.Fatal != nil {
				t.Fatalf("modern reconciliation returned fatal: %+v", result.Fatal)
			}
			if unknown == "" {
				var legacyID string
				for _, candidate := range batch.Candidates {
					if candidate.EvidenceKind == EvidenceLegacy {
						legacyID = candidate.EventID
					}
				}
				if len(result.DeleteEventIDs) != 1 || result.DeleteEventIDs[0] != legacyID {
					t.Fatalf("modern total minus known compaction did not prove the legacy delta: %+v", result)
				}
				if len(result.MarkerUpserts) != 1 || result.MarkerUpserts[0].ResolvedEventID == nil {
					t.Fatalf("known compaction marker was not resolved: %+v", result.MarkerUpserts)
				}
				return
			}
			if len(result.DeleteEventIDs) != 0 || len(result.WindowUpserts) != 1 {
				t.Fatalf("unknown compaction relation was guessed as coverage/reset: %+v", result)
			}
			window, err := DecodeLegacyReconciliationWindow(result.WindowUpserts[0].StateJSON)
			if err != nil || len(window.ProposalEventIDs) != 1 || window.ChainState.Kind != "continuous" {
				t.Fatalf("unknown relation did not preserve the open legacy proposal and chain: %+v err=%v", window, err)
			}
		})
	}
	t.Run("insufficient modern total remains unknown", func(t *testing.T) {
		run, epoch, ids := setupReconcileStorage(t, []string{owner})
		batch := modernProjectionBatch(t, ids[0], "")
		tooLarge := testUsage(t, 120, 0, nil, 0, 0)
		for index := range batch.Candidates {
			if batch.Candidates[index].Operation != OperationCompaction {
				continue
			}
			batch.Candidates[index].Usage = tooLarge
			response := *batch.Candidates[index].Response
			response.Usage = validUsage(tooLarge)
			batch.Candidates[index].Response = &response
		}
		for index := range batch.Compactions {
			compaction := *batch.Compactions[index].Record.Compaction
			latest := *compaction.Latest
			latest.Usage = validUsage(tooLarge)
			compaction.Latest = &latest
			batch.Compactions[index].Record.Compaction = &compaction
		}
		result := reconcileRootForTest(t, run, epoch, []ProcessBatch{batch})[0]
		if result.Fatal != nil || len(result.DeleteEventIDs) != 0 || len(result.WindowUpserts) != 1 {
			t.Fatalf("insufficient modern total was treated as fatal or proven coverage: %+v", result)
		}
	})
}

func TestReconcileModernCompactionSumOverflowIsFatal(t *testing.T) {
	owner := "owner"
	run, epoch, ids := setupReconcileStorage(t, []string{owner})
	state := processorTestState()
	state.Source.SourceFileID = ids[0]
	state.Source.OwningThreadID = owner
	state.Source.RootSessionID = owner
	model := "model"
	state.Source.ActiveModel = &model
	state.Source.ActiveModelOffset = int64Pointer(0)
	windowStart := uint64(10)
	state.Carry.OpenWindowStartOffset = &windowStart
	maxUsage := testUsage(t, int64(^uint64(0)>>1), 0, nil, 0, 0)
	records := make([]OwnedRecord, 0, 2)
	for index := 0; index < 2; index++ {
		start := int64(10 + index*10)
		record := processorRecord(RawCompacted, start, start+10, start+10)
		record.Parsed.TimestampMS = int64Pointer(100 + start)
		responseID := fmt.Sprintf("overflow-%d", index)
		latest := &ResponseEvidence{
			ResponseID: responseID, ThreadID: owner,
			Usage: validUsage(maxUsage), ThreadTokenUsage: validUsage(maxUsage),
		}
		record.Parsed.Compaction = &CompactionEvidence{ResponseID: responseID, Latest: latest}
		records = append(records, record)
	}
	batch, err := ProcessRecords(state, records, 40)
	if err != nil {
		t.Fatal(err)
	}
	results := reconcileRootForTest(t, run, epoch, []ProcessBatch{batch})
	if len(results) != 1 || results[0].Fatal == nil || results[0].Fatal.Code != FatalArithmeticOverflow {
		t.Fatalf("modern compaction sum overflow did not become FatalArithmeticOverflow: %+v", results)
	}
}

func compensationTestTurn(sourceFileID int64, owner string) TurnWrite {
	end := int64(100)
	model := "model"
	return TurnWrite{
		SourceFileID: sourceFileID, Generation: 1, TurnKey: "turn", ThreadID: owner,
		StartedAtMS: int64Pointer(100), EndedAtMS: int64Pointer(200), StartOffset: 0, EndOffset: &end,
		Status: TurnCompleted, StartTotal: usagePointer(testUsageForTurn(50)), LastTotal: usagePointer(testUsageForTurn(60)),
		Accounted: sharedusage.Zero(), ModelState: TurnValueSingle, SingleModel: &model,
		ReasoningEffortState: TurnValueNone, QualityStatus: "complete", StateThroughOffset: end,
	}
}

func testUsageForTurn(input int64) sharedusage.NormalizedTokenUsage {
	value, _ := sharedusage.NewNormalizedTokenUsage(input, 0, nil, 0, 0, input)
	return value
}

func TestReconcileTurnCompensationRecomputesAccountedAndHonorsBlockers(t *testing.T) {
	owner := "owner"
	run, epoch, ids := setupReconcileStorage(t, []string{owner})
	turnKey := "turn"
	candidate := explicitCandidate(ids[0], owner, owner, "turn-response", testUsage(t, 3, 0, nil, 0, 0), 20, 30, 150, "model", &turnKey, OperationResponse)
	base := ProcessBatch{SourceState: reconcileSourceState(ids[0], owner, owner), Candidates: []UsageCandidate{candidate},
		TurnUpserts: []TurnWrite{compensationTestTurn(ids[0], owner)}, LogicalSafeOffset: 100}
	result := reconcileRootForTest(t, run, epoch, []ProcessBatch{base})[0]
	var compensation *sharedusage.CanonicalUsageEventWrite
	for index := range result.Events {
		if result.Events[index].Kind == sharedusage.EventKindTurnCompensation {
			compensation = &result.Events[index]
		}
	}
	if compensation == nil || compensation.Usage.InputTokens != 7 {
		t.Fatalf("turn accounted usage was not recomputed before compensation: events=%+v", result.Events)
	}
	blockers := []struct {
		name string
		set  func(*CompensationBlocks)
	}{
		{"start missing", func(value *CompensationBlocks) { value.StartMissing = true }},
		{"time missing", func(value *CompensationBlocks) { value.TimeMissing = true }},
		{"reset", func(value *CompensationBlocks) { value.Reset = true }},
		{"ownership gap", func(value *CompensationBlocks) { value.OwnershipGap = true }},
		{"parser gap", func(value *CompensationBlocks) { value.ParserGap = true }},
		{"required invalid", func(value *CompensationBlocks) { value.RequiredInvalid = true }},
		{"model unresolved", func(value *CompensationBlocks) { value.ModelUnresolved = true }},
	}
	for _, blocker := range blockers {
		t.Run(blocker.name, func(t *testing.T) {
			batch := base
			turn := compensationTestTurn(ids[0], owner)
			blocker.set(&turn.Blocks)
			batch.TurnUpserts = []TurnWrite{turn}
			result := reconcileRootForTest(t, run, epoch, []ProcessBatch{batch})[0]
			for _, event := range result.Events {
				if event.Kind == sharedusage.EventKindTurnCompensation {
					t.Fatalf("compensation ignored durable blocker %q: %+v", blocker.name, event)
				}
			}
		})
	}
}

func TestTurnCompactionBlockerUsesFinalMarkerAndWindowView(t *testing.T) {
	owner := "owner"
	run, epoch, ids := setupReconcileStorage(t, []string{owner})
	marker := CompactionMarkerWrite{
		SourceFileID: ids[0], Generation: 1, StartOffset: 50, EndOffset: 51,
		OwningThreadID: owner, RootSessionID: owner, OccurredAtMS: int64Pointer(150),
		Model: stringPointer("model"), ResponseID: stringPointer("compact-r"), UnknownReason: markerReasonPointer(MarkerUsageMissing),
	}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := writeMarkers(private, epoch, []CompactionMarkerWrite{marker})
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	turn := compensationTestTurn(ids[0], owner)
	resolved := marker
	resolved.ResolvedEventID = stringPointer(ResponseEventID(owner, "compact-r"))
	resolved.UnknownReason = nil
	compactionUsage := testUsage(t, 3, 0, nil, 0, 0)
	response := &ResponseEvidence{
		ResponseID: "compact-r", ThreadID: owner, TurnID: turn.TurnKey,
		Usage: validUsage(compactionUsage),
	}
	candidate := explicitCandidate(ids[0], owner, owner, "compact-r", compactionUsage, 50, 51, 150, "model", &turn.TurnKey, OperationCompaction)
	candidate.Response = response
	pending := PendingUsageEvidence{Record: PendingEvidenceRecord{
		Kind: PendingCompacted, TimestampMS: int64Pointer(150), StartOffset: 50, EndOffset: 51,
		Compaction: &CompactionEvidence{ResponseID: "compact-r", Latest: response},
	}, Model: stringPointer("model")}
	batch := ProcessBatch{
		SourceState: reconcileSourceState(ids[0], owner, owner), Candidates: []UsageCandidate{candidate},
		Compactions: []PendingUsageEvidence{pending}, TurnUpserts: []TurnWrite{turn}, LogicalSafeOffset: 100,
	}
	result := reconcileRootForTest(t, run, epoch, []ProcessBatch{batch})[0]
	if result.Fatal != nil || len(result.MarkerUpserts) != 1 || result.MarkerUpserts[0].ResolvedEventID == nil || len(result.Events) != 2 {
		t.Fatalf("resolved marker patch did not replace the durable unresolved marker before compensation: %+v", result)
	}
	var compensated bool
	for _, event := range result.Events {
		if event.Kind == sharedusage.EventKindTurnCompensation && event.Usage.InputTokens == 7 {
			compensated = true
		}
	}
	if !compensated {
		t.Fatalf("resolved final marker view still blocked turn compensation: %+v", result.Events)
	}
	readTurnBlocker := func(patch ReconcileResult) bool {
		t.Helper()
		blocked := true
		if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
			var err error
			blocked, err = hasUnresolvedTurnCompaction(reader, epoch, turn, nil, &patch)
			return err
		}); err != nil {
			t.Fatal(err)
		}
		return blocked
	}
	if readTurnBlocker(ReconcileResult{}) != true || readTurnBlocker(ReconcileResult{MarkerUpserts: []CompactionMarkerWrite{resolved}}) ||
		readTurnBlocker(ReconcileResult{MarkerDeletes: []PrivateRowKey{{SourceFileID: ids[0], Generation: 1, StartOffset: 50}}}) {
		t.Fatal("dynamic blocker did not use the final marker view after resolved upsert or delete")
	}
}

func markerReasonPointer(value MarkerUnknownReason) *MarkerUnknownReason {
	return &value
}

func TestTurnWindowBlockerUsesOperationAndFinalWindowView(t *testing.T) {
	owner := "owner"
	run, epoch, ids := setupReconcileStorage(t, []string{owner})
	responseID := "compact-r"
	usage := testUsage(t, 3, 0, nil, 0, 0)
	event := commitTestEvent(ResponseEventID(owner, responseID), owner, usage)
	event.TurnKey = stringPointer("turn")
	fact := EventFactWrite{EventID: event.EventID, OwningThreadID: owner, ResponseID: &responseID,
		EvidenceKind: EvidenceExplicit, Operation: OperationCompaction}
	occurrence := OccurrenceWrite{SourceFileID: ids[0], Generation: 1, StartOffset: 50, EndOffset: 51, EventID: event.EventID}
	windowState := LegacyReconciliationWindow{
		Version: 1, PreviousTotal: UsageValue{State: UsageValueMissing}, CurrentTotal: UsageValue{State: UsageValueMissing},
		LastUsage: UsageValue{State: UsageValueMissing}, ExplicitResponseIDs: []string{responseID},
		LegacyCoveredResponseIDs: []string{}, ProposalEventIDs: []string{}, TurnAccountedBefore: sharedusage.Zero(),
		ChainState: LegacyWindowChainState{Kind: "continuous"}, Closed: false,
	}
	encoded, err := CanonicalLegacyReconciliationWindowJSON(windowState)
	if err != nil {
		t.Fatal(err)
	}
	window := WindowWrite{SourceFileID: ids[0], Generation: 1, StartOffset: 40, EndOffset: 60,
		OwningThreadID: owner, TurnKey: stringPointer("turn"), StateJSON: encoded}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		return seedReconciliationRows(tx, source.UsageTargetActive, epoch, event, []EventFactWrite{fact}, []OccurrenceWrite{occurrence}, nil, []WindowWrite{window})
	}); err != nil {
		t.Fatal(err)
	}
	turn := compensationTestTurn(ids[0], owner)
	readTurnBlocker := func(patch ReconcileResult) bool {
		t.Helper()
		blocked := false
		if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
			var err error
			blocked, err = hasUnresolvedTurnCompaction(reader, epoch, turn, nil, &patch)
			return err
		}); err != nil {
			t.Fatal(err)
		}
		return blocked
	}
	if !readTurnBlocker(ReconcileResult{}) {
		t.Fatal("durable operation='compaction' in an uncovered turn window was missed")
	}
	covered := windowState
	covered.LegacyCoveredResponseIDs = []string{responseID}
	coveredJSON, err := CanonicalLegacyReconciliationWindowJSON(covered)
	if err != nil {
		t.Fatal(err)
	}
	window.StateJSON = coveredJSON
	patch := ReconcileResult{WindowUpserts: []WindowWrite{window}}
	if readTurnBlocker(patch) {
		t.Fatal("window upsert that covered the compaction remained a dynamic blocker")
	}
	patch = ReconcileResult{WindowDeletes: []PrivateRowKey{{SourceFileID: ids[0], Generation: 1, StartOffset: 40}}}
	if readTurnBlocker(patch) {
		t.Fatal("window deletion remained a dynamic blocker")
	}
}

func commitTestEvent(eventID, owner string, usage sharedusage.NormalizedTokenUsage) sharedusage.CanonicalUsageEventWrite {
	return sharedusage.CanonicalUsageEventWrite{
		EventID: eventID, Kind: sharedusage.EventKindNormal, OccurredAtMS: 100,
		ThreadID: owner, RootSessionID: owner, Model: "model", Usage: usage, CreatedAtMS: 1,
	}
}

func seedReconciliationRows(tx *source.WriteTx, target source.UsageWriteTarget, epoch int64,
	event sharedusage.CanonicalUsageEventWrite, facts []EventFactWrite, occurrences []OccurrenceWrite,
	markers []CompactionMarkerWrite, windows []WindowWrite) error {
	if _, err := tx.WriteUsageNoRevision(target, event); err != nil {
		return err
	}
	return tx.Private(func(private storage.PrivateTx) error {
		if _, err := writeFacts(private, epoch, facts); err != nil {
			return err
		}
		if _, err := writeOccurrences(private, epoch, occurrences, nil, event.CreatedAtMS); err != nil {
			return err
		}
		if _, err := writeMarkers(private, epoch, markers); err != nil {
			return err
		}
		if _, err := writeWindows(private, epoch, windows); err != nil {
			return err
		}
		return nil
	})
}

func TestCommitRetargetsOccurrenceAndStripsSurvivingWindowBeforeDelete(t *testing.T) {
	owner := "owner"
	run, epoch, ids := setupReconcileStorage(t, []string{owner})
	legacyID := strings.Repeat("a", 64)
	legacyUsage := testUsage(t, 3, 0, nil, 0, 0)
	legacy := commitTestEvent(legacyID, owner, legacyUsage)
	legacyFact := EventFactWrite{EventID: legacyID, OwningThreadID: owner, EvidenceKind: EvidenceLegacy, Operation: OperationResponse}
	occurrence := OccurrenceWrite{SourceFileID: ids[0], Generation: 1, StartOffset: 10, EndOffset: 20, EventID: legacyID}
	windowState := LegacyReconciliationWindow{
		Version: 1, PreviousTotal: UsageValue{State: UsageValueMissing}, CurrentTotal: UsageValue{State: UsageValueMissing},
		LastUsage: UsageValue{State: UsageValueMissing}, ExplicitResponseIDs: []string{}, LegacyCoveredResponseIDs: []string{},
		ProposalEventIDs: []string{legacyID}, TurnAccountedBefore: sharedusage.Zero(),
		ChainState: LegacyWindowChainState{Kind: "continuous"}, Closed: false,
	}
	windowJSON, err := CanonicalLegacyReconciliationWindowJSON(windowState)
	if err != nil {
		t.Fatal(err)
	}
	window := WindowWrite{SourceFileID: ids[0], Generation: 1, StartOffset: 10, EndOffset: 20,
		OwningThreadID: owner, StateJSON: windowJSON}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		return seedReconciliationRows(tx, source.UsageTargetActive, epoch, legacy, []EventFactWrite{legacyFact}, []OccurrenceWrite{occurrence}, nil, []WindowWrite{window})
	}); err != nil {
		t.Fatal(err)
	}
	responseID := "replacement"
	newUsage := testUsage(t, 3, 0, nil, 0, 0)
	newEvent := commitTestEvent(ResponseEventID(owner, responseID), owner, newUsage)
	newFact := EventFactWrite{EventID: newEvent.EventID, OwningThreadID: owner, ResponseID: &responseID,
		EvidenceKind: EvidenceExplicit, Operation: OperationResponse}
	patch := ReconcileResult{
		DeleteEventIDs: []string{legacyID}, Events: []sharedusage.CanonicalUsageEventWrite{newEvent},
		Facts:       []EventFactWrite{newFact},
		Occurrences: []OccurrenceWrite{{SourceFileID: ids[0], Generation: 1, StartOffset: 10, EndOffset: 20, EventID: newEvent.EventID}},
	}
	var changed bool
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		changed, err = Commit(tx, source.UsageTargetActive, patch, 77)
		return err
	}); err != nil {
		t.Fatalf("explicit occurrence retarget and surviving-window rewrite failed: %v", err)
	}
	if !changed {
		t.Fatal("canonical replacement did not report its visible insertion")
	}
	if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
		var occurrenceID string
		var occurrenceTime int64
		if err := reader.QueryRow(`SELECT event_id,created_at_ms FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?`, epoch).Scan(&occurrenceID, &occurrenceTime); err != nil {
			return err
		}
		var eventCount, factCount int
		if err := reader.QueryRow(`SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=?`, epoch).Scan(&eventCount); err != nil {
			return err
		}
		if err := reader.QueryRow(`SELECT count(*) FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=?`, epoch).Scan(&factCount); err != nil {
			return err
		}
		if occurrenceID != newEvent.EventID || occurrenceTime != 1 || eventCount != 1 || factCount != 1 {
			t.Fatalf("retarget/delete topology was not applied atomically: occurrence=%q time=%d events=%d facts=%d", occurrenceID, occurrenceTime, eventCount, factCount)
		}
		var raw string
		if err := reader.QueryRow(`SELECT state_json FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=?`, epoch).Scan(&raw); err != nil {
			return err
		}
		stored, err := DecodeLegacyReconciliationWindow([]byte(raw))
		if err != nil || len(stored.ProposalEventIDs) != 0 {
			t.Fatalf("surviving window retained a deleted proposal reference: %+v err=%v", stored, err)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestCommitDeletesMarkersBeforeCanonicalFactsAndRejectsMissingDelete(t *testing.T) {
	for _, deleteMarker := range []bool{true, false} {
		name := "missing marker delete rejected"
		if deleteMarker {
			name = "marker deleted first"
		}
		t.Run(name, func(t *testing.T) {
			owner := "owner"
			run, epoch, ids := setupReconcileStorage(t, []string{owner})
			responseID := "compact-r"
			event := commitTestEvent(ResponseEventID(owner, responseID), owner, testUsage(t, 3, 0, nil, 0, 0))
			fact := EventFactWrite{EventID: event.EventID, OwningThreadID: owner, ResponseID: &responseID,
				EvidenceKind: EvidenceExplicit, Operation: OperationCompaction}
			marker := CompactionMarkerWrite{
				SourceFileID: ids[0], Generation: 1, StartOffset: 50, EndOffset: 51,
				OwningThreadID: owner, RootSessionID: owner, OccurredAtMS: int64Pointer(150),
				Model: stringPointer("model"), ResponseID: &responseID, ResolvedEventID: stringPointer(event.EventID),
			}
			if err := run.Storage().Write(func(tx *source.WriteTx) error {
				return seedReconciliationRows(tx, source.UsageTargetActive, epoch, event, []EventFactWrite{fact}, nil,
					[]CompactionMarkerWrite{marker}, nil)
			}); err != nil {
				t.Fatal(err)
			}
			patch := ReconcileResult{DeleteEventIDs: []string{event.EventID}}
			if deleteMarker {
				patch.MarkerDeletes = []PrivateRowKey{{SourceFileID: ids[0], Generation: 1, StartOffset: 50}}
			}
			err := run.Storage().Write(func(tx *source.WriteTx) error {
				_, err := Commit(tx, source.UsageTargetActive, patch, 80)
				return err
			})
			if deleteMarker {
				if err != nil {
					t.Fatalf("marker-first delete failed: %v", err)
				}
			} else if err == nil {
				t.Fatal("canonical delete with a surviving resolved marker was accepted")
			}
			if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
				var events, markers int
				if err := reader.QueryRow(`SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=? AND event_id=?`, epoch, event.EventID).Scan(&events); err != nil {
					return err
				}
				if err := reader.QueryRow(`SELECT count(*) FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=?`, epoch).Scan(&markers); err != nil {
					return err
				}
				if deleteMarker && (events != 0 || markers != 0) || !deleteMarker && (events != 1 || markers != 1) {
					t.Fatalf("marker/delete rollback result mismatch: events=%d markers=%d", events, markers)
				}
				return nil
			}); err != nil {
				t.Fatal(err)
			}
		})
	}
}

func TestCommitActiveDuplicatePreservesCanonicalMetadataAndIsNotVisible(t *testing.T) {
	owner := "owner"
	run, _, ids := setupReconcileStorage(t, []string{owner, owner})
	usage := testUsage(t, 3, 0, nil, 0, 0)
	event := commitTestEvent(ResponseEventID(owner, "duplicate"), owner, usage)
	first := ReconcileResult{Events: []sharedusage.CanonicalUsageEventWrite{event}, Occurrences: []OccurrenceWrite{{
		SourceFileID: ids[0], Generation: 1, StartOffset: 10, EndOffset: 20, EventID: event.EventID,
	}}}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := Commit(tx, source.UsageTargetActive, first, 10)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	cost := int64(99)
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE usage_events SET estimated_cost_nanos_usd=? WHERE source='codex' AND source_epoch=1 AND event_id=?`, cost, event.EventID)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	second := ReconcileResult{Events: []sharedusage.CanonicalUsageEventWrite{event}, Occurrences: []OccurrenceWrite{{
		SourceFileID: ids[1], Generation: 1, StartOffset: 30, EndOffset: 40, EventID: event.EventID,
	}}}
	var changed bool
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		changed, err = Commit(tx, source.UsageTargetActive, second, 20)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if changed {
		t.Fatal("duplicate canonical event or added occurrence was reported as visible")
	}
	if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
		var created int64
		var storedCost int64
		if err := reader.QueryRow(`SELECT created_at_ms,estimated_cost_nanos_usd FROM usage_events WHERE source='codex' AND source_epoch=1 AND event_id=?`, event.EventID).Scan(&created, &storedCost); err != nil {
			return err
		}
		if created != 10 || storedCost != cost {
			t.Fatalf("active duplicate overwrote canonical creation metadata: created=%d cost=%d", created, storedCost)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestCommitBuildBootstrapAndActiveCopyRules(t *testing.T) {
	t.Run("bootstrap never compares active", func(t *testing.T) {
		owner := "owner"
		run := newUsageTestRun(t)
		if err := run.Storage().Write(func(tx *source.WriteTx) error {
			if err := seedUsageTestThreads(tx, owner); err != nil {
				return err
			}
			if err := tx.EnsureUsageEpoch(); err != nil {
				return err
			}
			buildEpoch, err := tx.BeginOrResumeUsageBuild(UsageParserVersion)
			if err != nil {
				return err
			}
			state, err := tx.UsageEpochState()
			if err != nil {
				return err
			}
			if state.ActiveEpoch != 0 {
				return errors.New("fresh usage epoch unexpectedly has an active epoch")
			}
			event := commitTestEvent("bootstrap-event", owner, testUsage(t, 3, 0, nil, 0, 0))
			if _, err := Commit(tx, source.UsageTargetBuild, ReconcileResult{Events: []sharedusage.CanonicalUsageEventWrite{event}}, 30); err != nil {
				return err
			}
			if _, err := tx.ActivateUsageBuild(buildEpoch, UsageParserVersion); err != nil {
				return err
			}
			state, err = tx.UsageEpochState()
			if err != nil {
				return err
			}
			if state.ActiveEpoch != buildEpoch {
				return fmt.Errorf("bootstrap activated epoch %d, want %d", state.ActiveEpoch, buildEpoch)
			}
			return nil
		}); err != nil {
			t.Fatalf("bootstrap build touched an absent active target or failed activation: %v", err)
		}
	})

	t.Run("copy identical active and do not copy conflicting cost", func(t *testing.T) {
		owner := "owner"
		run, _, _ := setupReconcileStorage(t, []string{owner})
		var buildEpoch int64
		identicalUsage := testUsage(t, 3, 0, nil, 0, 0)
		conflictingActiveUsage := testUsage(t, 4, 0, nil, 0, 0)
		if err := run.Storage().Write(func(tx *source.WriteTx) error {
			identical := commitTestEvent("will-copy", owner, identicalUsage)
			identical.CreatedAtMS = 11
			identical.EstimatedCostNanosUSD = int64Pointer(77)
			if _, err := tx.WriteUsageNoRevision(source.UsageTargetActive, identical); err != nil {
				return err
			}
			conflict := commitTestEvent("will-conflict", owner, conflictingActiveUsage)
			if _, err := tx.WriteUsageNoRevision(source.UsageTargetActive, conflict); err != nil {
				return err
			}
			var err error
			buildEpoch, err = tx.BeginOrResumeUsageBuild(UsageParserVersion)
			return err
		}); err != nil {
			t.Fatal(err)
		}
		identical := commitTestEvent("will-copy", owner, identicalUsage)
		conflictingBuild := commitTestEvent("will-conflict", owner, testUsage(t, 8, 0, nil, 0, 0))
		if err := run.Storage().Write(func(tx *source.WriteTx) error {
			_, err := Commit(tx, source.UsageTargetBuild, ReconcileResult{Events: []sharedusage.CanonicalUsageEventWrite{identical, conflictingBuild}}, 33)
			return err
		}); err != nil {
			t.Fatal(err)
		}
		if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
			var created int64
			var cost sql.NullInt64
			if err := reader.QueryRow(`SELECT created_at_ms,estimated_cost_nanos_usd FROM usage_events WHERE source='codex' AND source_epoch=? AND event_id=?`, buildEpoch, "will-copy").Scan(&created, &cost); err != nil {
				return err
			}
			if created != 11 || !cost.Valid || cost.Int64 != 77 {
				t.Fatalf("identical active event did not preserve cost and creation time: %d %+v", created, cost)
			}
			if err := reader.QueryRow(`SELECT created_at_ms,estimated_cost_nanos_usd FROM usage_events WHERE source='codex' AND source_epoch=? AND event_id=?`, buildEpoch, "will-conflict").Scan(&created, &cost); err != nil {
				return err
			}
			if created != 33 || cost.Valid {
				t.Fatalf("conflicting active event leaked old cost into build: %d %+v", created, cost)
			}
			return nil
		}); err != nil {
			t.Fatal(err)
		}
	})
}
