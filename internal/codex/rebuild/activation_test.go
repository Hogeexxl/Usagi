package rebuild

import (
	"errors"
	"strings"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

func TestEmptyHomeBootstrapAndComparatorActivationRollback(t *testing.T) {
	for _, mode := range []string{"bootstrap", "equal", "different", "error"} {
		t.Run(mode, func(t *testing.T) {
			h := newRebuildHarness(t)
			if mode != "bootstrap" {
				h.seedActiveEpoch(t, 1)
			}
			before := testDataRevision(t, h)
			compareErr := errors.New("comparison failed")
			comparator := func(storage.PrivateTx, domain.SourceID, int64, int64, int64, int64) (bool, error) {
				if mode == "error" {
					return false, compareErr
				}
				return mode != "different", nil
			}
			c, err := NewCoordinator(comparator, testActiveStateProof, func(*source.WriteTx, source.UsageWriteTarget, []string) error { return nil })
			if err != nil {
				t.Fatal(err)
			}
			var epoch int64
			if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
				var err error
				epoch, err = c.BeginOrResume(tx, Bootstrap, 1, nil, nil, 1)
				return err
			}); err != nil {
				t.Fatal(err)
			}
			var outcome source.UsageActivationOutcome
			err = h.run.Storage().Write(func(tx *source.WriteTx) error { var err error; outcome, err = c.Activate(tx, epoch, 1); return err })
			if mode == "error" {
				if !errors.Is(err, compareErr) {
					t.Fatalf("error=%v", err)
				}
				assertBuildRetained(t, h, epoch)
				if testDataRevision(t, h) != before {
					t.Fatal("failed comparator changed revision")
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			want := before
			if mode == "different" {
				want++
			}
			if outcome.DataRevision != want || outcome.VisibleChanged != (mode == "different") {
				t.Fatalf("outcome=%+v want revision %d", outcome, want)
			}
			state, _, err := h.run.Storage().LoadUsageEpoch()
			if err != nil || state.BuildEpoch != nil || state.ActiveEpoch != epoch {
				t.Fatalf("state=%+v error=%v", state, err)
			}
		})
	}
}

func TestRedundantActivationRequiresExplicitFreshProof(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-z")
	h.insertSource(t, 1, "/sessions/winner.jsonl", "root-z", 1, 11, 101, 128, "present")
	h.insertSource(t, 2, "/sessions/twin.jsonl", "root-z", 1, 12, 102, 128, "present")
	c := newTestCoordinator(t, nil)
	var epoch int64
	requirements := []MemberRequirement{requirement(1, 1, 11, 101, 128, 128, "root-z", "root-z"), requirement(2, 1, 12, 102, 128, 128, "root-z", "root-z")}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		epoch, err = c.BeginOrResume(tx, Bootstrap, 1, requirements, nil, 1)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	seedBuildStateAndCheckpoint(t, h, epoch, 1, 128, 128, "none", nil, []byte("guard"))
	proof := RedundancyActivationProof{SourceFileID: 2, Generation: 1, WinnerSourceFileID: 1, WinnerGeneration: 1, RequiredThroughOffset: 128}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		if err := RecordRebuilt(tx, epoch, 1, false, 2); err != nil {
			return err
		}
		if err := tx.Private(func(p storage.PrivateTx) error {
			_, err := p.Exec("UPDATE codex_source_checkpoints SET committed_offset=128,guard_hash=?,processing_status='ready' WHERE source_file_id=2 AND consumer_kind='usage'", []byte("twin-guard"))
			return err
		}); err != nil {
			return err
		}
		return RecordVerifiedRedundant(tx, epoch, proof, 2)
	}); err != nil {
		t.Fatal(err)
	}
	for _, test := range []struct {
		name   string
		proofs []RedundancyActivationProof
	}{
		{"old durable terminal", nil},
		{"wrong generation", []RedundancyActivationProof{{SourceFileID: 2, Generation: 2, WinnerSourceFileID: 1, WinnerGeneration: 1, RequiredThroughOffset: 128}}},
		{"wrong winner", []RedundancyActivationProof{{SourceFileID: 2, Generation: 1, WinnerSourceFileID: 1, WinnerGeneration: 2, RequiredThroughOffset: 128}}},
		{"wrong boundary", []RedundancyActivationProof{{SourceFileID: 2, Generation: 1, WinnerSourceFileID: 1, WinnerGeneration: 1, RequiredThroughOffset: 127}}},
		{"duplicate", []RedundancyActivationProof{proof, proof}},
	} {
		t.Run(test.name, func(t *testing.T) {
			err := h.run.Storage().Write(func(tx *source.WriteTx) error {
				if test.proofs == nil {
					_, err := Activate(tx, epoch, 1, testComparator)
					return err
				}
				_, err := c.ActivateWithRedundancyProofs(tx, epoch, 1, test.proofs)
				return err
			})
			if !errors.Is(err, ErrActivationBlocked) {
				t.Fatalf("error=%v", err)
			}
			assertBuildRetained(t, h, epoch)
		})
	}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := newTestCoordinator(t, nil).ActivateWithRedundancyProofs(tx, epoch, 1, []RedundancyActivationProof{proof})
		return err
	}); err != nil {
		t.Fatal(err)
	}
	var states int64
	if err := h.run.Storage().PrivateRead(func(r storage.PrivateReader) error {
		return r.QueryRow("SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=2", epoch).Scan(&states)
	}); err != nil {
		t.Fatal(err)
	}
	if states != 0 {
		t.Fatal("redundant activation synthesized contributor state")
	}
}

