package rebuild

import (
	"errors"
	"fmt"
	"strings"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	"github.com/Hogeexxl/Usagi/internal/usage"
)

func TestNewCoordinatorRequiresAllThreeSeams(t *testing.T) {
	if _, err := NewCoordinator(nil, testActiveStateProof, func(*source.WriteTx, source.UsageWriteTarget, []string) error { return nil }); !errors.Is(err, ErrNilCoordinatorSeam) {
		t.Fatalf("nil comparator error = %v", err)
	}
	if _, err := NewCoordinator(testComparator, nil, func(*source.WriteTx, source.UsageWriteTarget, []string) error { return nil }); !errors.Is(err, ErrNilCoordinatorSeam) {
		t.Fatalf("nil Active proof error = %v", err)
	}
	if _, err := NewCoordinator(testComparator, testActiveStateProof, nil); !errors.Is(err, ErrNilCoordinatorSeam) {
		t.Fatalf("nil window stripper error = %v", err)
	}
}

func TestParserChangedSameTargetResumesAndAppendsDiscovery(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-a")
	h.insertSource(t, 1, "/sessions/a.jsonl", "root-a", 1, 11, 101, 128, "present")
	h.seedThread(t, "root-b")
	requirementA := requirement(1, 1, 11, 101, 128, 128, "root-a", "root-a")
	coordinator := newTestCoordinator(t, nil)
	var buildEpoch int64
	err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		buildEpoch, err = coordinator.BeginOrResume(tx, ParserChanged, 2, []MemberRequirement{requirementA}, nil, 10)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_source_checkpoints SET committed_offset=64,guard_hash=?,processing_status='ready' WHERE source_file_id=1 AND consumer_kind='usage'`, []byte("working-guard"))
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	h.insertSource(t, 2, "/sessions/b.jsonl", "root-b", 1, 12, 102, 64, "present")
	requirementB := requirement(2, 1, 12, 102, 64, 64, "root-b", "root-b")
	err = h.run.Storage().Write(func(tx *source.WriteTx) error {
		resumed, err := coordinator.BeginOrResume(tx, ParserChanged, 2, []MemberRequirement{requirementA, requirementB}, nil, 20)
		if err == nil && resumed != buildEpoch {
			return fmt.Errorf("resumed epoch=%d, want %d", resumed, buildEpoch)
		}
		return err
	})
	if err != nil {
		t.Fatalf("same target ParserChanged must resume: %v", err)
	}
	members := loadTestManifest(t, h, buildEpoch)
	if len(members) != 2 {
		t.Fatalf("manifest member count=%d, want 2", len(members))
	}
	if members[0].sourceFileID != 1 || members[0].completion != completionPending || members[0].carryPhase != carryNone {
		t.Fatalf("existing safe progress was reset: %+v", members[0])
	}
	assertCheckpoint(t, h, 1, 2, 64, []byte("working-guard"), "ready")
	if members[1].sourceFileID != 2 || members[1].membershipReason != "discovered_during_build" {
		t.Fatalf("new discovery was not appended: %+v", members[1])
	}
}

func TestResetAcceptsExplicitMissingHistoricalRequirement(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-a")
	h.insertSource(t, 1, "/historical/a.jsonl", "root-a", 1, 11, 101, 128, "missing")
	h.seedActiveSource(t, 1, 1, 11, 101, 1, 128, 128, "/historical/a.jsonl", "root-a", "none", nil, []byte("guard"))
	coordinator := newTestCoordinator(t, nil)
	var buildEpoch int64
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		buildEpoch, err = coordinator.BeginOrResume(tx, Bootstrap, 1, nil, nil, 10)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	requirementA := requirement(1, 1, 11, 101, 128, 128, "root-a", "root-a")
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return ResetBuildMembersTx(tx, buildEpoch, SourceInvalidated, []int64{1}, nil,
			[]MemberRequirement{requirementA}, testActiveStateProof, coordinator.stripWindows, 20)
	}); err != nil {
		t.Fatalf("explicit durable missing requirement was rejected: %v", err)
	}
	members := loadTestManifest(t, h, buildEpoch)
	if len(members) != 1 || members[0].completion != completionPending {
		t.Fatalf("reset manifest = %+v", members)
	}
	assertCheckpoint(t, h, 1, 1, 0, nil, "rebuild_required")
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return coordinator.BeginCarry(tx, buildEpoch, 1, SourceInvalidated, 30)
	}); err != nil {
		t.Fatal(err)
	}
	finished := false
	for step := 0; step < 20; step++ {
		var outcome CarryOutcome
		if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
			var err error
			outcome, err = newTestCoordinator(t, nil).ResumeCarry(tx, buildEpoch, 1, SourceInvalidated, int64(40+step))
			return err
		}); err != nil {
			t.Fatal(err)
		}
		if outcome == CarryFinalized {
			finished = true
			break
		}
	}
	if !finished {
		t.Fatal("durable missing member did not finish carry")
	}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error { _, err := coordinator.Activate(tx, buildEpoch, 1); return err }); err != nil {
		t.Fatalf("valid missing historical activation: %v", err)
	}
}

func TestLogicalRebindRefreshesFrozenProofWithoutWorkingCheckpoint(t *testing.T) {
	for _, compressed := range []bool{false, true} {
		name := "plain"
		if compressed {
			name = "compressed unverified NULL guard"
		}
		t.Run(name, func(t *testing.T) {
			h := newRebuildHarness(t)
			h.seedActiveEpoch(t, 1)
			h.seedThread(t, "root-old")
			h.seedThread(t, "root-new")
			path, tail, offset, guard, start := "/sessions/a.jsonl", "half_line", int64(64), []byte("old-guard"), int64Pointer(64)
			if compressed {
				path, tail, offset, guard, start = "/sessions/a.jsonl.zst", "unverified", 128, nil, nil
			}
			h.insertSource(t, 1, path, "root-old", 1, 11, 101, 128, "present")
			h.seedActiveSource(t, 1, 1, 11, 101, 1, offset, 128, path, "root-old", tail, start, guard)
			c := newTestCoordinator(t, nil)
			var epoch int64
			if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
				var err error
				epoch, err = c.BeginOrResume(tx, Bootstrap, 1, []MemberRequirement{requirement(1, 1, 11, 101, offset, 128, "root-old", "root-old")}, nil, 1)
				return err
			}); err != nil {
				t.Fatal(err)
			}
			old := loadTestManifest(t, h, epoch)[0]
			if old.activeOffset != offset || !bytesEqualNil(old.activeGuard, guard) || len(old.activeFingerprint) == 0 {
				t.Fatalf("initial frozen proof=%+v", old)
			}
			assertCheckpoint(t, h, 1, 1, 0, nil, "rebuild_required")
			if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
				if err := tx.Private(func(p storage.PrivateTx) error {
					if _, err := p.Exec("UPDATE codex_source_files SET thread_id='root-new' WHERE source_file_id=1"); err != nil {
						return err
					}
					_, err := p.Exec("UPDATE codex_usage_source_states SET owning_thread_id='root-new',root_session_id='root-new' WHERE ledger_epoch=1 AND source_file_id=1")
					return err
				}); err != nil {
					return err
				}
				return ResetBuildMembersTx(tx, epoch, SourceInvalidated, []int64{1}, nil, []MemberRequirement{requirement(1, 1, 11, 101, offset, 128, "root-new", "root-new")}, testActiveStateProof, c.stripWindows, 2)
			}); err != nil {
				t.Fatal(err)
			}
			updated := loadTestManifest(t, h, epoch)[0]
			if updated.activeOffset != old.activeOffset || !bytesEqualNil(updated.activeGuard, old.activeGuard) || len(updated.activeFingerprint) == 0 || string(updated.activeFingerprint) == string(old.activeFingerprint) {
				t.Fatalf("rebound proof old=%+v new=%+v", old, updated)
			}
		})
	}
}

func TestFreezePropagatesCheckpointScanErrorAndRollsBackBuild(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-a")
	h.insertSource(t, 1, "/sessions/a.jsonl", "root-a", 1, 11, 101, 128, "present")
	h.seedActiveSource(t, 1, 1, 11, 101, 1, 128, 128, "/sessions/a.jsonl", "root-a", "none", nil, []byte("guard"))
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec("UPDATE codex_source_checkpoints SET parser_version='invalid' WHERE source_file_id=1 AND consumer_kind='usage'")
			return err
		})
	}); err != nil {
		t.Fatalf("seed malformed scan value: %v", err)
	}
	coordinator := newTestCoordinator(t, nil)
	requirementA := requirement(1, 1, 11, 101, 128, 128, "root-a", "root-a")
	err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := coordinator.BeginOrResume(tx, Bootstrap, 1, []MemberRequirement{requirementA}, nil, 10)
		return err
	})
	if err == nil {
		t.Fatal("checkpoint scan conversion error was swallowed")
	}
	state, found, err := h.run.Storage().LoadUsageEpoch()
	if err != nil {
		t.Fatal(err)
	}
	if !found || state.BuildEpoch != nil {
		t.Fatalf("failed freeze left a Build target: %+v, found=%v", state, found)
	}
}

func TestBuildCleanupPreservesSharedCanonicalFactAndStripsOrphanWindow(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-a")
	h.insertSource(t, 1, "/sessions/a.jsonl", "root-a", 1, 11, 101, 128, "present")
	h.insertSource(t, 2, "/sessions/b.jsonl", "root-a", 1, 12, 102, 128, "present")
	coordinator := newTestCoordinator(t, nil)
	requirements := []MemberRequirement{
		requirement(1, 1, 11, 101, 128, 128, "root-a", "root-a"),
		requirement(2, 1, 12, 102, 128, 128, "root-a", "root-a"),
	}
	var buildEpoch int64
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		buildEpoch, err = coordinator.BeginOrResume(tx, Bootstrap, 1, requirements, nil, 10)
		if err != nil {
			return err
		}
		if err := writeCanonicalBuildEvent(tx, "shared-event", "root-a"); err != nil {
			return err
		}
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`INSERT INTO codex_usage_event_facts(source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation)
				VALUES('codex',?,'shared-event','root-a',NULL,'legacy','response')`, buildEpoch); err != nil {
				return err
			}
			for _, sourceID := range []int64{1, 2} {
				if _, err := private.Exec(`INSERT INTO codex_usage_event_occurrences(source,ledger_epoch,source_file_id,file_generation,
					source_start_offset,source_end_offset,event_id,created_at_ms) VALUES('codex',?,?,1,0,20,'shared-event',1)`, buildEpoch, sourceID); err != nil {
					return err
				}
			}
			_, err := private.Exec(`INSERT INTO codex_usage_reconciliation_windows(source,ledger_epoch,source_file_id,file_generation,
				source_start_offset,source_end_offset,owning_thread_id,turn_key,state_json)
				VALUES('codex',?,?,1,0,32,'root-a',NULL,'{"refs":["shared-event"]}')`, buildEpoch, int64(2))
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	stripCalls := 0
	stripper := func(tx *source.WriteTx, target source.UsageWriteTarget, eventIDs []string) error {
		if target != source.UsageTargetBuild || len(eventIDs) != 1 {
			return fmt.Errorf("unexpected stripper arguments: target=%d IDs=%v", target, eventIDs)
		}
		if eventIDs[0] == "shared-event" {
			return errors.New("shared canonical event was treated as orphan")
		}
		if eventIDs[0] != "orphan-event" {
			return fmt.Errorf("unexpected orphan event %q", eventIDs[0])
		}
		stripCalls++
		return tx.Private(func(private storage.PrivateTx) error {
			var canonical, fact int64
			if err := private.QueryRow("SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=? AND event_id=?", buildEpoch, eventIDs[0]).Scan(&canonical); err != nil {
				return err
			}
			if err := private.QueryRow("SELECT count(*) FROM codex_usage_event_facts WHERE ledger_epoch=? AND event_id=?", buildEpoch, eventIDs[0]).Scan(&fact); err != nil {
				return err
			}
			if canonical != 1 || fact != 1 {
				return errors.New("stripper ran after canonical/fact deletion")
			}
			_, err := private.Exec("UPDATE codex_usage_reconciliation_windows SET state_json='{}' WHERE ledger_epoch=? AND source_file_id=2", buildEpoch)
			return err
		})
	}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return ResetBuildMembersTx(tx, buildEpoch, SourceInvalidated, []int64{1}, nil,
			requirements, testActiveStateProof, stripper, 20)
	}); err != nil {
		t.Fatal(err)
	}
	if stripCalls != 0 {
		t.Fatalf("shared canonical event was treated as orphan; stripper calls=%d", stripCalls)
	}
	var occurrenceA, occurrenceB, canonical, fact int64
	if err := h.run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
		if err := reader.QueryRow("SELECT count(*) FROM codex_usage_event_occurrences WHERE ledger_epoch=? AND source_file_id=1", buildEpoch).Scan(&occurrenceA); err != nil {
			return err
		}
		if err := reader.QueryRow("SELECT count(*) FROM codex_usage_event_occurrences WHERE ledger_epoch=? AND source_file_id=2", buildEpoch).Scan(&occurrenceB); err != nil {
			return err
		}
		if err := reader.QueryRow("SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=? AND event_id='shared-event'", buildEpoch).Scan(&canonical); err != nil {
			return err
		}
		return reader.QueryRow("SELECT count(*) FROM codex_usage_event_facts WHERE ledger_epoch=? AND event_id='shared-event'", buildEpoch).Scan(&fact)
	}); err != nil {
		t.Fatal(err)
	}
	if occurrenceA != 0 || occurrenceB != 1 || canonical != 1 || fact != 1 {
		t.Fatalf("shared cleanup counts A=%d B=%d canonical=%d fact=%d", occurrenceA, occurrenceB, canonical, fact)
	}

	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetBuild, testCanonicalEvent("orphan-event", "root-a")); err != nil {
			return err
		}
		if err := tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`INSERT INTO codex_usage_event_facts(source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation)
				VALUES('codex',?,'orphan-event','root-a',NULL,'legacy','response')`, buildEpoch); err != nil {
				return err
			}
			if _, err := private.Exec(`INSERT INTO codex_usage_event_occurrences(source,ledger_epoch,source_file_id,file_generation,
				source_start_offset,source_end_offset,event_id,created_at_ms) VALUES('codex',?,1,1,30,50,'orphan-event',1)`, buildEpoch); err != nil {
				return err
			}
			return nil
		}); err != nil {
			return err
		}
		if err := tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_usage_reconciliation_windows SET state_json='{"refs":["orphan-event"]}'
				WHERE ledger_epoch=? AND source_file_id=2`, buildEpoch)
			return err
		}); err != nil {
			return err
		}
		_, err := CleanupBuildMembersSemantic(tx, buildEpoch, []int64{1}, stripper)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if stripCalls != 1 {
		t.Fatalf("orphan stripper calls=%d, want 1", stripCalls)
	}
	var stateJSON string
	if err := h.run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow(`SELECT state_json FROM codex_usage_reconciliation_windows WHERE ledger_epoch=? AND source_file_id=2`, buildEpoch).Scan(&stateJSON)
	}); err != nil {
		t.Fatal(err)
	}
	if stateJSON != "{}" {
		t.Fatalf("surviving window state=%q, want stripped {}", stateJSON)
	}
}

