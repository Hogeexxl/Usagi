package rebuild

import (
	"errors"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

func TestResetQuarantinedRootExpandsAllMembersAndPreservesOtherRoot(t *testing.T) {
	h, c, epoch := seedQuarantinedBuild(t, true)
	h.insertSource(t, 3, "/sessions/c.jsonl", "root-b", 1, 13, 103, 32, "present")
	reqs := []MemberRequirement{requirement(1, 1, 11, 101, 128, 128, "root-a", "root-a"), requirement(3, 1, 13, 103, 32, 32, "root-b", "root-b")}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		if _, err := c.BeginOrResume(tx, Bootstrap, 1, reqs, nil, 2); err != nil {
			return err
		}
		return tx.Private(func(p storage.PrivateTx) error {
			_, err := p.Exec("UPDATE codex_usage_build_sources SET completion_status='rebuilt',completed_generation=1,completed_through_offset=32 WHERE build_epoch=? AND source_file_id=3", epoch)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return ResetBuildMembersTx(tx, epoch, SourceInvalidated, []int64{1}, nil, reqs, testActiveStateProof, c.stripWindows, 3)
	}); err != nil {
		t.Fatal(err)
	}
	members := loadTestManifest(t, h, epoch)
	for _, m := range members {
		if m.sourceFileID == 3 {
			if m.completion != completionRebuilt || m.completedOffset.Int64 != 32 {
				t.Fatalf("safe sibling reset: %+v", m)
			}
			continue
		}
		if m.completion != completionPending || m.errorCode.Valid || m.completedGeneration.Valid || m.completedOffset.Valid || m.carryFrom.Valid || m.carryPhase != carryNone {
			t.Fatalf("quarantined root not fully reset: %+v", m)
		}
	}
	var count int64
	if err := h.run.Storage().PrivateRead(func(r storage.PrivateReader) error {
		return r.QueryRow("SELECT (SELECT count(*) FROM codex_usage_session_quarantine WHERE ledger_epoch=?)+(SELECT count(*) FROM codex_usage_session_quarantine_sources WHERE ledger_epoch=?)", epoch, epoch).Scan(&count)
	}); err != nil {
		t.Fatal(err)
	}
	if count != 0 {
		t.Fatal("root reset retained quarantine/proof")
	}
}

func TestQuarantineRetryRebuildsWholePresentRootAndBlocksMissingWithoutContributor(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-a")
	h.insertSource(t, 1, "/sessions/a.jsonl", "root-a", 1, 11, 101, 128, "present")
	h.insertSource(t, 2, "/sessions/b.jsonl", "root-a", 1, 12, 102, 64, "present")
	h.insertSource(t, 3, "/historical/c.jsonl", "root-a", 1, 13, 103, 32, "missing")
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return tx.Private(func(p storage.PrivateTx) error {
			if _, err := p.Exec("INSERT INTO codex_usage_session_quarantine VALUES(1,'root-a','FATAL',1,1,1)"); err != nil {
				return err
			}
			for _, row := range []struct{ id, device, inode, size int64 }{{1, 11, 101, 128}, {2, 12, 102, 64}, {3, 13, 103, 32}} {
				if _, err := p.Exec("INSERT INTO codex_usage_session_quarantine_sources VALUES(1,'root-a',?,?,?,?,?,1)", row.id, int64(1), row.device, row.inode, row.size); err != nil {
					return err
				}
			}
			return nil
		})
	}); err != nil {
		t.Fatal(err)
	}
	c := newTestCoordinator(t, nil)
	reqs := []MemberRequirement{requirement(1, 1, 11, 101, 128, 128, "root-a", "root-a"), requirement(2, 1, 12, 102, 64, 64, "root-a", "root-a")}
	var epoch int64
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		epoch, err = c.BeginOrResume(tx, QuarantineRetry, 1, reqs, []int64{1}, 2)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	for _, m := range loadTestManifest(t, h, epoch) {
		if m.sourceFileID == 3 {
			if m.completion != completionBlocked || m.errorCode.String != "QUARANTINE_RETRY_RAW_MISSING" {
				t.Fatalf("missing blocker=%+v", m)
			}
		} else if m.completion != completionPending || m.carryFrom.Valid || m.carryPhase != carryNone {
			t.Fatalf("present not rebuild: %+v", m)
		}
	}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error { return c.BeginCarry(tx, epoch, 2, QuarantineRetry, 3) }); !errors.Is(err, ErrCarryIneligible) {
		t.Fatalf("retry carry error=%v", err)
	}
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error { _, err := c.Activate(tx, epoch, 1); return err }); !errors.Is(err, ErrActivationBlocked) {
		t.Fatalf("blocked activation error=%v", err)
	}
	reqs = append(reqs, requirement(3, 1, 13, 103, 32, 32, "root-a", "root-a"))
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		if err := tx.Private(func(p storage.PrivateTx) error {
			_, err := p.Exec("UPDATE codex_source_files SET file_status='present' WHERE source_file_id=3")
			return err
		}); err != nil {
			return err
		}
		_, err := c.BeginOrResume(tx, QuarantineRetry, 1, reqs, []int64{3}, 4)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	for _, m := range loadTestManifest(t, h, epoch) {
		if m.completion != completionPending || m.errorCode.Valid {
			t.Fatalf("reappearance not pending rebuild: %+v", m)
		}
	}
}

