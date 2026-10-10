package usage

import (
	"bytes"
	"strings"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

func TestTurnCompressedPhysicalSentinelAllowsLargerLogicalOffsets(t *testing.T) {
	run := newUsageTestRun(t)
	var sourceFileID int64
	modelOffset, effortOffset, previousOffset := int64(200), int64(220), int64(10)
	turnKey, model, effort := "turn", "gpt-model", "high"
	previousTotal := testUsage(t, 8, 1, nil, 2, 1)
	carry, expectedCarry := carryWireTestValue()
	carryJSON, err := CanonicalReconciliationCarryJSON(carry)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(carryJSON, expectedCarry) {
		t.Fatalf("carry fixture canonical bytes differ: %s", carryJSON)
	}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		if err := seedUsageTestThreads(tx, "owner", "root"); err != nil {
			return err
		}
		if err := activateUsageTestEpoch(tx); err != nil {
			return err
		}
		var err error
		sourceFileID, err = insertUsageTestSourceFile(tx, "compressed-offsets.jsonl.zst", "owner")
		if err != nil {
			return err
		}
		if err := tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_source_files SET observed_size=20 WHERE source_file_id=?`, sourceFileID)
			return err
		}); err != nil {
			return err
		}
		state := SourceState{
			SourceFileID: sourceFileID, Generation: 1, DeviceID: 1, Inode: 1,
			UsageParserVersion: UsageParserVersion, CanonicalAlgorithmVersion: UsageCanonicalAlgorithmVersion,
			ResolvedThroughOffset: 10, ObservedRawSize: 20, RawTailStatus: RawTailUnverified,
			OwningThreadID: "owner", RootSessionID: "root", ContinuationState: ContinuationOwningLive,
			PreviousTotal: &previousTotal, PreviousTotalOffset: &previousOffset,
			ChainState: ChainContinuous, ActiveTurnKey: &turnKey,
			ActiveModel: &model, ActiveModelOffset: &modelOffset,
			ActiveReasoningEffort: &effort, ActiveReasoningEffortOffset: &effortOffset,
			UpdatedAtMS: 30, ReconciliationStateJSON: carryJSON,
		}
		if err := WriteSourceState(tx, source.UsageTargetActive, state); err != nil {
			return err
		}
		turn := TurnWrite{
			SourceFileID: sourceFileID, Generation: 1, TurnKey: turnKey, ThreadID: "owner",
			StartOffset: 100, Status: TurnOpen, StartTotal: &previousTotal, LastTotal: &previousTotal,
			Accounted: sharedusage.Zero(), ModelState: TurnValueSingle, SingleModel: &model,
			ReasoningEffortState: TurnValueSingle, SingleReasoningEffort: &effort,
			QualityStatus: "complete", StateThroughOffset: 500, UpdatedAtMS: 30,
		}
		if err := WriteTurn(tx, source.UsageTargetActive, turn); err != nil {
			return err
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		loaded, found, err := LoadCounterState(tx, source.UsageTargetActive, sourceFileID, 1)
		if err != nil {
			return err
		}
		loadedCarryJSON, err := CanonicalReconciliationCarryJSON(loaded.Carry)
		if err != nil {
			return err
		}
		if !found || loaded.Source.ObservedRawSize != 20 || loaded.Source.ResolvedThroughOffset != 10 ||
			loaded.Source.PreviousTotalOffset == nil || *loaded.Source.PreviousTotalOffset != 10 ||
			loaded.Source.ActiveModelOffset == nil || *loaded.Source.ActiveModelOffset != modelOffset ||
			loaded.Source.ActiveReasoningEffortOffset == nil || *loaded.Source.ActiveReasoningEffortOffset != effortOffset ||
			loaded.OpenTurn == nil || loaded.OpenTurn.StartOffset != 100 || loaded.OpenTurn.StateThroughOffset != 500 ||
			!bytes.Equal(loadedCarryJSON, expectedCarry) {
			t.Fatalf("restart load rejected or truncated canonical carry/logical offsets: found=%t state=%+v turn=%+v carry=%s", found, loaded.Source, loaded.OpenTurn, loadedCarryJSON)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestCounterCarryCanonicalJSONWire(t *testing.T) {
	carry, expected := carryWireTestValue()
	encoded, err := CanonicalReconciliationCarryJSON(carry)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(encoded, expected) {
		t.Fatalf("canonical carry wire = %s", encoded)
	}
	decoded, err := DecodeReconciliationCarryJSON(encoded)
	if err != nil {
		t.Fatal(err)
	}
	reencoded, err := CanonicalReconciliationCarryJSON(decoded)
	if err != nil || !bytes.Equal(reencoded, encoded) {
		t.Fatalf("carry decode/encode mismatch: %s err=%v", reencoded, err)
	}
	for name, invalid := range map[string][]byte{
		"top-level unknown field":       append([]byte(strings.TrimSuffix(string(encoded), "}")), []byte(`,"future":1}`)...),
		"nested response unknown field": bytes.Replace(encoded, []byte(`"response_id":"response-id"`), []byte(`"response_id":"response-id","future":1`), 1),
		"noncanonical whitespace":       bytes.Replace(encoded, []byte(`{"version":1`), []byte(`{"version": 1`), 1),
		"wrong tagged wire kind":        bytes.Replace(encoded, []byte(`"kind":"response_usage"`), []byte(`"kind":"response"`), 1),
	} {
		t.Run(name, func(t *testing.T) {
			if _, err := DecodeReconciliationCarryJSON(invalid); err == nil {
				t.Fatal("noncanonical or invalid carry JSON was accepted")
			}
		})
	}
}

func carryWireTestValue() (ReconciliationCarry, []byte) {
	usage := sharedusage.NormalizedTokenUsage{
		InputTokens: 5, CachedTokens: 1, OutputTokens: 2, ReasoningTokens: 1, TotalTokens: 7,
	}
	carry := ReconciliationCarry{
		Version: 1, OpenWindowStartOffset: uint64Pointer(7), PendingResponseIDs: []string{"z", "a", "a"},
		ModernCounterDomain: &ModernCounterDomain{ThreadID: "thread"}, ModernCounterTotal: &usage,
		PendingEvidence: []PendingUsageEvidence{
			{
				Record: PendingEvidenceRecord{Kind: PendingResponseUsage, TimestampMS: int64Pointer(123), StartOffset: 20, EndOffset: 25,
					Response: &ResponseEvidence{ResponseID: "response-id", ThreadID: "owner", TurnID: "turn-id",
						Usage: validUsage(usage), ThreadTokenUsage: UsageValue{State: UsageValueMissing}}},
				ReasoningEffort: stringPointer("high"),
			},
			{
				Record: PendingEvidenceRecord{Kind: PendingCompacted, StartOffset: 10, EndOffset: 15,
					Compaction: &CompactionEvidence{ResponseID: "compact-id"}},
				Model: stringPointer("model"),
			},
		},
	}
	return carry, []byte(`{"version":1,"open_window_start_offset":7,"pending_response_ids":["a","z"],"modern_counter_domain":["thread",null],"modern_counter_total":{"input_tokens":5,"cached_tokens":1,"cache_write_tokens":null,"output_tokens":2,"reasoning_tokens":1,"total_tokens":7},"pending_evidence":[{"record":{"kind":"compacted","timestamp_ms":null,"start_offset":10,"end_offset":15,"evidence":{"compaction_response_id":"compact-id","latest_token_usage_record":null}},"model":"model","reasoning_effort":null},{"record":{"kind":"response_usage","timestamp_ms":123,"start_offset":20,"end_offset":25,"evidence":{"response_id":"response-id","thread_id":"owner","session_id":null,"turn_id":"turn-id","usage":{"kind":"valid","usage":{"input_tokens":5,"cached_tokens":1,"cache_write_tokens":null,"output_tokens":2,"reasoning_tokens":1,"total_tokens":7}},"thread_token_usage":{"kind":"missing"}}},"model":null,"reasoning_effort":"high"}]}`)
}

func uint64Pointer(value uint64) *uint64 { return &value }