func TestCompressedUnverifiedActiveStateCarriesAndActivatesWithNilGuard(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-z")
	h.insertSource(t, 1, "/sessions/z.jsonl.zst", "root-z", 1, 11, 101, 128, "present")
	h.seedActiveSource(t, 1, 1, 11, 101, 1, 128, 128, "/sessions/z.jsonl.zst", "root-z", "unverified", nil, nil)
	coordinator := newTestCoordinator(t, nil)
	requirementZ := requirement(1, 1, 11, 101, 128, 128, "root-z", "root-z")
	var buildEpoch int64
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		buildEpoch, err = coordinator.BeginOrResume(tx, Bootstrap, 1, []MemberRequirement{requirementZ}, nil, 10)
		return err
	}); err != nil {
		t.Fatalf("freeze legal compressed Active terminal: %v", err)
	}
	member := loadTestManifest(t, h, buildEpoch)[0]
	if member.activeOffset != 128 || member.activeGuard != nil || len(member.activeFingerprint) == 0 || member.tailStatus != "unverified" {
		t.Fatalf("compressed Active proof was dropped: %+v", member)
	}
	assertCheckpoint(t, h, 1, 1, 0, nil, "rebuild_required")
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return coordinator.BeginCarry(tx, buildEpoch, 1, SourceInvalidated, 20)
	}); err != nil {
		t.Fatalf("begin carry from compressed Active terminal: %v", err)
	}
	finalized := false
	for step := 0; step < 10; step++ {
		var outcome CarryOutcome
		err := h.run.Storage().Write(func(tx *source.WriteTx) error {
			var err error
			outcome, err = newTestCoordinator(t, nil).ResumeCarry(tx, buildEpoch, 1, SourceInvalidated, int64(30+step))
			return err
		})
		if err != nil {
			t.Fatalf("resume carry step %d: %v", step, err)
		}
		if outcome == CarryFinalized {
			finalized = true
			break
		}
	}
	if !finalized {
		t.Fatal("carry did not finalize after restartable phase steps")
	}
	member = loadTestManifest(t, h, buildEpoch)[0]
	if member.completion != completionCarried || member.completedOffset.Int64 != 128 {
		t.Fatalf("compressed carry completion = %+v", member)
	}
	assertCheckpoint(t, h, 1, 1, 128, nil, "ready")
	var activation source.UsageActivationOutcome
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		activation, err = coordinator.Activate(tx, buildEpoch, 1)
		return err
	}); err != nil {
		t.Fatalf("activate carried compressed terminal: %v", err)
	}
	if activation.ActiveEpoch != buildEpoch {
		t.Fatalf("active epoch=%d, want %d", activation.ActiveEpoch, buildEpoch)
	}
}