func TestPlanRootReuseProducesNoPerSourceWork(t *testing.T) {
	c := newTestCoordinator(t, nil)
	plans := []MemberBuildPlan{{}}
	plan := c.PlanRoot("root-a", plans, ReuseProofValid)
	if plan.Action != RootReuseQuarantine || len(plan.Work) != 0 || len(plan.Members) != 1 {
		t.Fatalf("reuse plan=%+v", plan)
	}
	plan = c.PlanRoot("root-a", plans, ReuseNotEligible)
	if plan.Action != RootNormalBuild || len(plan.Work) != 1 {
		t.Fatalf("normal plan=%+v", plan)
	}
}

func TestParserReplacementCleansResolvedMarkerAndAllOldTerminalStates(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-a")
	h.insertSource(t, 1, "/sessions/a.jsonl", "root-a", 1, 11, 101, 128, "present")
	h.insertSource(t, 2, "/historical/b.jsonl", "root-a", 1, 12, 102, 64, "missing")
	h.insertSource(t, 3, "/historical/c.jsonl", "root-a", 1, 13, 103, 32, "missing")
	h.seedActiveSource(t, 2, 1, 12, 102, 1, 64, 64, "/historical/b.jsonl", "root-a", "none", nil, []byte("b-guard"))
	h.seedActiveSource(t, 3, 1, 13, 103, 1, 32, 32, "/historical/c.jsonl", "root-a", "none", nil, []byte("c-guard"))
	c := newTestCoordinator(t, nil)
	reqs := []MemberRequirement{requirement(1, 1, 11, 101, 128, 128, "root-a", "root-a")}
	var epoch int64
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		epoch, err = c.BeginOrResume(tx, Bootstrap, 1, reqs, nil, 1)
		if err != nil {
			return err
		}
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetBuild, testCanonicalEvent("bound-event", "root-a")); err != nil {
			return err
		}
		return tx.Private(func(p storage.PrivateTx) error {
			for _, statement := range []string{
				`UPDATE codex_usage_build_sources SET completion_status='rebuilt',completed_generation=1,completed_through_offset=128 WHERE source_file_id=1`,
				`UPDATE codex_usage_build_sources SET completion_status='carried',completed_generation=1,completed_through_offset=64 WHERE source_file_id=2`,
				`UPDATE codex_usage_build_sources SET completion_status='quarantined',completion_error_code='FATAL' WHERE source_file_id=3`,
			} {
				if _, err := p.Exec(statement); err != nil {
					return err
				}
			}
			if _, err := p.Exec("INSERT INTO codex_usage_event_facts VALUES('codex',?,'bound-event','root-a','response','explicit','compaction')", epoch); err != nil {
				return err
			}
			if _, err := p.Exec("INSERT INTO codex_compaction_markers(source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,root_session_id,response_id,resolved_event_id) VALUES('codex',?,1,1,16,24,'root-a','root-a','response','bound-event')", epoch); err != nil {
				return err
			}
			if _, err := p.Exec("INSERT INTO codex_usage_event_occurrences VALUES('codex',?,1,1,0,8,'bound-event',1)", epoch); err != nil {
				return err
			}
			if _, err := p.Exec("INSERT INTO codex_usage_session_quarantine VALUES(?,'root-a','FATAL',1,1,1)", epoch); err != nil {
				return err
			}
			_, err := p.Exec("INSERT INTO codex_usage_session_quarantine_sources VALUES(?,'root-a',1,1,11,101,128,1)", epoch)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	stripErr := errors.New("window strip failed")
	failing := newTestCoordinator(t, func(*source.WriteTx, source.UsageWriteTarget, []string) error { return stripErr })
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := failing.BeginOrResume(tx, ParserChanged, 2, reqs, nil, 2)
		return err
	}); !errors.Is(err, stripErr) {
		t.Fatalf("strip failure=%v", err)
	}
	state, _, err := h.run.Storage().LoadUsageEpoch()
	if err != nil || state.BuildParserVersion == nil || *state.BuildParserVersion != 1 {
		t.Fatalf("retarget did not rollback: %+v error=%v", state, err)
	}
	var count int64
	if err := h.run.Storage().PrivateRead(func(r storage.PrivateReader) error {
		return r.QueryRow("SELECT (SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=?)+(SELECT count(*) FROM codex_usage_event_facts WHERE ledger_epoch=?)+(SELECT count(*) FROM codex_compaction_markers WHERE ledger_epoch=?)+(SELECT count(*) FROM codex_usage_event_occurrences WHERE ledger_epoch=?)", epoch, epoch, epoch, epoch).Scan(&count)
	}); err != nil {
		t.Fatal(err)
	}
	if count != 4 {
		t.Fatalf("cleanup failed to rollback bound semantic rows: count=%d", count)
	}
	reqs = append(reqs, requirement(2, 1, 12, 102, 64, 64, "root-a", "root-a"), requirement(3, 1, 13, 103, 32, 32, "root-a", "root-a"))
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := c.BeginOrResume(tx, ParserChanged, 2, reqs, nil, 3)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	for _, m := range loadTestManifest(t, h, epoch) {
		if m.sourceFileID == 1 {
			if m.completion != completionPending {
				t.Fatalf("present reset=%+v", m)
			}
		} else if m.completion != completionBlocked || m.errorCode.String != "PARSER_CHANGED_RAW_MISSING" {
			t.Fatalf("missing reset=%+v", m)
		}
		if m.completedGeneration.Valid || m.completedOffset.Valid || m.carryFrom.Valid || m.carryPhase != carryNone || m.afterStartOffset.Valid || m.afterTurnKey.Valid || m.afterAnomalyID.Valid || m.afterFactEventID.Valid || m.afterMarkerOffset.Valid || m.afterWindowOffset.Valid {
			t.Fatalf("old progress survived: %+v", m)
		}
	}
	if err := h.run.Storage().PrivateRead(func(r storage.PrivateReader) error {
		return r.QueryRow("SELECT (SELECT count(*) FROM usage_events WHERE source='codex' AND source_epoch=?)+(SELECT count(*) FROM codex_usage_event_facts WHERE ledger_epoch=?)+(SELECT count(*) FROM codex_compaction_markers WHERE ledger_epoch=?)+(SELECT count(*) FROM codex_usage_event_occurrences WHERE ledger_epoch=?)+(SELECT count(*) FROM codex_usage_session_quarantine WHERE ledger_epoch=?)+(SELECT count(*) FROM codex_usage_session_quarantine_sources WHERE ledger_epoch=?)", epoch, epoch, epoch, epoch, epoch, epoch).Scan(&count)
	}); err != nil {
		t.Fatal(err)
	}
	if count != 0 {
		t.Fatalf("old parser semantic/quarantine survived: %d", count)
	}
}