func TestQuarantineActivationAllowsMissingMemberWithoutCurrentProof(t *testing.T) {
	h, c, epoch := seedQuarantinedBuild(t, true)
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error { _, err := c.Activate(tx, epoch, 1); return err }); err != nil {
		t.Fatal(err)
	}
	state, _, err := h.run.Storage().LoadUsageEpoch()
	if err != nil || state.ActiveEpoch != epoch || state.BuildEpoch != nil {
		t.Fatalf("state=%+v error=%v", state, err)
	}
}

func TestQuarantineNoLeakCoversIndependentRootAttributionAndRollsBack(t *testing.T) {
	cases := []struct{ name, sql, want string }{
		{"state", `INSERT INTO codex_usage_source_states(ledger_epoch,source_file_id,file_generation,device_id,inode,usage_parser_version,canonical_algorithm_version,resolved_through_offset,observed_raw_size,raw_tail_status,owning_thread_id,root_session_id,continuation_state,chain_state,updated_at_ms) VALUES(?,2,1,12,102,1,1,64,64,'none','root-a','root-a','owning_live','continuous',1)`, "source-state root attribution"},
		{"skill", `INSERT INTO codex_skill_usage_events(ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,occurred_at_ms,thread_id,root_session_id,skill_name,created_at_ms) VALUES(?,2,1,0,8,1,'root-a','root-a','leak',1)`, "skill root attribution"},
		{"marker", `INSERT INTO codex_compaction_markers(ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,root_session_id,unknown_reason) VALUES(?,2,1,0,8,'root-a','root-a','usage_missing')`, "marker root attribution"},
		{"window", `INSERT INTO codex_usage_reconciliation_windows(ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,state_json) VALUES(?,2,1,0,8,'root-a','{}')`, "window root attribution"},
		{"turn", `INSERT INTO codex_turns(ledger_epoch,source_file_id,file_generation,turn_key,thread_id,start_offset,status,accounted_input_tokens,accounted_cached_tokens,accounted_output_tokens,accounted_reasoning_tokens,accounted_total_tokens,accounted_fingerprint,accounted_candidate_count,model_state,unresolved_model_seen,reasoning_effort_state,unresolved_reasoning_effort_seen,compensation_allowed,block_start_missing,block_time_missing,block_reset,block_ownership_gap,block_parser_gap,block_required_invalid,block_model_unresolved,quality_status,state_through_offset,updated_at_ms) VALUES(?,2,1,'leak-turn','root-a',0,'open',0,0,0,0,0,X'00',0,'none',0,'none',0,1,0,0,0,0,0,0,0,'complete',0,1)`, "turn root attribution"},
		{"other generation of manifest source", `INSERT INTO codex_skill_usage_events(ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,occurred_at_ms,thread_id,root_session_id,skill_name,created_at_ms) VALUES(?,1,2,0,8,1,'root-b','root-b','leak',1)`, "manifest sources"},
		{"fact with canonical in another root", `INSERT INTO codex_usage_event_facts(ledger_epoch,event_id,owning_thread_id,evidence_kind,operation) VALUES(?,'other-root-event','root-a','legacy','response')`, "facts by owning thread"},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			h, c, epoch := seedQuarantinedBuild(t, false)
			if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
				if test.name == "fact with canonical in another root" {
					if _, err := tx.WriteUsageNoRevision(source.UsageTargetBuild, testCanonicalEvent("other-root-event", "root-a")); err != nil {
						return err
					}
				}
				return tx.Private(func(p storage.PrivateTx) error {
					if _, err := p.Exec(test.sql, epoch); err != nil {
						return err
					}
					if test.name == "fact with canonical in another root" {
						_, err := p.Exec("UPDATE usage_events SET thread_id='root-b',root_session_id='root-b' WHERE source='codex' AND source_epoch=? AND event_id='other-root-event'", epoch)
						return err
					}
					return nil
				})
			}); err != nil {
				t.Fatal(err)
			}
			before := testDataRevision(t, h)
			err := h.run.Storage().Write(func(tx *source.WriteTx) error {
				if err := tx.Private(func(p storage.PrivateTx) error {
					_, err := p.Exec("UPDATE codex_usage_session_quarantine SET last_activity_at_ms=99 WHERE ledger_epoch=?", epoch)
					return err
				}); err != nil {
					return err
				}
				_, err := c.Activate(tx, epoch, 1)
				return err
			})
			if !errors.Is(err, ErrActivationBlocked) || !strings.Contains(err.Error(), test.want) {
				t.Fatalf("error=%v want %s", err, test.want)
			}
			assertBuildRetained(t, h, epoch)
			var activity int64
			if err := h.run.Storage().PrivateRead(func(r storage.PrivateReader) error {
				return r.QueryRow("SELECT last_activity_at_ms FROM codex_usage_session_quarantine WHERE ledger_epoch=?", epoch).Scan(&activity)
			}); err != nil {
				t.Fatal(err)
			}
			if activity != 1 || testDataRevision(t, h) != before {
				t.Fatal("rejected no-leak activation did not rollback")
			}
		})
	}
}