func TestCarryUnionsSkillOffsetsAndHoldOnlyEventsAcrossRestarts(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-a")
	h.insertSource(t, 1, "/sessions/a.jsonl", "root-a", 1, 11, 101, 128, "present")
	h.seedActiveSource(t, 1, 1, 11, 101, 1, 128, 128, "/sessions/a.jsonl", "root-a", "none", nil, []byte("guard"))
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetActive, testCanonicalEvent("hold-only", "root-a")); err != nil {
			return err
		}
		costed := testCanonicalEvent("marker-event", "root-a")
		costed.EstimatedCostNanosUSD = int64Pointer(17)
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetActive, costed); err != nil {
			return err
		}
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`INSERT INTO codex_skill_usage_events(ledger_epoch,source_file_id,file_generation,
				source_start_offset,source_end_offset,occurred_at_ms,thread_id,root_session_id,model,skill_name,created_at_ms)
				VALUES(1,1,1,8,16,1,'root-a','root-a',NULL,'skill-only',1)`); err != nil {
				return err
			}
			if _, err := private.Exec(`INSERT INTO codex_usage_event_holds(source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason)
				VALUES('codex',1,1,1,'hold-only','carry')`); err != nil {
				return err
			}
			for _, statement := range []string{
				`INSERT INTO codex_usage_event_facts(source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation) VALUES('codex',1,'marker-event','root-a','response','explicit','compaction')`,
				`INSERT INTO codex_usage_event_occurrences VALUES('codex',1,1,1,16,24,'marker-event',1)`,
				`INSERT INTO codex_compaction_markers(source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,root_session_id,response_id,resolved_event_id) VALUES('codex',1,1,1,32,40,'root-a','root-a','response','marker-event')`,
				`INSERT INTO codex_usage_reconciliation_windows(source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,state_json) VALUES('codex',1,1,1,48,56,'root-a','{}')`,
				`INSERT INTO codex_turns(ledger_epoch,source_file_id,file_generation,turn_key,thread_id,start_offset,status,accounted_input_tokens,accounted_cached_tokens,accounted_output_tokens,accounted_reasoning_tokens,accounted_total_tokens,accounted_fingerprint,accounted_candidate_count,model_state,unresolved_model_seen,reasoning_effort_state,unresolved_reasoning_effort_seen,compensation_allowed,block_start_missing,block_time_missing,block_reset,block_ownership_gap,block_parser_gap,block_required_invalid,block_model_unresolved,quality_status,state_through_offset,updated_at_ms) VALUES(1,1,1,'carry-turn','root-a',0,'open',0,0,0,0,0,X'00',0,'none',0,'none',0,1,0,0,0,0,0,0,0,'complete',128,1)`,
				`INSERT INTO codex_ingest_anomalies(ledger_epoch,anomaly_id,detected_at_ms,thread_id,source_file_id,file_generation,anomaly_type,severity,details_json) VALUES(1,'no-copy',1,'root-a',1,1,'fixture','warning','{}')`,
			} {
				if _, err := private.Exec(statement); err != nil {
					return err
				}
			}
			return nil
		})
	}); err != nil {
		t.Fatal(err)
	}
	coordinator := newTestCoordinator(t, nil)
	var buildEpoch int64
	memberReq := requirement(1, 1, 11, 101, 128, 128, "root-a", "root-a")
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		buildEpoch, err = coordinator.BeginOrResume(tx, Bootstrap, 1, []MemberRequirement{memberReq}, nil, 10)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return coordinator.BeginCarry(tx, buildEpoch, 1, SourceInvalidated, 20)
	}); err != nil {
		t.Fatal(err)
	}
	observedHoldCopy := false
	finalized := false
	seenPhases := map[string]bool{"occurrences": true}
	for step := 0; step < 24; step++ {
		var outcome CarryOutcome
		if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
			var err error
			outcome, err = newTestCoordinator(t, nil).ResumeCarry(tx, buildEpoch, 1, SourceInvalidated, int64(30+step))
			return err
		}); err != nil {
			t.Fatalf("restartable carry phase %d: %v", step, err)
		}
		var phase string
		var holdCount, skillCount int64
		if err := h.run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
			if err := reader.QueryRow("SELECT carry_phase FROM codex_usage_build_sources WHERE build_epoch=? AND source_file_id=1", buildEpoch).Scan(&phase); err != nil {
				return err
			}
			if err := reader.QueryRow("SELECT count(*) FROM codex_usage_event_holds WHERE ledger_epoch=? AND source_file_id=1 AND hold_reason='carry'", buildEpoch).Scan(&holdCount); err != nil {
				return err
			}
			return reader.QueryRow("SELECT count(*) FROM codex_skill_usage_events WHERE ledger_epoch=? AND source_file_id=1 AND skill_name='skill-only'", buildEpoch).Scan(&skillCount)
		}); err != nil {
			t.Fatal(err)
		}
		if phase == "markers" && holdCount == 1 {
			observedHoldCopy = true
			activationErr := h.run.Storage().Write(func(tx *source.WriteTx) error { _, err := coordinator.Activate(tx, buildEpoch, 1); return err })
			if !errors.Is(activationErr, ErrActivationBlocked) || !strings.Contains(activationErr.Error(), "unresolved event holds") {
				t.Fatalf("hold did not block activation: %v", activationErr)
			}
		}
		seenPhases[phase] = true
		if phase == "anomalies" {
			var stateCount, anomalyCount int64
			if err := h.run.Storage().PrivateRead(func(r storage.PrivateReader) error {
				if err := r.QueryRow("SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=1", buildEpoch).Scan(&stateCount); err != nil {
					return err
				}
				return r.QueryRow("SELECT count(*) FROM codex_ingest_anomalies WHERE ledger_epoch=?", buildEpoch).Scan(&anomalyCount)
			}); err != nil {
				t.Fatal(err)
			}
			if stateCount != 1 || anomalyCount != 0 {
				t.Fatalf("turns transition sourceState=%d anomalies=%d", stateCount, anomalyCount)
			}
			if m := loadTestManifest(t, h, buildEpoch)[0]; m.afterAnomalyID.Valid {
				t.Fatal("anomaly cursor must remain NULL")
			}
		}
		if outcome == CarryFinalized {
			finalized = true
			break
		}
	}
	if !observedHoldCopy || !finalized {
		t.Fatalf("hold-only carry observed=%v finalized=%v", observedHoldCopy, finalized)
	}
	for _, phase := range []string{"occurrences", "facts", "markers", "windows", "turns", "anomalies", "finalize"} {
		if !seenPhases[phase] {
			t.Fatalf("crash/resume phase %s not visited", phase)
		}
	}
	var buildCanonical, buildHold, buildSkill, buildState int64
	if err := h.run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
		if err := reader.QueryRow("SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=? AND event_id='hold-only'", buildEpoch).Scan(&buildCanonical); err != nil {
			return err
		}
		if err := reader.QueryRow("SELECT count(*) FROM codex_usage_event_holds WHERE ledger_epoch=? AND source_file_id=1", buildEpoch).Scan(&buildHold); err != nil {
			return err
		}
		if err := reader.QueryRow("SELECT count(*) FROM codex_skill_usage_events WHERE ledger_epoch=? AND source_file_id=1 AND skill_name='skill-only'", buildEpoch).Scan(&buildSkill); err != nil {
			return err
		}
		return reader.QueryRow("SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=1", buildEpoch).Scan(&buildState)
	}); err != nil {
		t.Fatal(err)
	}
	if buildCanonical != 1 || buildHold != 0 || buildSkill != 1 || buildState != 1 {
		t.Fatalf("carried rows canonical=%d hold=%d skill=%d sourceState=%d", buildCanonical, buildHold, buildSkill, buildState)
	}
	var cost, created int64
	if err := h.run.Storage().PrivateRead(func(r storage.PrivateReader) error {
		return r.QueryRow("SELECT estimated_cost_nanos_usd,created_at_ms FROM usage_events WHERE source='codex' AND source_epoch=? AND event_id='marker-event'", buildEpoch).Scan(&cost, &created)
	}); err != nil {
		t.Fatal(err)
	}
	if cost != 17 || created != 11 {
		t.Fatalf("carry lost Cost/CreatedAt: %d/%d", cost, created)
	}
	before := testDataRevision(t, h)
	var activation source.UsageActivationOutcome
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		activation, err = coordinator.Activate(tx, buildEpoch, 1)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if activation.VisibleChanged || activation.DataRevision != before {
		t.Fatalf("identical canonical carry changed visibility: %+v", activation)
	}
}