func TestActiveStateProofCallbackErrorRollsBackBuild(t *testing.T) {
	h := newRebuildHarness(t)
	h.seedActiveEpoch(t, 1)
	h.seedThread(t, "root-a")
	h.insertSource(t, 1, "/sessions/a.jsonl", "root-a", 1, 11, 101, 128, "present")
	h.seedActiveSource(t, 1, 1, 11, 101, 1, 128, 128, "/sessions/a.jsonl", "root-a", "none", nil, []byte("guard"))
	proofErr := errors.New("active proof failed")
	c, err := NewCoordinator(testComparator, func(*source.WriteTx, int64, int64) ([]byte, error) { return nil, proofErr }, func(*source.WriteTx, source.UsageWriteTarget, []string) error { return nil })
	if err != nil {
		t.Fatal(err)
	}
	err = h.run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := c.BeginOrResume(tx, Bootstrap, 1, []MemberRequirement{requirement(1, 1, 11, 101, 128, 128, "root-a", "root-a")}, nil, 1)
		return err
	})
	if !errors.Is(err, proofErr) {
		t.Fatalf("error=%v", err)
	}
	state, _, err := h.run.Storage().LoadUsageEpoch()
	if err != nil || state.BuildEpoch != nil || state.ActiveEpoch != 1 {
		t.Fatalf("proof failure left build: %+v error=%v", state, err)
	}
	assertCheckpoint(t, h, 1, 1, 128, []byte("guard"), "ready")
}
