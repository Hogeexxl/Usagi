package usage

import (
	"bufio"
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/codex/rollout"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

func setupReconcileStorage(t *testing.T, owners []string) (source.RunContext, int64, []int64) {
	t.Helper()
	run := newUsageTestRun(t)
	var epoch int64
	ids := make([]int64, len(owners))
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		uniqueOwners := make(map[string]struct{})
		for _, owner := range owners {
			uniqueOwners[owner] = struct{}{}
		}
		ownerList := make([]string, 0, len(uniqueOwners))
		for owner := range uniqueOwners {
			ownerList = append(ownerList, owner)
		}
		if err := seedUsageTestThreads(tx, ownerList...); err != nil {
			return err
		}
		if err := activateUsageTestEpoch(tx); err != nil {
			return err
		}
		state, err := tx.UsageEpochState()
		if err != nil {
			return err
		}
		epoch = state.ActiveEpoch
		for index, owner := range owners {
			ids[index], err = insertUsageTestSourceFile(tx, "reconcile-"+string(rune('a'+index))+".jsonl", owner)
			if err != nil {
				return err
			}
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	return run, epoch, ids
}

func reconcileSourceState(sourceFileID int64, owner, root string) SourceState {
	state := processorTestState().Source
	state.SourceFileID = sourceFileID
	state.OwningThreadID = owner
	state.RootSessionID = root
	state.ObservedRawSize = 128
	state.RawTailStatus = RawTailUnverified
	state.ResolvedThroughOffset = 0
	state.ReconciliationStateJSON, _ = CanonicalReconciliationCarryJSON(NewReconciliationCarry())
	return state
}

func explicitCandidate(sourceFileID int64, owner, root, responseID string, usage sharedusage.NormalizedTokenUsage,
	start, end, occurredAt int64, model string, turnKey *string, operation Operation) UsageCandidate {
	evidence := &ResponseEvidence{ResponseID: responseID, ThreadID: owner, Usage: UsageValue{State: UsageValueValid, Value: usage}}
	return UsageCandidate{
		SourceFileID: sourceFileID, Generation: 1, EventID: ResponseEventID(owner, responseID),
		EvidenceKind: EvidenceExplicit, Operation: operation, OccurredAtMS: occurredAt,
		StartOffset: start, EndOffset: end, OwningThreadID: owner, RootSessionID: root,
		TurnKey: cloneString(turnKey), Model: model, Response: evidence, Usage: usage,
	}
}

func legacyCandidate(sourceFileID int64, owner, root string, start, end int64,
	previous, current, usage sharedusage.NormalizedTokenUsage, turnKey *string) UsageCandidate {
	occurredAtMS := int64(100)
	eventID := LegacyEventID(owner, turnKey, 1, occurredAtMS, &previous, current, usage, "model", nil)
	return UsageCandidate{
		SourceFileID: sourceFileID, Generation: 1, EventID: eventID,
		EventKind: 1, EvidenceKind: EvidenceLegacy, Operation: OperationResponse,
		OccurredAtMS: occurredAtMS, StartOffset: start, EndOffset: end, OwningThreadID: owner,
		RootSessionID: root, TurnKey: cloneString(turnKey), Model: "model",
		PreviousTotal: usagePointer(previous), CurrentTotal: usagePointer(current), Usage: usage,
	}
}

func reconcileRootForTest(t *testing.T, run source.RunContext, epoch int64, batches []ProcessBatch) []ReconcileResult {
	t.Helper()
	var results []ReconcileResult
	if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
		var err error
		results, err = ReconcileRoot(reader, epoch, batches)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	return results
}

func TestLegacyReconciliationWindowCanonicalCodec(t *testing.T) {
	eventID := strings.Repeat("a", 64)
	window := LegacyReconciliationWindow{
		Version: 1, PreviousTotal: UsageValue{State: UsageValueMissing},
		CurrentTotal: UsageValue{State: UsageValueInvalid}, LastUsage: UsageValue{State: UsageValueMissing},
		ExplicitResponseIDs:      []string{"response-z", "response-a", "response-z"},
		LegacyCoveredResponseIDs: []string{}, ProposalEventIDs: []string{eventID},
		TurnAccountedBefore: sharedusage.Zero(), ChainState: LegacyWindowChainState{Kind: "continuous"}, Closed: true,
	}
	encoded, err := CanonicalLegacyReconciliationWindowJSON(window)
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := DecodeLegacyReconciliationWindow(encoded)
	if err != nil {
		t.Fatal(err)
	}
	reencoded, err := CanonicalLegacyReconciliationWindowJSON(decoded)
	if err != nil || !bytes.Equal(encoded, reencoded) {
		t.Fatalf("window wire did not round-trip canonically: %s, err=%v", reencoded, err)
	}
	if !bytes.Contains(encoded, []byte(`"explicit_response_ids":["response-a","response-z"]`)) {
		t.Fatalf("response identifiers were not sorted and deduplicated: %s", encoded)
	}
	invalid := window
	invalid.ProposalEventIDs = []string{strings.Repeat("A", 64)}
	if _, err := CanonicalLegacyReconciliationWindowJSON(invalid); err == nil {
		t.Fatal("non-lowercase proposal event ID was accepted")
	}
	if _, err := DecodeLegacyReconciliationWindow(bytes.Replace(encoded, []byte(eventID), []byte(strings.Repeat("A", 64)), 1)); err == nil {
		t.Fatal("decoder accepted a noncanonical proposal event ID")
	}
	if _, err := DecodeLegacyReconciliationWindow(bytes.Replace(encoded, []byte(`{"version":1`), []byte(`{"version": 1`), 1)); err == nil {
		t.Fatal("decoder accepted noncanonical whitespace")
	}
}

func TestReconcileRootStagesDuplicateResponsesAcrossSources(t *testing.T) {
	owner := "owner"
	run, epoch, ids := setupReconcileStorage(t, []string{owner, owner})
	usage := testUsage(t, 4, 1, nil, 2, 1)
	first := explicitCandidate(ids[0], owner, owner, "response-1", usage, 10, 20, 100, "first-model", nil, OperationResponse)
	secondTurn := "second-turn"
	second := explicitCandidate(ids[1], owner, owner, "response-1", usage, 30, 40, 147, "embedded-model", &secondTurn, OperationCompaction)
	batches := []ProcessBatch{
		{SourceState: reconcileSourceState(ids[0], owner, owner), Candidates: []UsageCandidate{first}, LogicalSafeOffset: 20},
		{SourceState: reconcileSourceState(ids[1], owner, owner), Candidates: []UsageCandidate{second}, LogicalSafeOffset: 40},
	}
	results := reconcileRootForTest(t, run, epoch, batches)
	if len(results) != 2 || len(results[0].Events) != 1 || len(results[0].Occurrences) != 2 || len(results[0].Facts) != 1 {
		t.Fatalf("root batch did not merge the shared response: %+v", results)
	}
	event := results[0].Events[0]
	if event.OccurredAtMS != first.OccurredAtMS || event.Model != first.Model || event.TurnKey != nil || results[0].Facts[0].Operation != OperationCompaction {
		t.Fatalf("first staged binding did not retain canonical attribution while promoting operation: event=%+v fact=%+v", event, results[0].Facts[0])
	}
	second.Usage = testUsage(t, 5, 1, nil, 2, 1)
	batches[1].Candidates = []UsageCandidate{second}
	conflict := reconcileRootForTest(t, run, epoch, batches)
	if conflict[0].Fatal == nil || conflict[0].Fatal.Code != FatalCompactionIdentity {
		t.Fatalf("same response with contradictory compaction usage did not fail identity validation: %+v", conflict[0].Fatal)
	}
}

func TestReconcileLegacyCoverageRequiresProvenSourceOrTurn(t *testing.T) {
	t.Run("same source adjacent without turn", func(t *testing.T) {
		owner := "owner"
		run, epoch, ids := setupReconcileStorage(t, []string{owner})
		legacyUsage := testUsage(t, 3, 0, nil, 0, 0)
		before := testUsage(t, 5, 0, nil, 0, 0)
		after := testUsage(t, 8, 0, nil, 0, 0)
		explicit := explicitCandidate(ids[0], owner, owner, "adjacent", legacyUsage, 20, 30, 200, "model", nil, OperationResponse)
		legacy := legacyCandidate(ids[0], owner, owner, 10, 20, before, after, legacyUsage, nil)
		batch := ProcessBatch{SourceState: reconcileSourceState(ids[0], owner, owner), Candidates: []UsageCandidate{legacy, explicit}, LogicalSafeOffset: 30}
		result := reconcileRootForTest(t, run, epoch, []ProcessBatch{batch})[0]
		if len(result.DeleteEventIDs) != 1 || result.DeleteEventIDs[0] != legacy.EventID {
			t.Fatalf("same-source adjacent explicit response did not cover a nil-turn legacy window: %+v", result)
		}
	})

	t.Run("cross source nil turns do not associate", func(t *testing.T) {
		owner := "owner"
		run, epoch, ids := setupReconcileStorage(t, []string{owner, owner})
		usage := testUsage(t, 3, 0, nil, 0, 0)
		legacy := legacyCandidate(ids[0], owner, owner, 10, 20, sharedusage.Zero(), usage, usage, nil)
		explicit := explicitCandidate(ids[1], owner, owner, "unrelated", usage, 100, 110, 200, "model", nil, OperationResponse)
		batches := []ProcessBatch{
			{SourceState: reconcileSourceState(ids[0], owner, owner), Candidates: []UsageCandidate{legacy}, LogicalSafeOffset: 20},
			{SourceState: reconcileSourceState(ids[1], owner, owner), Candidates: []UsageCandidate{explicit}, LogicalSafeOffset: 110},
		}
		result := reconcileRootForTest(t, run, epoch, batches)[0]
		if len(result.DeleteEventIDs) != 0 || len(result.WindowUpserts) != 1 {
			t.Fatalf("cross-source nil-turn evidence was guessed as related: %+v", result)
		}
		window, err := DecodeLegacyReconciliationWindow(result.WindowUpserts[0].StateJSON)
		if err != nil || len(window.ExplicitResponseIDs) != 0 {
			t.Fatalf("unproven explicit response entered the legacy window: %+v err=%v", window, err)
		}
	})

	t.Run("one response cannot cover two logical windows", func(t *testing.T) {
		owner, turn := "owner", "turn"
		run, epoch, ids := setupReconcileStorage(t, []string{owner, owner, owner})
		usage := testUsage(t, 3, 0, nil, 0, 0)
		legacyA := legacyCandidate(ids[0], owner, owner, 10, 20, sharedusage.Zero(), usage, usage, &turn)
		legacyB := legacyCandidate(ids[1], owner, owner, 100, 110, sharedusage.Zero(), usage, usage, &turn)
		explicit := explicitCandidate(ids[2], owner, owner, "shared", usage, 500, 510, 200, "model", &turn, OperationResponse)
		batches := []ProcessBatch{
			{SourceState: reconcileSourceState(ids[0], owner, owner), Candidates: []UsageCandidate{legacyA}, LogicalSafeOffset: 20},
			{SourceState: reconcileSourceState(ids[1], owner, owner), Candidates: []UsageCandidate{legacyB}, LogicalSafeOffset: 110},
			{SourceState: reconcileSourceState(ids[2], owner, owner), Candidates: []UsageCandidate{explicit}, LogicalSafeOffset: 510},
		}
		result := reconcileRootForTest(t, run, epoch, batches)[0]
		if result.Fatal == nil || result.Fatal.Code != FatalLegacyCoverage {
			t.Fatalf("one explicit response was assigned to two logical windows without a fatal conflict: %+v", result)
		}
	})
}

func TestUniqueUsageSubsetsPrunesFullPositiveCoverage(t *testing.T) {
	usage := testUsage(t, 1, 0, nil, 0, 0)
	candidates := make([]UsageCandidate, 48)
	target := sharedusage.Zero()
	for index := range candidates {
		candidates[index] = UsageCandidate{Usage: usage}
		target, _ = target.CheckedAdd(usage)
	}
	matching := uniqueUsageSubsets(candidates, target)
	if len(matching) != 1 || len(matching[0]) != len(candidates) {
		t.Fatalf("unique full positive set was not found: matches=%d", len(matching))
	}
	if multiple := uniqueUsageSubsets([]UsageCandidate{
		{Usage: testUsage(t, 1, 0, nil, 0, 0)},
		{Usage: testUsage(t, 2, 0, nil, 0, 0)},
		{Usage: testUsage(t, 3, 0, nil, 0, 0)},
	}, testUsage(t, 3, 0, nil, 0, 0)); len(multiple) != 2 {
		t.Fatalf("ambiguous subset did not stop after exposing a second solution: %d", len(multiple))
	}
	cacheWrite := int64(0)
	unknownCache := testUsage(t, 1, 0, nil, 0, 0)
	knownCache := testUsage(t, 1, 0, &cacheWrite, 0, 0)
	if matches := uniqueUsageSubsets([]UsageCandidate{{Usage: unknownCache}}, knownCache); len(matches) != 0 {
		t.Fatalf("unknown cache-write knownness was treated as a proven match: %+v", matches)
	}
}

func readTopAndEmbeddedFixture(t *testing.T) [][]byte {
	t.Helper()
	path := filepath.Join("..", "..", "..", "tests", "fixtures", "codex", "compaction", "schema_top_and_embedded.jsonl")
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	var lines [][]byte
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		lines = append(lines, append([]byte(nil), scanner.Bytes()...))
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	if len(lines) != 2 {
		t.Fatalf("expected two fixture records, got %d", len(lines))
	}
	return lines
}

func processFixtureRecord(t *testing.T, sourceFileID int64, owner string, raw []byte, start, end int64) ProcessBatch {
	t.Helper()
	state := processorTestState()
	state.Source.SourceFileID = sourceFileID
	state.Source.OwningThreadID = owner
	state.Source.RootSessionID = owner
	if state.Source.ObservedRawSize < end {
		state.Source.ObservedRawSize = end
	}
	model := "fixture-model"
	state.Source.ActiveModel = &model
	state.Source.ActiveModelOffset = int64Pointer(0)
	record := rollout.Record{SourceFileID: sourceFileID, Generation: 1, LogicalStartOffset: start, LogicalEndOffset: end, JSON: raw}
	owned := OwnedRecord{
		Parsed:            ParseRecord(record),
		Ownership:         rollout.Ownership{Kind: rollout.OwnershipOwning, ThreadID: owner},
		PhysicalEndOffset: end,
	}
	batch, err := ProcessRecords(state, []OwnedRecord{owned}, 40)
	if err != nil {
		t.Fatal(err)
	}
	return batch
}

func TestReconcileTopAndEmbeddedCompactionAcrossBatches(t *testing.T) {
	owner := "01a0adad-044f-74b2-9e99-307a40594f89"
	run, epoch, ids := setupReconcileStorage(t, []string{owner, owner})
	lines := readTopAndEmbeddedFixture(t)
	top := processFixtureRecord(t, ids[0], owner, lines[0], 0, 10)
	compacted := processFixtureRecord(t, ids[1], owner, lines[1], 100, 110)
	if len(top.Candidates) != 1 || len(compacted.Compactions) != 1 {
		t.Fatalf("D processor did not produce the expected fixture evidence: top=%+v compacted=%+v", top, compacted)
	}
	staged := reconcileRootForTest(t, run, epoch, []ProcessBatch{top, compacted})
	if staged[0].Fatal != nil || len(staged[0].Events) != 1 || len(staged[0].Occurrences) != 2 ||
		staged[0].Events[0].OccurredAtMS != top.Candidates[0].OccurredAtMS ||
		staged[0].Events[0].Model != top.Candidates[0].Model || staged[0].Facts[0].Operation != OperationCompaction ||
		len(staged[0].MarkerUpserts) != 1 || staged[0].MarkerUpserts[0].ResolvedEventID == nil ||
		*staged[0].MarkerUpserts[0].ResolvedEventID != top.Candidates[0].EventID {
		t.Fatalf("root staging did not retain first binding and upgrade it through embedded compaction: %+v", staged[0])
	}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		for _, result := range staged {
			if _, err := Commit(tx, source.UsageTargetActive, result, 50); err != nil {
				return err
			}
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	durable := reconcileRootForTest(t, run, epoch, []ProcessBatch{compacted})[0]
	if durable.Fatal != nil || len(durable.Events) != 1 || durable.Events[0].OccurredAtMS != top.Candidates[0].OccurredAtMS ||
		durable.Events[0].Model != top.Candidates[0].Model || len(durable.MarkerUpserts) != 1 || durable.MarkerUpserts[0].ResolvedEventID == nil {
		t.Fatalf("durable duplicate did not preserve first attribution: %+v", durable)
	}
	idOnly := compacted
	idOnly.Candidates = nil
	idOnly.Compactions = append([]PendingUsageEvidence(nil), compacted.Compactions...)
	idOnlyCompaction := *idOnly.Compactions[0].Record.Compaction
	idOnlyCompaction.Latest = nil
	idOnly.Compactions[0].Record.Compaction = &idOnlyCompaction
	resolved := reconcileRootForTest(t, run, epoch, []ProcessBatch{idOnly})[0]
	if resolved.Fatal != nil || len(resolved.MarkerUpserts) != 1 || resolved.MarkerUpserts[0].ResolvedEventID == nil ||
		*resolved.MarkerUpserts[0].ResolvedEventID != top.Candidates[0].EventID || len(resolved.Facts) != 1 || resolved.Facts[0].Operation != OperationCompaction {
		t.Fatalf("ID-only compaction did not resolve through the durable canonical binding: %+v", resolved)
	}
	conflicting := compacted
	conflicting.Compactions = append([]PendingUsageEvidence(nil), compacted.Compactions...)
	badUsage := testUsage(t, 311997, 311040, int64Pointer(0), 4993, 0)
	conflicting.Compactions[0].Record.Compaction.Latest.Usage = validUsage(badUsage)
	bad := reconcileRootForTest(t, run, epoch, []ProcessBatch{conflicting})[0]
	if bad.Fatal == nil || bad.Fatal.Code != FatalCompactionIdentity {
		t.Fatalf("durable binding with contradictory embedded usage did not fail compaction identity: %+v", bad.Fatal)
	}
}

func TestProcessBatchLegacyReplacementCommitsOnlyExplicitCanonicalEvent(t *testing.T) {
	owner := "owner"
	run, epoch, ids := setupReconcileStorage(t, []string{owner})
	state := processorTestState()
	state.Source.SourceFileID = ids[0]
	state.Source.OwningThreadID = owner
	state.Source.RootSessionID = owner
	state.Source.ObservedRawSize = 2500
	state.Source.PreviousTotal = usagePointer(testUsage(t, 50, 0, nil, 0, 0))
	state.Source.PreviousTotalOffset = int64Pointer(900)
	model := "model"
	state.Source.ActiveModel = &model
	state.Source.ActiveModelOffset = int64Pointer(0)
	legacy := tokenCountRecord(testUsage(t, 80, 0, nil, 0, 0), UsageValue{State: UsageValueMissing}, 1000)
	legacy.Parsed.StartOffset, legacy.Parsed.EndOffset = 10, 20
	response := processorRecord(RawResponseUsage, 20, 30, 2000)
	response.Parsed.TimestampMS = int64Pointer(101)
	response.Parsed.Response = &ResponseEvidence{
		ResponseID: "replacement", ThreadID: owner,
		Usage: validUsage(testUsage(t, 30, 0, nil, 0, 0)),
	}
	batch, err := ProcessRecords(state, []OwnedRecord{legacy, response}, 70)
	if err != nil {
		t.Fatal(err)
	}
	if len(batch.Candidates) != 2 || batch.Candidates[0].EvidenceKind != EvidenceLegacy || batch.Candidates[0].EventKind != 1 {
		t.Fatalf("D processor did not produce recovered legacy and explicit evidence: %+v", batch.Candidates)
	}
	result := reconcileRootForTest(t, run, epoch, []ProcessBatch{batch})[0]
	legacyID := batch.Candidates[0].EventID
	explicitID := batch.Candidates[1].EventID
	if result.Fatal != nil || len(result.DeleteEventIDs) != 1 || result.DeleteEventIDs[0] != legacyID ||
		len(result.Events) != 1 || result.Events[0].EventID != explicitID || len(result.Facts) != 1 || result.Facts[0].EventID != explicitID {
		t.Fatalf("same-call coverage patch retained a replaced legacy canonical event or fact: %+v", result)
	}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := Commit(tx, source.UsageTargetActive, result, 70)
		return err
	}); err != nil {
		t.Fatalf("same-call explicit replacement failed v14 SQLite commit: %v", err)
	}
	if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
		var eventCount, occurrenceCount, factCount int
		if err := reader.QueryRow(`SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=?`, epoch).Scan(&eventCount); err != nil {
			return err
		}
		if err := reader.QueryRow(`SELECT count(*) FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?`, epoch).Scan(&occurrenceCount); err != nil {
			return err
		}
		if err := reader.QueryRow(`SELECT count(*) FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=?`, epoch).Scan(&factCount); err != nil {
			return err
		}
		if eventCount != 1 || occurrenceCount != 2 || factCount != 1 {
			t.Fatalf("replacement commit persisted wrong canonical/provenance cardinality: events=%d occurrences=%d facts=%d", eventCount, occurrenceCount, factCount)
		}
		var onlyEvent, onlyOccurrence, onlyFact string
		if err := reader.QueryRow(`SELECT event_id FROM usage_events WHERE source='codex' AND source_epoch=?`, epoch).Scan(&onlyEvent); err != nil {
			return err
		}
		if err := reader.QueryRow(`SELECT event_id FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?`, epoch).Scan(&onlyOccurrence); err != nil {
			return err
		}
		if err := reader.QueryRow(`SELECT event_id FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=?`, epoch).Scan(&onlyFact); err != nil {
			return err
		}
		if onlyEvent != explicitID || onlyOccurrence != explicitID || onlyFact != explicitID {
			t.Fatalf("replacement commit retained the legacy ID: event=%q occurrence=%q fact=%q", onlyEvent, onlyOccurrence, onlyFact)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestProcessBatchLaterExplicitRetargetsDurableLegacyOccurrence(t *testing.T) {
	owner := "owner"
	run, epoch, ids := setupReconcileStorage(t, []string{owner})
	state := processorTestState()
	state.Source.SourceFileID = ids[0]
	state.Source.OwningThreadID = owner
	state.Source.RootSessionID = owner
	state.Source.PreviousTotal = usagePointer(testUsage(t, 50, 0, nil, 0, 0))
	state.Source.PreviousTotalOffset = int64Pointer(0)
	model := "model"
	state.Source.ActiveModel = &model
	state.Source.ActiveModelOffset = int64Pointer(0)
	legacy := tokenCountRecord(testUsage(t, 80, 0, nil, 0, 0), UsageValue{State: UsageValueMissing}, 20)
	legacy.Parsed.StartOffset, legacy.Parsed.EndOffset = 10, 20
	firstBatch, err := ProcessRecords(state, []OwnedRecord{legacy}, 40)
	if err != nil {
		t.Fatal(err)
	}
	legacyID := firstBatch.Candidates[0].EventID
	first := reconcileRootForTest(t, run, epoch, []ProcessBatch{firstBatch})[0]
	if first.Fatal != nil || len(first.Events) != 1 || first.Events[0].EventID != legacyID || len(first.WindowUpserts) != 1 {
		t.Fatalf("D legacy batch did not create its durable proposal window: %+v", first)
	}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := Commit(tx, source.UsageTargetActive, first, 50)
		return err
	}); err != nil {
		t.Fatalf("initial legacy commit failed: %v", err)
	}

	secondState, err := CounterStateFromSourceState(firstBatch.SourceState)
	if err != nil {
		t.Fatal(err)
	}
	response := processorRecord(RawResponseUsage, 20, 30, 30)
	response.Parsed.TimestampMS = int64Pointer(60)
	response.Parsed.Response = &ResponseEvidence{
		ResponseID: "later-explicit", ThreadID: owner,
		Usage: validUsage(testUsage(t, 30, 0, nil, 0, 0)),
	}
	secondBatch, err := ProcessRecords(secondState, []OwnedRecord{response}, 60)
	if err != nil {
		t.Fatal(err)
	}
	second := reconcileRootForTest(t, run, epoch, []ProcessBatch{secondBatch})[0]
	explicitID := ResponseEventID(owner, "later-explicit")
	if second.Fatal != nil || len(second.DeleteEventIDs) != 1 || second.DeleteEventIDs[0] != legacyID ||
		len(second.Events) != 1 || second.Events[0].EventID != explicitID || len(second.Facts) != 1 || second.Facts[0].EventID != explicitID {
		t.Fatalf("durable window did not generate the later explicit replacement patch: %+v", second)
	}
	var retargeted bool
	for _, occurrence := range second.Occurrences {
		if occurrence.SourceFileID == ids[0] && occurrence.Generation == 1 && occurrence.StartOffset == 10 &&
			occurrence.EndOffset == 20 && occurrence.EventID == explicitID {
			retargeted = true
		}
	}
	if !retargeted {
		t.Fatalf("later explicit coverage omitted the legacy logical occurrence retarget: %+v", second.Occurrences)
	}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := Commit(tx, source.UsageTargetActive, second, 60)
		return err
	}); err != nil {
		t.Fatalf("later explicit replacement commit failed: %v", err)
	}
	if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
		var eventID string
		var startOffset, endOffset int64
		if err := reader.QueryRow(`SELECT event_id,source_start_offset,source_end_offset FROM codex_usage_event_occurrences
			WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=1 AND source_start_offset=10`, epoch, ids[0]).
			Scan(&eventID, &startOffset, &endOffset); err != nil {
			return err
		}
		if eventID != explicitID || startOffset != 10 || endOffset != 20 {
			t.Fatalf("durable legacy occurrence was not retargeted in place: event=%q range=%d-%d", eventID, startOffset, endOffset)
		}
		var legacyEvents, legacyFacts, legacyOccurrences, windows int
		if err := reader.QueryRow(`SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=? AND event_id=?`, epoch, legacyID).Scan(&legacyEvents); err != nil {
			return err
		}
		if err := reader.QueryRow(`SELECT count(*) FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=? AND event_id=?`, epoch, legacyID).Scan(&legacyFacts); err != nil {
			return err
		}
		if err := reader.QueryRow(`SELECT count(*) FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=? AND event_id=?`, epoch, legacyID).Scan(&legacyOccurrences); err != nil {
			return err
		}
		if err := reader.QueryRow(`SELECT count(*) FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=? AND source_file_id=?`, epoch, ids[0]).Scan(&windows); err != nil {
			return err
		}
		if legacyEvents != 0 || legacyFacts != 0 || legacyOccurrences != 0 || windows != 0 {
			t.Fatalf("legacy durable rows survived explicit replacement: events=%d facts=%d occurrences=%d windows=%d",
				legacyEvents, legacyFacts, legacyOccurrences, windows)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestRecoveredDeltaResidualReconcilesClosedTurnAcrossBatches(t *testing.T) {
	owner := "owner"
	run, epoch, ids := setupReconcileStorage(t, []string{owner})
	previous := testUsage(t, 50, 0, nil, 0, 0)
	current := testUsage(t, 150, 0, nil, 0, 0)
	model := "model"
	state := processorTestState()
	state.Source.SourceFileID = ids[0]
	state.Source.OwningThreadID = owner
	state.Source.RootSessionID = owner
	state.Source.PreviousTotal = usagePointer(previous)
	state.Source.PreviousTotalOffset = int64Pointer(0)
	state.Source.ActiveModel = &model
	state.Source.ActiveModelOffset = int64Pointer(0)
	start := lifecycleRecord(LifecycleStarted, "turn", 0)
	legacy := tokenCountRecord(current, UsageValue{State: UsageValueMissing}, 20)
	legacy.Parsed.StartOffset, legacy.Parsed.EndOffset = 10, 20
	firstBatch, err := ProcessRecords(state, []OwnedRecord{start, legacy}, 100)
	if err != nil {
		t.Fatal(err)
	}
	if len(firstBatch.Candidates) != 1 || firstBatch.Candidates[0].EventKind != 1 ||
		firstBatch.Candidates[0].Usage.InputTokens != 100 || len(firstBatch.TurnUpserts) != 1 ||
		firstBatch.TurnUpserts[0].Status != TurnOpen {
		t.Fatalf("D did not produce the open recovered-delta turn and proposal: %+v", firstBatch)
	}
	legacyID := firstBatch.Candidates[0].EventID
	first := reconcileRootForTest(t, run, epoch, []ProcessBatch{firstBatch})[0]
	if first.Fatal != nil || len(first.Events) != 1 || first.Events[0].EventID != legacyID || len(first.WindowUpserts) != 1 {
		t.Fatalf("initial recovered-delta reconciliation failed: %+v", first)
	}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := Commit(tx, source.UsageTargetActive, first, 100)
		return err
	}); err != nil {
		t.Fatalf("initial recovered-delta commit failed: %v", err)
	}

	var secondState CounterState
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		loaded, found, err := LoadCounterState(tx, source.UsageTargetActive, ids[0], 1)
		if err != nil {
			return err
		}
		if !found || loaded.OpenTurn == nil {
			t.Fatal("committed open turn did not reload")
		}
		secondState = loaded
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	response := processorRecord(RawResponseUsage, 20, 30, 30)
	response.Parsed.TimestampMS = int64Pointer(200)
	response.Parsed.Response = &ResponseEvidence{
		ResponseID: "residual-response", ThreadID: owner,
		Usage: validUsage(testUsage(t, 40, 0, nil, 0, 0)),
	}
	complete := lifecycleRecord(LifecycleCompleted, "turn", 30)
	secondBatch, err := ProcessRecords(secondState, []OwnedRecord{response, complete}, 200)
	if err != nil {
		t.Fatal(err)
	}
	if len(secondBatch.Candidates) != 1 || secondBatch.Candidates[0].EvidenceKind != EvidenceExplicit ||
		len(secondBatch.TurnUpserts) != 1 || secondBatch.TurnUpserts[0].Status != TurnCompleted ||
		secondBatch.TurnUpserts[0].Accounted.InputTokens != 140 {
		t.Fatalf("D did not produce the later explicit and closed turn input: %+v", secondBatch)
	}

	second := reconcileRootForTest(t, run, epoch, []ProcessBatch{secondBatch})[0]
	explicitID := ResponseEventID(owner, "residual-response")
	residual := testUsage(t, 60, 0, nil, 0, 0)
	residualID := LegacyEventID(owner, firstBatch.Candidates[0].TurnKey, 1,
		firstBatch.Candidates[0].OccurredAtMS, &previous, current, residual,
		firstBatch.Candidates[0].Model, firstBatch.Candidates[0].ReasoningEffort)
	if second.Fatal != nil || len(second.DeleteEventIDs) != 1 || second.DeleteEventIDs[0] != legacyID ||
		len(second.Events) != 2 || len(second.Facts) != 2 {
		t.Fatalf("later explicit did not produce the residual replacement patch: %+v", second)
	}
	var sawExplicit, sawResidual, sawCompensation bool
	for _, event := range second.Events {
		switch event.EventID {
		case explicitID:
			sawExplicit = event.Usage.InputTokens == 40
		case residualID:
			sawResidual = event.Kind == sharedusage.EventKindRecovered && event.Usage.InputTokens == 60
		}
		if event.Kind == sharedusage.EventKindTurnCompensation {
			sawCompensation = true
		}
	}
	if !sawExplicit || !sawResidual || sawCompensation {
		t.Fatalf("canonical events or compensation do not reflect the 40+60 turn total: %+v", second.Events)
	}
	if len(second.TurnUpserts) != 1 || second.TurnUpserts[0].Accounted.InputTokens != 100 ||
		second.TurnUpserts[0].AccountedCandidateCount != 2 {
		t.Fatalf("closed turn accounted did not overlay the final residual patch: %+v", second.TurnUpserts)
	}
	var window LegacyReconciliationWindow
	if len(second.WindowUpserts) != 1 {
		t.Fatalf("residual window was not retained: %+v", second.WindowUpserts)
	}
	window, err = DecodeLegacyReconciliationWindow(second.WindowUpserts[0].StateJSON)
	if err != nil || len(window.ProposalEventIDs) != 1 || window.ProposalEventIDs[0] != residualID ||
		len(window.LegacyCoveredResponseIDs) != 1 || window.LegacyCoveredResponseIDs[0] != "residual-response" {
		t.Fatalf("residual proposal/coverage was not retained in the durable window: %+v err=%v", window, err)
	}
	var retargeted bool
	for _, occurrence := range second.Occurrences {
		if occurrence.SourceFileID == ids[0] && occurrence.Generation == 1 && occurrence.StartOffset == 10 &&
			occurrence.EndOffset == 20 && occurrence.EventID == residualID {
			retargeted = true
		}
	}
	if !retargeted {
		t.Fatalf("residual replacement did not preserve the legacy logical occurrence: %+v", second.Occurrences)
	}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := Commit(tx, source.UsageTargetActive, second, 200)
		return err
	}); err != nil {
		t.Fatalf("residual replacement commit failed: %v", err)
	}
	if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
		var count, total int64
		if err := reader.QueryRow(`SELECT count(*),coalesce(sum(input_tokens),0) FROM usage_events
			WHERE source='codex' AND source_epoch=?`, epoch).Scan(&count, &total); err != nil {
			return err
		}
		if count != 2 || total != 100 {
			t.Fatalf("durable canonical usage is not exactly explicit 40 plus residual 60: count=%d total=%d", count, total)
		}
		var legacyCount, compensationCount int
		if err := reader.QueryRow(`SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=? AND event_id=?`, epoch, legacyID).Scan(&legacyCount); err != nil {
			return err
		}
		if err := reader.QueryRow(`SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=? AND event_kind='turn_compensation'`, epoch).Scan(&compensationCount); err != nil {
			return err
		}
		if legacyCount != 0 || compensationCount != 0 {
			t.Fatalf("replaced legacy or spurious compensation survived: legacy=%d compensation=%d", legacyCount, compensationCount)
		}
		var accountedInput, candidateCount int64
		var status string
		if err := reader.QueryRow(`SELECT accounted_input_tokens,accounted_candidate_count,status FROM codex_turns
			WHERE ledger_epoch=? AND source_file_id=? AND file_generation=1`, epoch, ids[0]).Scan(&accountedInput, &candidateCount, &status); err != nil {
			return err
		}
		if accountedInput != 100 || candidateCount != 2 || status != string(TurnCompleted) {
			t.Fatalf("closed turn persisted wrong accounted total: input=%d candidates=%d status=%q", accountedInput, candidateCount, status)
		}
		var occurrenceEvent string
		if err := reader.QueryRow(`SELECT event_id FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?
			AND source_file_id=? AND file_generation=1 AND source_start_offset=10`, epoch, ids[0]).Scan(&occurrenceEvent); err != nil {
			return err
		}
		if occurrenceEvent != residualID {
			t.Fatalf("legacy logical occurrence did not retarget to residual: %q", occurrenceEvent)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestProcessBatchLogicalWindowCollectsExplicitSet(t *testing.T) {
	for _, name := range []string{"same call", "durable explicit prefix", "after boundary new call", "nonadjacent last requires set coverage", "multiple exact sets"} {
		t.Run(name, func(t *testing.T) {
			owner := "owner"
			run, epoch, ids := setupReconcileStorage(t, []string{owner})
			state := processorTestState()
			state.Source.SourceFileID = ids[0]
			state.Source.PreviousTotal = usagePointer(testUsage(t, 0, 0, nil, 0, 0))
			state.Source.PreviousTotalOffset = int64Pointer(900)
			state.Source.ObservedRawSize = 2000
			state.Source.ActiveModel = stringPointer("model")
			state.Source.ActiveModelOffset = int64Pointer(0)
			state.Carry.OpenWindowStartOffset = uint64Pointer(0)
			response := func(id string, amount, start int64) OwnedRecord {
				record := processorRecord(RawResponseUsage, start, start+10, 1000+start+10)
				record.Parsed.TimestampMS = int64Pointer(100 + start)
				record.Parsed.Response = &ResponseEvidence{ResponseID: id, ThreadID: owner,
					Usage: validUsage(testUsage(t, amount, 0, nil, 0, 0))}
				return record
			}
			records := []OwnedRecord{response("forty", 40, 0), response("sixty", 60, 10)}
			tokenStart := int64(20)
			if name == "multiple exact sets" {
				records = append(records, response("hundred", 100, 20))
				tokenStart = 30
			}
			legacy := tokenCountRecord(testUsage(t, 100, 0, nil, 0, 0), UsageValue{State: UsageValueMissing}, 1000+tokenStart+10)
			legacy.Parsed.StartOffset, legacy.Parsed.EndOffset = tokenStart, tokenStart+10
			if name == "nonadjacent last requires set coverage" {
				legacy.Parsed.Last = validUsage(testUsage(t, 40, 0, nil, 0, 0))
			}
			if name == "durable explicit prefix" {
				prefix, err := ProcessRecords(state, records, 100)
				if err != nil {
					t.Fatal(err)
				}
				prefixResult := reconcileRootForTest(t, run, epoch, []ProcessBatch{prefix})[0]
				if err := run.Storage().Write(func(tx *source.WriteTx) error {
					_, err := Commit(tx, source.UsageTargetActive, prefixResult, 100)
					return err
				}); err != nil {
					t.Fatal(err)
				}
				state, err = CounterStateFromSourceState(prefixResult.SourceState)
				if err != nil {
					t.Fatal(err)
				}
				records = nil
			}
			records = append(records, legacy)
			if name == "after boundary new call" {
				records = append(records, response("next-call", 100, 30))
			}
			batch, err := ProcessRecords(state, records, 200)
			if err != nil {
				t.Fatal(err)
			}
			result := reconcileRootForTest(t, run, epoch, []ProcessBatch{batch})[0]
			if name == "multiple exact sets" {
				if result.Fatal == nil || result.Fatal.Code != FatalLegacyCoverage {
					t.Fatalf("logical window accepted two exact explicit sets: %+v", result)
				}
				return
			}
			if result.Fatal != nil {
				t.Fatalf("known logical window did not reconcile its explicit set: %+v", result.Fatal)
			}
			if err := run.Storage().Write(func(tx *source.WriteTx) error {
				_, err := Commit(tx, source.UsageTargetActive, result, 200)
				return err
			}); err != nil {
				t.Fatal(err)
			}
			if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
				var total, count, legacyCount int64
				if err := reader.QueryRow(`SELECT coalesce(sum(input_tokens),0),count(*),
					coalesce(sum(event_kind='recovered'),0) FROM usage_events WHERE source='codex' AND source_epoch=?`, epoch).
					Scan(&total, &count, &legacyCount); err != nil {
					return err
				}
				wantTotal, wantCount := int64(100), int64(2)
				if name == "after boundary new call" {
					wantTotal, wantCount = 200, 3
				}
				if total != wantTotal || count != wantCount || legacyCount != 0 {
					t.Fatalf("logical window double counted legacy: total=%d events=%d legacy=%d", total, count, legacyCount)
				}
				if name == "nonadjacent last requires set coverage" {
					var proposalOccurrences int
					if err := reader.QueryRow(`SELECT count(*) FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?
						AND source_file_id=? AND source_start_offset=20`, epoch, ids[0]).Scan(&proposalOccurrences); err != nil {
						return err
					}
					if proposalOccurrences != 0 {
						t.Fatal("nonadjacent last match guessed a single-event retarget for two-event coverage")
					}
				}
				var rawCarry string
				if err := reader.QueryRow(`SELECT reconciliation_state_json FROM codex_usage_source_states
					WHERE ledger_epoch=? AND source_file_id=?`, epoch, ids[0]).Scan(&rawCarry); err != nil {
					return err
				}
				carry, err := DecodeReconciliationCarryJSON([]byte(rawCarry))
				if err != nil || carry.OpenWindowStartOffset == nil || *carry.OpenWindowStartOffset != uint64(tokenStart+10) {
					t.Fatalf("root first-source carry did not retain its logical boundary: %+v err=%v", carry, err)
				}
				return nil
			}); err != nil {
				t.Fatal(err)
			}
			if name == "same call" {
				nextState, err := CounterStateFromSourceState(result.SourceState)
				if err != nil {
					t.Fatal(err)
				}
				nextLegacy := tokenCountRecord(testUsage(t, 150, 0, nil, 0, 0), UsageValue{State: UsageValueMissing}, 1060)
				nextLegacy.Parsed.StartOffset, nextLegacy.Parsed.EndOffset = 50, 60
				nextBatch, err := ProcessRecords(nextState, []OwnedRecord{response("twenty", 20, 30), response("thirty", 30, 40), nextLegacy}, 300)
				if err != nil {
					t.Fatal(err)
				}
				next := reconcileRootForTest(t, run, epoch, []ProcessBatch{nextBatch})[0]
				if next.Fatal != nil {
					t.Fatalf("next logical window reused the prior window's evidence: %+v", next.Fatal)
				}
				if err := run.Storage().Write(func(tx *source.WriteTx) error {
					_, err := Commit(tx, source.UsageTargetActive, next, 300)
					return err
				}); err != nil {
					t.Fatal(err)
				}
				if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
					var total, count int64
					if err := reader.QueryRow(`SELECT sum(input_tokens),count(*) FROM usage_events
						WHERE source='codex' AND source_epoch=?`, epoch).Scan(&total, &count); err != nil {
						return err
					}
					carry, err := DecodeReconciliationCarryJSON(next.SourceState.ReconciliationStateJSON)
					if err != nil || total != 150 || count != 4 || carry.OpenWindowStartOffset == nil || *carry.OpenWindowStartOffset != 60 {
						t.Fatalf("next logical window did not advance from 30 to 60: total=%d events=%d carry=%+v err=%v", total, count, carry, err)
					}
					return nil
				}); err != nil {
					t.Fatal(err)
				}
			}
		})
	}
}

