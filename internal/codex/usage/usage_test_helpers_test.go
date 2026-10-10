package usage

import (
	"context"
	"path/filepath"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

func newUsageTestRun(t *testing.T) source.RunContext {
	t.Helper()
	db, err := storage.Open(context.Background(), storage.Config{Path: filepath.Join(t.TempDir(), "usage.sqlite3")})
	if err != nil {
		t.Fatal(err)
	}
	workerCtx, cancel := context.WithCancel(context.Background())
	descriptor, err := source.NewDescriptor(domain.SourceCodex, "Codex")
	if err != nil {
		cancel()
		_ = db.Close()
		t.Fatal(err)
	}
	run, err := source.NewStorageFactory(db).Context(workerCtx, "usage-test", descriptor)
	if err != nil {
		cancel()
		_ = db.Close()
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cancel()
		if err := db.Close(); err != nil {
			t.Error(err)
		}
	})
	return run
}

func seedUsageTestThreads(tx *source.WriteTx, threadIDs ...string) error {
	for _, threadID := range threadIDs {
		identity, err := domain.NewSessionIdentity(threadID, domain.SourceCodex, "native:"+threadID)
		if err != nil {
			return err
		}
		patch, err := domain.NewResolvedThreadPatch(identity, 1)
		if err != nil {
			return err
		}
		patch.RootSessionID = domain.Set(threadID)
		patch.AgentRole = domain.Set(domain.AgentRole("main"))
		if _, err := tx.UpsertThreadNoRevision(identity, patch); err != nil {
			return err
		}
	}
	return nil
}

func insertUsageTestSourceFile(tx *source.WriteTx, path, threadID string) (int64, error) {
	var sourceFileID int64
	err := tx.Private(func(private storage.PrivateTx) error {
		result, err := private.Exec(`INSERT INTO codex_source_files(
			thread_id,current_path,source_area,device_id,inode,file_generation,observed_size,
			observed_mtime_ns,file_status,last_seen_at_ms
		) VALUES(?,?,'sessions',1,(SELECT COALESCE(MAX(inode),0)+1 FROM codex_source_files),1,128,3,'present',4)`, threadID, path)
		if err != nil {
			return err
		}
		sourceFileID, err = result.LastInsertId()
		return err
	})
	return sourceFileID, err
}

func activateUsageTestEpoch(tx *source.WriteTx) error {
	if err := tx.EnsureUsageEpoch(); err != nil {
		return err
	}
	epoch, err := tx.BeginOrResumeUsageBuild(UsageParserVersion)
	if err != nil {
		return err
	}
	_, err = tx.ActivateUsageBuild(epoch, UsageParserVersion)
	return err
}