func TestRecordRebuiltRequiresCompressedFullViewProofAndActivationRejectsPending(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedThread(t, "root-z")
	h.insertSource(t, 1, "/sessions/z.jsonl.zst", "root-z", 1, 11, 101, 128, "present")
	coordinator := newTestCoordinator(t, nil)
	memberReq := requirement(1, 1, 11, 101, 128, 128, "root-z", "root-z")
	var buildEpoch int64
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		buildEpoch, err = coordinator.BeginOrResume(tx, Bootstrap, 1, []MemberRequirement{memberReq}, nil, 10)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := coordinator.Activate(tx, buildEpoch, 1)
		return err
	}); !errors.Is(err, ErrActivationBlocked) {
		t.Fatalf("pending activation error=%v", err)
	}
	seedBuildStateAndCheckpoint(t, h, buildEpoch, 1, 128, 128, "unverified", nil, nil)
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return RecordRebuilt(tx, buildEpoch, 1, false, 20)
	}); !errors.Is(err, ErrActivationBlocked) {
		t.Fatalf("compressed terminal without full-view proof error=%v", err)
	}
	if got := loadTestManifest(t, h, buildEpoch)[0].completion; got != completionPending {
		t.Fatalf("rejected compressed completion persisted as %q", got)
	}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return RecordRebuilt(tx, buildEpoch, 1, true, 30)
	}); err != nil {
		t.Fatalf("full compressed view terminal rejected: %v", err)
	}
	if got := loadTestManifest(t, h, buildEpoch)[0].completion; got != completionRebuilt {
		t.Fatalf("compressed completion=%q", got)
	}
}