func seedQuarantinedBuild(t *testing.T, missingMember bool) (*rebuildHarness, *Coordinator, int64) {
	t.Helper()
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-a")
	h.seedThread(t, "root-b")
	h.insertSource(t, 1, "/sessions/a.jsonl", "root-a", 1, 11, 101, 128, "present")
	h.insertSource(t, 2, "/historical/b.jsonl", "root-a", 1, 12, 102, 64, "missing")
	if missingMember {
		h.seedActiveSource(t, 2, 1, 12, 102, 1, 64, 64, "/historical/b.jsonl", "root-a", "none", nil, []byte("guard"))
	}
	c := newTestCoordinator(t, nil)
	var epoch int64
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		epoch, err = c.BeginOrResume(tx, Bootstrap, 1, []MemberRequirement{requirement(1, 1, 11, 101, 128, 128, "root-a", "root-a")}, nil, 1)
		if err != nil {
			return err
		}
		if err := RecordQuarantined(tx, epoch, 1, "FATAL", 1); err != nil {
			return err
		}
		if missingMember {
			if err := RecordQuarantined(tx, epoch, 2, "FATAL", 1); err != nil {
				return err
			}
		}
		return tx.Private(func(p storage.PrivateTx) error {
			if _, err := p.Exec("INSERT INTO codex_usage_session_quarantine(ledger_epoch,root_session_id,primary_error_code,last_activity_at_ms,first_seen_at_ms,updated_at_ms) VALUES(?,'root-a','FATAL',1,1,1)", epoch); err != nil {
				return err
			}
			_, err := p.Exec("INSERT INTO codex_usage_session_quarantine_sources(ledger_epoch,root_session_id,source_file_id,file_generation,device_id,inode,observed_size,updated_at_ms) VALUES(?,'root-a',1,1,11,101,128,1)", epoch)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	return h, c, epoch
}

func assertBuildRetained(t *testing.T, h *rebuildHarness, epoch int64) {
	t.Helper()
	state, _, err := h.run.Storage().LoadUsageEpoch()
	if err != nil || state.BuildEpoch == nil || *state.BuildEpoch != epoch || state.ActiveEpoch == epoch {
		t.Fatalf("activation failure lost build: state=%+v error=%v", state, err)
	}
}
func testDataRevision(t *testing.T, h *rebuildHarness) int64 {
	t.Helper()
	var revision int64
	if err := h.run.Storage().PrivateRead(func(r storage.PrivateReader) error {
		return r.QueryRow("SELECT data_revision FROM app_meta WHERE id=1").Scan(&revision)
	}); err != nil {
		t.Fatal(err)
	}
	return revision
}

func TestQuarantineActivationRejectsIncompleteStaleOrPartialRootProof(t *testing.T) {
	for _, test := range []struct{ name, sql string }{
		{"missing present proof", "DELETE FROM codex_usage_session_quarantine_sources WHERE ledger_epoch=?"},
		{"extra missing historical proof", "INSERT INTO codex_usage_session_quarantine_sources VALUES(?,'root-a',2,1,12,102,64,1)"},
		{"stale physical proof", "UPDATE codex_usage_session_quarantine_sources SET observed_size=127 WHERE ledger_epoch=?"},
		{"partial root terminal", "UPDATE codex_usage_build_sources SET completion_status='pending',completion_error_code=NULL WHERE build_epoch=? AND source_file_id=2"},
		{"error code mismatch", "UPDATE codex_usage_session_quarantine SET primary_error_code='OTHER_FATAL' WHERE ledger_epoch=?"},
	} {
		t.Run(test.name, func(t *testing.T) {
			h, c, epoch := seedQuarantinedBuild(t, true)
			err := h.run.Storage().Write(func(tx *source.WriteTx) error {
				if err := tx.Private(func(p storage.PrivateTx) error { _, err := p.Exec(test.sql, epoch); return err }); err != nil {
					return err
				}
				_, err := c.Activate(tx, epoch, 1)
				return err
			})
			if !errors.Is(err, ErrActivationBlocked) {
				t.Fatalf("error=%v", err)
			}
			assertBuildRetained(t, h, epoch)
			for _, m := range loadTestManifest(t, h, epoch) {
				if m.completion != completionQuarantined {
					t.Fatal("failed activation mutation persisted")
				}
			}
		})
	}
}

func TestCanonicalDifferenceActivatesAndBumpsRevisionOnce(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-z")
	h.insertSource(t, 1, "/sessions/z.jsonl", "root-z", 1, 11, 101, 128, "present")
	c := newTestCoordinator(t, nil)
	var epoch int64
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		epoch, err = c.BeginOrResume(tx, FatalIsolation, 1, []MemberRequirement{requirement(1, 1, 11, 101, 128, 128, "root-z", "root-z")}, []int64{1}, 1)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	assertBuildRetained(t, h, epoch)
	seedBuildStateAndCheckpoint(t, h, epoch, 1, 128, 128, "none", nil, []byte("guard"))
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetBuild, testCanonicalEvent("new-event", "root-z")); err != nil {
			return err
		}
		return RecordRebuilt(tx, epoch, 1, false, 2)
	}); err != nil {
		t.Fatal(err)
	}
	before := testDataRevision(t, h)
	var outcome source.UsageActivationOutcome
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error { var err error; outcome, err = c.Activate(tx, epoch, 1); return err }); err != nil {
		t.Fatal(err)
	}
	if !outcome.VisibleChanged || outcome.DataRevision != before+1 {
		t.Fatalf("canonical difference outcome=%+v", outcome)
	}
}