func TestProcessBatchLogicalWindowPreservesProposalOccurrencePK(t *testing.T) {
	for _, amount := range []int64{100, 40} {
		t.Run(map[int64]string{100: "full replacement", 40: "residual"}[amount], func(t *testing.T) {
			owner := "owner"
			run, epoch, ids := setupReconcileStorage(t, []string{owner})
			state := processorTestState()
			state.Source.SourceFileID = ids[0]
			state.Source.PreviousTotal = usagePointer(testUsage(t, 0, 0, nil, 0, 0))
			state.Source.PreviousTotalOffset = int64Pointer(900)
			state.Source.ObservedRawSize = 2000
			state.Source.ActiveModel = stringPointer("model")
			state.Source.ActiveModelOffset = int64Pointer(0)
			state.Carry.OpenWindowStartOffset = uint64Pointer(0)
			legacy := tokenCountRecord(testUsage(t, 100, 0, nil, 0, 0), UsageValue{State: UsageValueMissing}, 1030)
			legacy.Parsed.StartOffset, legacy.Parsed.EndOffset = 20, 30
			firstBatch, err := ProcessRecords(state, []OwnedRecord{legacy}, 100)
			if err != nil {
				t.Fatal(err)
			}
			first := reconcileRootForTest(t, run, epoch, []ProcessBatch{firstBatch})[0]
			if first.Fatal != nil || len(first.WindowUpserts) != 1 || first.WindowUpserts[0].StartOffset != 0 ||
				first.WindowUpserts[0].EndOffset != 30 || len(first.Occurrences) != 1 || first.Occurrences[0].StartOffset != 20 {
				t.Fatalf("logical window range was conflated with the token_count occurrence: %+v", first)
			}
			if err := run.Storage().Write(func(tx *source.WriteTx) error {
				_, err := Commit(tx, source.UsageTargetActive, first, 100)
				return err
			}); err != nil {
				t.Fatal(err)
			}
			secondState, err := CounterStateFromSourceState(first.SourceState)
			if err != nil {
				t.Fatal(err)
			}
			response := processorRecord(RawResponseUsage, 30, 40, 1040)
			response.Parsed.TimestampMS = int64Pointer(200)
			response.Parsed.Response = &ResponseEvidence{ResponseID: "later-response", ThreadID: owner,
				Usage: validUsage(testUsage(t, amount, 0, nil, 0, 0))}
			secondBatch, err := ProcessRecords(secondState, []OwnedRecord{response}, 200)
			if err != nil {
				t.Fatal(err)
			}
			second := reconcileRootForTest(t, run, epoch, []ProcessBatch{secondBatch})[0]
			if second.Fatal != nil || len(second.DeleteEventIDs) != 1 || second.DeleteEventIDs[0] != firstBatch.Candidates[0].EventID {
				t.Fatalf("late explicit did not reconcile the durable logical window: %+v", second)
			}
			wantOccurrenceID := ResponseEventID(owner, "later-response")
			if amount == 40 {
				proposal := firstBatch.Candidates[0]
				wantOccurrenceID = LegacyEventID(owner, proposal.TurnKey, 1, proposal.OccurredAtMS,
					proposal.PreviousTotal, *proposal.CurrentTotal, testUsage(t, 60, 0, nil, 0, 0), proposal.Model, proposal.ReasoningEffort)
			}
			if err := run.Storage().Write(func(tx *source.WriteTx) error {
				_, err := Commit(tx, source.UsageTargetActive, second, 200)
				return err
			}); err != nil {
				t.Fatal(err)
			}
			if err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
				var eventID string
				var start, end int64
				if err := reader.QueryRow(`SELECT event_id,source_start_offset,source_end_offset FROM codex_usage_event_occurrences
					WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=1 AND source_start_offset=20`, epoch, ids[0]).
					Scan(&eventID, &start, &end); err != nil {
					return err
				}
				var total, inventedPKs int64
				if err := reader.QueryRow(`SELECT sum(input_tokens) FROM usage_events WHERE source='codex' AND source_epoch=?`, epoch).Scan(&total); err != nil {
					return err
				}
				if err := reader.QueryRow(`SELECT count(*) FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?
					AND source_file_id=? AND source_start_offset=0`, epoch, ids[0]).Scan(&inventedPKs); err != nil {
					return err
				}
				if eventID != wantOccurrenceID || start != 20 || end != 30 || total != 100 || inventedPKs != 0 {
					t.Fatalf("proposal provenance moved to window start or usage doubled: event=%q range=%d-%d total=%d inventedPKs=%d", eventID, start, end, total, inventedPKs)
				}
				return nil
			}); err != nil {
				t.Fatal(err)
			}
		})
	}
}