func TestParserChangedBlocksMissingHistoricalMemberAndResumeDoesNotReset(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-a")
	h.insertSource(t, 1, "/sessions/a.jsonl", "root-a", 1, 11, 101, 128, "present")
	h.insertSource(t, 2, "/historical/b.jsonl", "root-a", 1, 12, 102, 64, "missing")
	h.seedActiveSource(t, 2, 1, 12, 102, 1, 64, 64, "/historical/b.jsonl", "root-a", "none", nil, []byte("b-guard"))
	coordinator := newTestCoordinator(t, nil)
	requirementA := requirement(1, 1, 11, 101, 128, 128, "root-a", "root-a")
	var buildEpoch int64
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		buildEpoch, err = coordinator.BeginOrResume(tx, Bootstrap, 1, []MemberRequirement{requirementA}, nil, 10)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := coordinator.BeginOrResume(tx, ParserChanged, 2, []MemberRequirement{requirementA, requirement(2, 1, 12, 102, 64, 64, "root-a", "root-a")}, nil, 20)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	members := loadTestManifest(t, h, buildEpoch)
	var missing manifestMember
	for _, member := range members {
		if member.sourceFileID == 2 {
			missing = member
		}
	}
	if missing.completion != completionBlocked || !missing.errorCode.Valid || missing.errorCode.String != "PARSER_CHANGED_RAW_MISSING" ||
		missing.completedGeneration.Valid || missing.completedOffset.Valid || missing.carryFrom.Valid || missing.carryPhase != carryNone ||
		missing.afterStartOffset.Valid || missing.afterTurnKey.Valid || missing.afterFactEventID.Valid || missing.afterMarkerOffset.Valid || missing.afterWindowOffset.Valid {
		t.Fatalf("missing parser replacement member = %+v", missing)
	}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := coordinator.BeginOrResume(tx, ParserChanged, 2, []MemberRequirement{requirementA}, nil, 30)
		return err
	}); err != nil {
		t.Fatalf("same target ParserChanged resume rejected: %v", err)
	}
	resumed := loadTestManifest(t, h, buildEpoch)
	for _, member := range resumed {
		if member.sourceFileID == 2 && (member.completion != completionBlocked || member.errorCode.String != "PARSER_CHANGED_RAW_MISSING") {
			t.Fatalf("same-target resume reset parser blocker: %+v", member)
		}
	}
}

