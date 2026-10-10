package usage

import (
	"math"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/codex/rollout"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

func TestCounterSubagentBootstrapOnlySeedsTrustedTotal(t *testing.T) {
	state := processorTestState()
	state.Source.OwningThreadID = "agent"
	state.Source.RootSessionID = "root"
	state.Source.ActiveModel = nil
	state.Source.ActiveModelOffset = nil
	state.Source.PreviousTotal = usagePointer(testUsage(t, 2, 0, nil, 0, 0))
	state.Source.PreviousTotalOffset = int64Pointer(4)

	invalid := processorRecord(RawTokenCount, 10, 20, 10)
	invalid.Ownership.ThreadID = "agent"
	invalid.Parsed.HasTokenInfo = true
	invalid.Parsed.Total = UsageValue{State: UsageValueInvalid}
	batch, err := ProcessRecords(state, []OwnedRecord{invalid}, 50)
	if err != nil {
		t.Fatal(err)
	}
	if batch.SourceState.ChainState != ChainContinuous || batch.SourceState.PreviousTotal.InputTokens != 2 || len(batch.Candidates) != 0 {
		t.Fatalf("invalid bootstrap changed the chain or produced usage: state=%+v candidates=%+v", batch.SourceState, batch.Candidates)
	}

	bootstrap := processorRecord(RawTokenCount, 20, 30, 15)
	bootstrap.Ownership.ThreadID = "agent"
	bootstrap.Parsed.HasTokenInfo = true
	bootstrap.Parsed.Total = validUsage(testUsage(t, 10, 1, nil, 2, 1))
	bootstrap.Parsed.Last = validUsage(testUsage(t, 8, 1, nil, 1, 1))
	context := processorRecord(RawTurnContext, 30, 40, 20)
	context.Ownership.ThreadID = "agent"
	context.Parsed.Model = "model"
	next := processorRecord(RawTokenCount, 40, 50, 25)
	next.Ownership.ThreadID = "agent"
	next.Parsed.HasTokenInfo = true
	next.Parsed.TimestampMS = int64Pointer(100)
	next.Parsed.Total = validUsage(testUsage(t, 12, 1, nil, 3, 1))
	next.Parsed.Last = UsageValue{State: UsageValueMissing}
	batch, err = ProcessRecords(state, []OwnedRecord{bootstrap, context, next}, 60)
	if err != nil {
		t.Fatal(err)
	}
	if len(batch.Candidates) != 1 || batch.Candidates[0].EventKind != 1 || batch.Candidates[0].Model != "model" ||
		batch.Candidates[0].Usage.InputTokens != 2 {
		t.Fatalf("bootstrap telemetry was counted or later delta was lost: %+v", batch.Candidates)
	}
	if batch.SourceState.PreviousTotalOffset == nil || *batch.SourceState.PreviousTotalOffset != 25 {
		t.Fatalf("trusted baseline did not retain the physical end: %+v", batch.SourceState.PreviousTotalOffset)
	}
}

func TestCounterNoSnapshotAndInvalidTotalPreserveBaseline(t *testing.T) {
	state := processorTestState()
	previous := testUsage(t, 8, 1, nil, 2, 1)
	state.Source.PreviousTotal = usagePointer(previous)
	state.Source.PreviousTotalOffset = int64Pointer(12)
	state.Source.ResolvedThroughOffset = 20
	state.Source.ObservedRawSize = 100
	state.Source.ChainState = ChainInterrupted
	reason := ChainBlockMalformed
	state.Source.ChainBlockReason = &reason

	noSnapshot := processorRecord(RawTokenCount, 20, 30, 30)
	noSnapshot.Parsed.HasTokenInfo = false
	invalid := processorRecord(RawTokenCount, 30, 40, 40)
	invalid.Parsed.HasTokenInfo = true
	invalid.Parsed.Total = UsageValue{State: UsageValueInvalid}
	batch, err := ProcessRecords(state, []OwnedRecord{noSnapshot, invalid}, 70)
	if err != nil {
		t.Fatal(err)
	}
	got := batch.SourceState
	if got.PreviousTotal == nil || got.PreviousTotal.InputTokens != previous.InputTokens || got.PreviousTotalOffset == nil ||
		*got.PreviousTotalOffset != 12 || got.ChainState != ChainInterrupted || got.ChainBlockReason == nil ||
		*got.ChainBlockReason != ChainBlockTotalInvalid || len(batch.Candidates) != 0 {
		t.Fatalf("no-snapshot/invalid-total handling lost trusted state: %+v", got)
	}

	state.Source.ChainState = ChainContinuous
	state.Source.ChainBlockReason = nil
	noSnapshotBatch, err := ProcessRecords(state, []OwnedRecord{noSnapshot}, 71)
	if err != nil {
		t.Fatal(err)
	}
	if noSnapshotBatch.SourceState.ChainState != ChainContinuous || noSnapshotBatch.SourceState.PreviousTotal == nil ||
		noSnapshotBatch.SourceState.PreviousTotal.InputTokens != previous.InputTokens || len(noSnapshotBatch.Candidates) != 0 {
		t.Fatalf("missing snapshot invented a counter gap or changed baseline: %+v", noSnapshotBatch)
	}
}

func TestCounterNormalRecoveredResetAndDuplicateAccounting(t *testing.T) {
	t.Run("normal", func(t *testing.T) {
		state := processorTestStateWithTurn()
		record := tokenCountRecord(testUsage(t, 10, 2, nil, 4, 1), validUsage(testUsage(t, 3, 1, nil, 2, 1)), 5)
		batch, err := ProcessRecords(state, []OwnedRecord{record}, 80)
		if err != nil {
			t.Fatal(err)
		}
		assertSingleLegacyAccounting(t, batch, 0, 3)
	})

	t.Run("recovered", func(t *testing.T) {
		state := processorTestStateWithTurn()
		state.Source.PreviousTotal = usagePointer(testUsage(t, 5, 1, nil, 2, 1))
		state.Source.PreviousTotalOffset = int64Pointer(5)
		record := tokenCountRecord(testUsage(t, 8, 2, nil, 3, 1), UsageValue{State: UsageValueMissing}, 10)
		batch, err := ProcessRecords(state, []OwnedRecord{record}, 81)
		if err != nil {
			t.Fatal(err)
		}
		assertSingleLegacyAccounting(t, batch, 1, 3)
	})

	t.Run("reset", func(t *testing.T) {
		state := processorTestStateWithTurn()
		state.Source.PreviousTotal = usagePointer(testUsage(t, 10, 2, nil, 4, 1))
		state.Source.PreviousTotalOffset = int64Pointer(5)
		record := tokenCountRecord(testUsage(t, 5, 1, nil, 2, 1), validUsage(testUsage(t, 2, 0, nil, 1, 1)), 10)
		batch, err := ProcessRecords(state, []OwnedRecord{record}, 82)
		if err != nil {
			t.Fatal(err)
		}
		assertSingleLegacyAccounting(t, batch, 0, 2)
		if len(batch.TurnUpserts) != 1 || !batch.TurnUpserts[0].Blocks.Reset {
			t.Fatalf("counter reset blocker missing: %+v", batch.TurnUpserts)
		}
	})

	t.Run("cache-write decrease", func(t *testing.T) {
		state := processorTestStateWithTurn()
		state.Source.PreviousTotal = usagePointer(testUsage(t, 10, 2, int64Pointer(2), 4, 1))
		state.Source.PreviousTotalOffset = int64Pointer(5)
		record := tokenCountRecord(
			testUsage(t, 10, 2, int64Pointer(1), 4, 1),
			validUsage(testUsage(t, 1, 0, nil, 0, 0)), 10,
		)
		batch, err := ProcessRecords(state, []OwnedRecord{record}, 821)
		if err != nil {
			t.Fatal(err)
		}
		if len(batch.Candidates) != 1 || batch.Fatal != nil || len(batch.TurnUpserts) != 1 || !batch.TurnUpserts[0].Blocks.Reset {
			t.Fatalf("cache-write decrease did not produce reset accounting: %+v", batch)
		}
	})

	t.Run("invalid last usage", func(t *testing.T) {
		state := processorTestStateWithTurn()
		state.Source.PreviousTotal = usagePointer(testUsage(t, 5, 1, nil, 2, 1))
		state.Source.PreviousTotalOffset = int64Pointer(5)
		record := tokenCountRecord(testUsage(t, 8, 1, nil, 3, 1), UsageValue{State: UsageValueInvalid}, 10)
		batch, err := ProcessRecords(state, []OwnedRecord{record}, 822)
		if err != nil {
			t.Fatal(err)
		}
		if len(batch.Candidates) != 0 || batch.SourceState.PreviousTotal == nil || batch.SourceState.PreviousTotal.InputTokens != 8 {
			t.Fatalf("invalid last usage produced a candidate or lost the new total baseline: %+v", batch)
		}
	})

	t.Run("duplicate", func(t *testing.T) {
		state := processorTestStateWithTurn()
		current := testUsage(t, 10, 2, nil, 4, 1)
		state.Source.PreviousTotal = usagePointer(current)
		state.Source.PreviousTotalOffset = int64Pointer(5)
		record := tokenCountRecord(current, validUsage(testUsage(t, 3, 1, nil, 2, 1)), 10)
		batch, err := ProcessRecords(state, []OwnedRecord{record}, 83)
		if err != nil {
			t.Fatal(err)
		}
		if len(batch.Candidates) != 0 || len(batch.TurnUpserts) != 1 || batch.TurnUpserts[0].AccountedCandidateCount != 0 ||
			batch.SourceState.PreviousTotalOffset == nil || *batch.SourceState.PreviousTotalOffset != 10 {
			t.Fatalf("duplicate cumulative total changed accounting or failed to advance its baseline: %+v", batch)
		}
	})
}

func TestCounterAccountingDefersLegacyOverflowToReconciliation(t *testing.T) {
	state := processorTestStateWithTurn()
	state.OpenTurn.Accounted = testUsage(t, math.MaxInt64-1, 0, nil, 0, 0)
	state.OpenTurn.AccountedCandidateCount = 1
	record := tokenCountRecord(testUsage(t, math.MaxInt64, 0, nil, 0, 0), validUsage(testUsage(t, 2, 0, nil, 0, 0)), 10)
	batch, err := ProcessRecords(state, []OwnedRecord{record}, 84)
	if err != nil {
		t.Fatal(err)
	}
	if batch.Fatal != nil || len(batch.Candidates) != 1 || len(batch.TurnUpserts) != 1 ||
		batch.TurnUpserts[0].Accounted.InputTokens != math.MaxInt64-1 || batch.TurnUpserts[0].AccountedCandidateCount != 1 {
		t.Fatalf("legacy overflow was treated as canonical or wrapped the turn: batch=%+v", batch)
	}
}

func TestCounterExplicitAccountingOverflowIsFatal(t *testing.T) {
	state := processorTestStateWithTurn()
	state.OpenTurn.RawTurnID = stringPointer("turn")
	state.Source.ActiveModel = stringPointer("model")
	inputs := []int64{math.MaxInt64 - 1, 1, 1}
	records := make([]OwnedRecord, 0, len(inputs))
	for index, input := range inputs {
		record := processorRecord(RawResponseUsage, int64(10+index*10), int64(20+index*10), int64(20+index*10))
		record.Parsed.TimestampMS = int64Pointer(int64(100 + index))
		record.Parsed.Response = &ResponseEvidence{
			ResponseID: string(rune('a' + index)), TurnID: "turn",
			Usage: validUsage(testUsage(t, input, 0, nil, 0, 0)),
		}
		records = append(records, record)
	}
	batch, err := ProcessRecords(state, records, 841)
	if err != nil {
		t.Fatal(err)
	}
	if batch.Fatal == nil || batch.Fatal.Code != FatalArithmeticOverflow || len(batch.Candidates) != 3 || len(batch.TurnUpserts) != 1 {
		t.Fatalf("explicit candidate overflow was not fatal: %+v", batch)
	}
	if batch.TurnUpserts[0].Accounted.InputTokens != math.MaxInt64 || batch.TurnUpserts[0].AccountedCandidateCount != 2 {
		t.Fatalf("overflow wrapped or advanced accounted state: %+v", batch.TurnUpserts[0])
	}
}

func TestCounterUnknownRootStopsBeforeSemanticRecord(t *testing.T) {
	state := processorTestStateWithTurn()
	state.Source.RootSessionID = ""
	record := tokenCountRecord(testUsage(t, 10, 1, nil, 2, 1), validUsage(testUsage(t, 3, 1, nil, 1, 1)), 10)
	batch, err := ProcessRecords(state, []OwnedRecord{record}, 85)
	if err != nil {
		t.Fatal(err)
	}
	if batch.StopBeforeOffset == nil || *batch.StopBeforeOffset != record.Parsed.StartOffset || batch.LogicalSafeOffset != record.Parsed.StartOffset ||
		!batch.UnresolvedBoundary || len(batch.Candidates) != 0 || len(batch.TurnUpserts) != 0 ||
		batch.SourceState.ResolvedThroughOffset != state.Source.ResolvedThroughOffset ||
		batch.SourceState.ContinuationState != ContinuationOwningLive {
		t.Fatalf("unresolved root crossed the semantic boundary: %+v", batch)
	}
}

func TestCounterReplayAndUnknownOwnershipBoundaries(t *testing.T) {
	t.Run("replayed ancestor", func(t *testing.T) {
		state := processorTestState()
		record := tokenCountRecord(testUsage(t, 10, 1, nil, 2, 1), validUsage(testUsage(t, 3, 1, nil, 1, 1)), 10)
		record.Ownership = rollout.Ownership{Kind: rollout.OwnershipReplayedAncestor, ThreadID: "ancestor"}
		batch, err := ProcessRecords(state, []OwnedRecord{record}, 86)
		if err != nil {
			t.Fatal(err)
		}
		if len(batch.Candidates) != 0 || batch.SourceState.ContinuationState != ContinuationReplayedAncestor ||
			batch.SourceState.ResolvedThroughOffset != 10 {
			t.Fatalf("replayed ancestor was not a usage no-op with durable phase: %+v", batch)
		}
	})

	t.Run("unknown ownership", func(t *testing.T) {
		state := processorTestStateWithTurn()
		record := tokenCountRecord(testUsage(t, 10, 1, nil, 2, 1), validUsage(testUsage(t, 3, 1, nil, 1, 1)), 10)
		record.Ownership = rollout.Ownership{Kind: rollout.OwnershipUnknown}
		batch, err := ProcessRecords(state, []OwnedRecord{record}, 87)
		if err != nil {
			t.Fatal(err)
		}
		if batch.Fatal != nil || batch.StopBeforeOffset == nil || *batch.StopBeforeOffset != record.Parsed.StartOffset ||
			batch.SourceState.ResolvedThroughOffset != state.Source.ResolvedThroughOffset || len(batch.Candidates) != 0 ||
			len(batch.TurnUpserts) != 1 || !batch.TurnUpserts[0].Blocks.OwnershipGap {
			t.Fatalf("unknown ownership was fatal or crossed its boundary: %+v", batch)
		}
	})

	t.Run("phase returns to owning live", func(t *testing.T) {
		state := processorTestState()
		replayed := processorRecord(RawIgnored, 10, 20, 20)
		replayed.Ownership = rollout.Ownership{Kind: rollout.OwnershipReplayedAncestor, ThreadID: "ancestor"}
		owning := processorRecord(RawTurnContext, 20, 30, 30)
		owning.Parsed.Model = "model"
		batch, err := ProcessRecords(state, []OwnedRecord{replayed, owning}, 871)
		if err != nil {
			t.Fatal(err)
		}
		if batch.SourceState.ContinuationState != ContinuationOwningLive || batch.SourceState.ActiveModel == nil || *batch.SourceState.ActiveModel != "model" {
			t.Fatalf("continuation phase did not follow the final owning record: %+v", batch.SourceState)
		}
	})
}

func TestCounterCompactionProducesEvidenceWithoutPairing(t *testing.T) {
	state := processorTestState()
	state.Source.ActiveModel = stringPointer("model")
	record := processorRecord(RawCompacted, 10, 20, 20)
	record.Parsed.TimestampMS = int64Pointer(100)
	record.Parsed.Compaction = &CompactionEvidence{
		ResponseID: "compact-id",
		Latest:     &ResponseEvidence{ResponseID: "response-id", Usage: validUsage(testUsage(t, 5, 1, nil, 2, 1))},
	}
	batch, err := ProcessRecords(state, []OwnedRecord{record}, 872)
	if err != nil {
		t.Fatal(err)
	}
	carry, err := LoadReconciliationCarry(batch.SourceState.ReconciliationStateJSON)
	if err != nil {
		t.Fatal(err)
	}
	if len(batch.Compactions) != 1 || len(batch.Candidates) != 1 || batch.Candidates[0].Operation != OperationCompaction ||
		len(batch.Occurrences) != 1 || batch.Fatal != nil || len(carry.PendingEvidence) != 0 {
		t.Fatalf("compaction evidence was paired or discarded in D: %+v", batch)
	}
}

func TestTurnExplicitDuplicateAccountsOnceAndLifecycleOrderIsStable(t *testing.T) {
	t.Run("explicit duplicate", func(t *testing.T) {
		state := processorTestStateWithTurn()
		state.OpenTurn.RawTurnID = stringPointer("turn")
		state.Source.ActiveModel = stringPointer("model")
		usage := testUsage(t, 5, 1, nil, 2, 1)
		response := &ResponseEvidence{ResponseID: "response", TurnID: "turn", Usage: validUsage(usage), ThreadTokenUsage: UsageValue{State: UsageValueMissing}}
		first := processorRecord(RawResponseUsage, 10, 20, 20)
		first.Parsed.TimestampMS = int64Pointer(100)
		first.Parsed.Response = response
		second := processorRecord(RawResponseUsage, 20, 30, 30)
		second.Parsed.TimestampMS = int64Pointer(101)
		second.Parsed.Response = response
		batch, err := ProcessRecords(state, []OwnedRecord{first, second}, 88)
		if err != nil {
			t.Fatal(err)
		}
		if len(batch.Candidates) != 1 || len(batch.Occurrences) != 2 || batch.Fatal != nil || len(batch.TurnUpserts) != 1 ||
			batch.TurnUpserts[0].AccountedCandidateCount != 1 || batch.TurnUpserts[0].Accounted.InputTokens != 5 {
			t.Fatalf("duplicate explicit response was double accounted: %+v", batch)
		}
	})

	t.Run("all lifecycle endings", func(t *testing.T) {
		for _, test := range []struct {
			name string
			kind LifecycleKind
			want TurnStatus
		}{
			{"complete", LifecycleCompleted, TurnCompleted},
			{"abort", LifecycleAborted, TurnAborted},
			{"fail", LifecycleFailed, TurnFailed},
		} {
			t.Run(test.name, func(t *testing.T) {
				state := processorTestState()
				baseline := testUsage(t, 4, 1, nil, 2, 1)
				state.Source.PreviousTotal = usagePointer(baseline)
				state.Source.PreviousTotalOffset = int64Pointer(5)
				start := lifecycleRecord(LifecycleStarted, "turn", 10)
				end := lifecycleRecord(test.kind, "turn", 20)
				batch, err := ProcessRecords(state, []OwnedRecord{start, end}, 891)
				if err != nil {
					t.Fatal(err)
				}
				if len(batch.TurnUpserts) != 1 || batch.TurnUpserts[0].Status != test.want ||
					batch.TurnUpserts[0].StartTotal == nil || batch.TurnUpserts[0].StartTotal.InputTokens != baseline.InputTokens {
					t.Fatalf("lifecycle %d did not persist expected turn: %+v", test.kind, batch.TurnUpserts)
				}
			})
		}
	})

	t.Run("sorted turn upserts", func(t *testing.T) {
		state := processorTestState()
		records := []OwnedRecord{
			lifecycleRecord(LifecycleStarted, "z-turn", 10), lifecycleRecord(LifecycleCompleted, "z-turn", 20),
			lifecycleRecord(LifecycleStarted, "a-turn", 30), lifecycleRecord(LifecycleCompleted, "a-turn", 40),
		}
		batch, err := ProcessRecords(state, records, 89)
		if err != nil {
			t.Fatal(err)
		}
		if len(batch.TurnUpserts) != 2 || batch.TurnUpserts[0].TurnKey != "a-turn" || batch.TurnUpserts[1].TurnKey != "z-turn" {
			t.Fatalf("turn upserts are not deterministically ordered: %+v", batch.TurnUpserts)
		}
	})
}

func processorTestState() CounterState {
	return CounterState{
		Source: SourceState{
			SourceFileID: 1, Generation: 1, DeviceID: 1, Inode: 2,
			UsageParserVersion: UsageParserVersion, CanonicalAlgorithmVersion: UsageCanonicalAlgorithmVersion,
			ObservedRawSize: 100, RawTailStatus: RawTailUnverified,
			OwningThreadID: "owner", RootSessionID: "owner", ContinuationState: ContinuationOwningLive,
			ChainState: ChainContinuous,
		},
		Carry: NewReconciliationCarry(),
	}
}

func processorTestStateWithTurn() CounterState {
	state := processorTestState()
	state.OpenTurn = &TurnWrite{
		SourceFileID: 1, Generation: 1, TurnKey: "turn", ThreadID: "owner",
		StartOffset: 1, Status: TurnOpen, Accounted: sharedusage.Zero(),
		ModelState: TurnValueNone, ReasoningEffortState: TurnValueNone,
		QualityStatus: "complete", StateThroughOffset: 1,
	}
	return state
}

func processorRecord(kind RawKind, start, end, physicalEnd int64) OwnedRecord {
	return OwnedRecord{
		Parsed:            ParsedRecord{Kind: kind, SourceFileID: 1, Generation: 1, StartOffset: start, EndOffset: end},
		Ownership:         rollout.Ownership{Kind: rollout.OwnershipOwning, ThreadID: "owner"},
		PhysicalEndOffset: physicalEnd,
	}
}

func tokenCountRecord(total sharedusage.NormalizedTokenUsage, last UsageValue, physicalEnd int64) OwnedRecord {
	record := processorRecord(RawTokenCount, 10, 20, physicalEnd)
	record.Parsed.HasTokenInfo = true
	record.Parsed.TimestampMS = int64Pointer(100)
	record.Parsed.Total = validUsage(total)
	record.Parsed.Last = last
	return record
}

func lifecycleRecord(lifecycle LifecycleKind, turnID string, start int64) OwnedRecord {
	record := processorRecord(RawLifecycle, start, start+10, start+10)
	record.Parsed.Lifecycle = lifecycle
	record.Parsed.TurnID = turnID
	record.Parsed.TimestampMS = int64Pointer(start + 100)
	return record
}

func assertSingleLegacyAccounting(t *testing.T, batch ProcessBatch, eventKind byte, input int64) {
	t.Helper()
	if len(batch.Candidates) != 1 || batch.Candidates[0].EvidenceKind != EvidenceLegacy || batch.Candidates[0].EventKind != eventKind ||
		len(batch.TurnUpserts) != 1 || batch.TurnUpserts[0].AccountedCandidateCount != 1 ||
		batch.TurnUpserts[0].Accounted.InputTokens != input {
		t.Fatalf("candidate/accounting mismatch: %+v", batch)
	}
}

func testUsage(t *testing.T, input, cached int64, cacheWrite *int64, output, reasoning int64) sharedusage.NormalizedTokenUsage {
	t.Helper()
	value, err := sharedusage.NewNormalizedTokenUsage(input, cached, cacheWrite, output, reasoning, input+output)
	if err != nil {
		t.Fatal(err)
	}
	return value
}

func usagePointer(value sharedusage.NormalizedTokenUsage) *sharedusage.NormalizedTokenUsage {
	return &value
}

func validUsage(value sharedusage.NormalizedTokenUsage) UsageValue {
	return UsageValue{State: UsageValueValid, Value: value}
}
