package rebuild

import (
	"context"
	"database/sql"
	"fmt"
	"path/filepath"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

type rebuildHarness struct {
	run source.RunContext
}

func newRebuildHarness(t *testing.T) *rebuildHarness {
	t.Helper()
	ctx := context.Background()
	db, err := storage.Open(ctx, storage.Config{Path: filepath.Join(t.TempDir(), "rebuild.sqlite3")})
	if err != nil {
		t.Fatal(err)
	}
	descriptor, err := source.NewDescriptor(domain.SourceCodex, "Codex")
	if err != nil {
		_ = db.Close()
		t.Fatal(err)
	}
	run, err := source.NewStorageFactory(db).Context(ctx, "rebuild-test", descriptor)
	if err != nil {
		_ = db.Close()
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := db.Close(); err != nil {
			t.Error(err)
		}
	})
	return &rebuildHarness{run: run}
}

func (h *rebuildHarness) seedActiveEpoch(t *testing.T, parserVersion int64) {
	t.Helper()
	err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		if err := tx.EnsureUsageEpoch(); err != nil {
			return err
		}
		epoch, err := tx.BeginOrResumeUsageBuild(parserVersion)
		if err != nil {
			return err
		}
		_, err = tx.ActivateUsageBuild(epoch, parserVersion)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
}

func (h *rebuildHarness) seedThread(t *testing.T, threadID string) {
	t.Helper()
	identity, err := domain.NewSessionIdentity(threadID, domain.SourceCodex, "native:"+threadID)
	if err != nil {
		t.Fatal(err)
	}
	patch, err := domain.NewResolvedThreadPatch(identity, 1)
	if err != nil {
		t.Fatal(err)
	}
	patch.RootSessionID = domain.Set(threadID)
	patch.AgentRole = domain.Set(domain.AgentRole("main"))
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		_, err := tx.UpsertThreadNoRevision(identity, patch)
		return err
	}); err != nil {
		t.Fatal(err)
	}
}

func (h *rebuildHarness) insertSource(t *testing.T, id int64, path, owner string, generation, device, inode, observed int64, status string) {
	t.Helper()
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`INSERT INTO codex_source_files(
				source_file_id,thread_id,current_path,source_area,device_id,inode,file_generation,observed_size,
				observed_mtime_ns,file_status,last_seen_at_ms
			) VALUES(?,?,?,'sessions',?,?,?,?,3,?,4)`, id, owner, path, device, inode, generation, observed, status)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
}

func (h *rebuildHarness) seedActiveSource(
	t *testing.T,
	id, generation, device, inode, parser, offset, observed int64,
	path, owner, tailStatus string,
	tailStart *int64,
	guard []byte,
) {
	t.Helper()
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`INSERT INTO codex_source_checkpoints(
				source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,processing_status,last_successful_scan_at_ms,last_error_code
			) VALUES(?,'usage',?,?,?,'ready',1,NULL)`, id, parser, offset, guard); err != nil {
				return err
			}
			_, err := private.Exec(`INSERT INTO codex_usage_source_states(
				ledger_epoch,source_file_id,file_generation,device_id,inode,usage_parser_version,canonical_algorithm_version,
				resolved_through_offset,observed_raw_size,raw_tail_status,raw_tail_start_offset,owning_thread_id,root_session_id,
				continuation_state,chain_state,updated_at_ms
			) VALUES(1,?,?,?,?,?,1,?,?,?,?,?,?,'owning_live','continuous',1)`,
				id, generation, device, inode, parser, offset, observed, tailStatus, tailStart, owner, owner)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
}

func requirement(id, generation, device, inode, required, observed int64, owner, root string) MemberRequirement {
	return MemberRequirement{
		SourceFileID: id, Generation: generation, DeviceID: device, Inode: inode,
		RequiredThroughOffset: required, ObservedRawSize: observed,
		ExpectedOwningThreadID: &owner, ExpectedRootSessionID: &root,
	}
}

func testActiveStateProof(tx *source.WriteTx, activeEpoch, sourceFileID int64) ([]byte, error) {
	var generation, device, inode, parser, resolved, observed int64
	var tail, owner, root string
	found := false
	err := tx.Private(func(private storage.PrivateTx) error {
		err := private.QueryRow(`SELECT file_generation,device_id,inode,usage_parser_version,resolved_through_offset,
			observed_raw_size,raw_tail_status,owning_thread_id,root_session_id FROM codex_usage_source_states
			WHERE ledger_epoch=? AND source_file_id=?`, activeEpoch, sourceFileID).
			Scan(&generation, &device, &inode, &parser, &resolved, &observed, &tail, &owner, &root)
		if err == sql.ErrNoRows {
			return nil
		}
		if err != nil {
			return err
		}
		found = true
		return nil
	})
	if err != nil || !found {
		return nil, err
	}
	return []byte(fmt.Sprintf("%d/%d/%d/%d/%d/%d/%s/%s/%s", generation, device, inode, parser, resolved, observed, tail, owner, root)), nil
}

func testComparator(storage.PrivateTx, domain.SourceID, int64, int64, int64, int64) (bool, error) {
	return true, nil
}

func newTestCoordinator(t *testing.T, stripper WindowReferenceStripper) *Coordinator {
	t.Helper()
	if stripper == nil {
		stripper = func(*source.WriteTx, source.UsageWriteTarget, []string) error { return nil }
	}
	coordinator, err := NewCoordinator(testComparator, testActiveStateProof, stripper)
	if err != nil {
		t.Fatal(err)
	}
	return coordinator
}

func loadTestManifest(t *testing.T, h *rebuildHarness, epoch int64) []manifestMember {
	t.Helper()
	var members []manifestMember
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		members, err = loadManifestMembers(tx, epoch)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	return members
}

func updateCheckpoint(t *testing.T, h *rebuildHarness, id int64, parser, offset int64, guard []byte, status string) {
	t.Helper()
	if err := h.run.Storage().Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_source_checkpoints SET parser_version=?,committed_offset=?,guard_hash=?,processing_status=?
				WHERE source_file_id=? AND consumer_kind='usage'`, parser, offset, guard, status, id)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
}

func assertCheckpoint(t *testing.T, h *rebuildHarness, id int64, parser, offset int64, guard []byte, status string) {
	t.Helper()
	var gotParser, gotOffset int64
	var gotGuard []byte
	var gotStatus string
	err := h.run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow(`SELECT parser_version,committed_offset,guard_hash,processing_status FROM codex_source_checkpoints
			WHERE source_file_id=? AND consumer_kind='usage'`, id).Scan(&gotParser, &gotOffset, &gotGuard, &gotStatus)
	})
	if err != nil {
		t.Fatal(err)
	}
	if gotParser != parser || gotOffset != offset || !bytesEqualNil(gotGuard, guard) || gotStatus != status {
		t.Fatalf("checkpoint=(%d,%d,%v,%s), want (%d,%d,%v,%s)", gotParser, gotOffset, gotGuard, gotStatus, parser, offset, guard, status)
	}
}

func bytesEqualNil(left, right []byte) bool {
	if len(left) == 0 && len(right) == 0 {
		return true
	}
	return string(left) == string(right)
}

func writeCanonicalBuildEvent(tx *source.WriteTx, eventID, threadID string) error {
	_, err := tx.WriteUsageNoRevision(source.UsageTargetBuild, testCanonicalEvent(eventID, threadID))
	return err
}