func seedBuildStateAndCheckpoint(t *testing.T, h *rebuildHarness, epoch, parser, offset, observed int64, tail string, tailStart *int64, guard []byte) {
	t.Helper()
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`INSERT INTO codex_source_checkpoints(source_file_id,consumer_kind,parser_version,committed_offset,
				guard_hash,processing_status,last_successful_scan_at_ms,last_error_code) VALUES(1,'usage',?,?,?,'ready',1,NULL)
				ON CONFLICT(source_file_id,consumer_kind) DO UPDATE SET parser_version=excluded.parser_version,
				committed_offset=excluded.committed_offset,guard_hash=excluded.guard_hash,processing_status='ready'`, parser, offset, guard); err != nil {
				return err
			}
			_, err := private.Exec(`INSERT INTO codex_usage_source_states(ledger_epoch,source_file_id,file_generation,device_id,inode,
				usage_parser_version,canonical_algorithm_version,resolved_through_offset,observed_raw_size,raw_tail_status,
				raw_tail_start_offset,owning_thread_id,root_session_id,continuation_state,chain_state,updated_at_ms)
				VALUES(?,1,1,11,101,?,1,?,?,?,?, 'root-z','root-z','owning_live','continuous',1)`,
				epoch, parser, offset, observed, tail, tailStart)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
}

func testCanonicalEvent(eventID, threadID string) usage.CanonicalUsageEventWrite {
	cacheWrite := int64(0)
	return usage.CanonicalUsageEventWrite{
		EventID: eventID, Kind: usage.EventKindNormal, OccurredAtMS: 10, ThreadID: threadID, RootSessionID: threadID,
		Model: "model", Usage: usage.NormalizedTokenUsage{InputTokens: 10, CachedTokens: 2, CacheWriteTokens: &cacheWrite,
			OutputTokens: 4, ReasoningTokens: 1, TotalTokens: 14}, CreatedAtMS: 11,
	}
}

func int64Pointer(value int64) *int64 { return &value }
