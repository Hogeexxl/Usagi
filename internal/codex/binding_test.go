package codex

import (
	"context"
	"database/sql"
	"errors"
	"path/filepath"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

func TestBindingFirstBindAndSameHome(t *testing.T) {
	db, bound := newCodexTestStorage(t)
	if err := bound.Write(func(tx *source.WriteTx) error {
		outcome, err := bindOrValidate(tx, "fingerprint-a")
		if err != nil {
			return err
		}
		if outcome != BindingBoundNow {
			t.Fatalf("first outcome = %d", outcome)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().StatusRevision; got != 1 {
		t.Fatalf("status revision = %d, want 1", got)
	}
	if err := bound.Write(func(tx *source.WriteTx) error {
		outcome, err := bindOrValidate(tx, "fingerprint-a")
		if err == nil && outcome != BindingReady {
			t.Fatalf("same-home outcome = %d", outcome)
		}
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().StatusRevision; got != 1 {
		t.Fatalf("same-home changed status revision to %d", got)
	}
}

func TestBindingSourceChangePreservesOldFingerprintAndLatches(t *testing.T) {
	db, bound := newCodexTestStorage(t)
	bindCodexHome(t, bound, "fingerprint-a")
	for range 2 {
		if err := bound.Write(func(tx *source.WriteTx) error {
			outcome, err := bindOrValidate(tx, "fingerprint-b")
			if err == nil && outcome != BindingSourceChanged {
				t.Fatalf("source-change outcome = %d", outcome)
			}
			return err
		}); err != nil {
			t.Fatal(err)
		}
	}
	var fingerprint, status string
	if err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow("SELECT home_fingerprint,binding_status FROM codex_adapter_state WHERE id=1").Scan(&fingerprint, &status)
	}); err != nil {
		t.Fatal(err)
	}
	if fingerprint != "fingerprint-a" || status != "source_changed" {
		t.Fatalf("binding = %q/%q", fingerprint, status)
	}
	if got := db.CurrentRevision().StatusRevision; got != 2 {
		t.Fatalf("status revision = %d, want 2", got)
	}
}

func TestBindingSourceChangeRollbackRestoresStateAndRevision(t *testing.T) {
	db, bound := newCodexTestStorage(t)
	bindCodexHome(t, bound, "fingerprint-a")
	rollback := errors.New("rollback")
	if err := bound.Write(func(tx *source.WriteTx) error {
		outcome, err := bindOrValidate(tx, "fingerprint-b")
		if err != nil {
			return err
		}
		if outcome != BindingSourceChanged {
			t.Fatalf("outcome = %d", outcome)
		}
		return rollback
	}); !errors.Is(err, rollback) {
		t.Fatalf("Write() error = %v", err)
	}
	var fingerprint, status string
	if err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow("SELECT home_fingerprint,binding_status FROM codex_adapter_state WHERE id=1").Scan(&fingerprint, &status)
	}); err != nil {
		t.Fatal(err)
	}
	if fingerprint != "fingerprint-a" || status != "ready" || db.CurrentRevision().StatusRevision != 1 {
		t.Fatalf("rollback state = %q/%q revision=%d", fingerprint, status, db.CurrentRevision().StatusRevision)
	}
}

func TestBindingMutationRollsBackWithWriteTx(t *testing.T) {
	db, bound := newCodexTestStorage(t)
	rollback := errors.New("rollback")
	if err := bound.Write(func(tx *source.WriteTx) error {
		if _, err := bindOrValidate(tx, "fingerprint-a"); err != nil {
			return err
		}
		return rollback
	}); !errors.Is(err, rollback) {
		t.Fatalf("Write() error = %v", err)
	}
	var fingerprint sql.NullString
	var status string
	if err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow("SELECT home_fingerprint,binding_status FROM codex_adapter_state WHERE id=1").Scan(&fingerprint, &status)
	}); err != nil {
		t.Fatal(err)
	}
	if fingerprint.Valid || status != "unbound" || db.CurrentRevision().StatusRevision != 0 {
		t.Fatalf("rollback state = %+v/%q revision=%d", fingerprint, status, db.CurrentRevision().StatusRevision)
	}
}

func bindCodexHome(t *testing.T, bound *source.Storage, fingerprint string) {
	t.Helper()
	if err := bound.Write(func(tx *source.WriteTx) error {
		_, err := bindOrValidate(tx, fingerprint)
		return err
	}); err != nil {
		t.Fatal(err)
	}
}

func newCodexTestStorage(t *testing.T) (*storage.DB, *source.Storage) {
	t.Helper()
	db, err := storage.Open(context.Background(), storage.Config{Path: filepath.Join(t.TempDir(), "usagi.sqlite3")})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := db.Close(); err != nil {
			t.Error(err)
		}
	})
	descriptor, err := source.NewDescriptor(domain.SourceCodex, "Codex")
	if err != nil {
		t.Fatal(err)
	}
	run, err := source.NewStorageFactory(db).Context(context.Background(), "codex-test", descriptor)
	if err != nil {
		t.Fatal(err)
	}
	return db, run.Storage()
}
