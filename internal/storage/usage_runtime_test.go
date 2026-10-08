package storage

import (
	"context"
	"database/sql"
	"errors"
	"reflect"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/domain"
)

func TestPrivateTxHasNoTransactionControl(t *testing.T) {
	privateTxType := reflect.TypeOf((*PrivateTx)(nil)).Elem()
	if privateTxType.NumMethod() != 3 {
		t.Fatalf("PrivateTx method count = %d; want Exec, Query, QueryRow only", privateTxType.NumMethod())
	}
	for _, method := range []string{"Exec", "Query", "QueryRow"} {
		if _, ok := privateTxType.MethodByName(method); !ok {
			t.Errorf("PrivateTx is missing %s", method)
		}
	}
	for _, method := range []string{"Commit", "Rollback", "ExecContext"} {
		if _, ok := privateTxType.MethodByName(method); ok {
			t.Errorf("PrivateTx exposes forbidden method %s", method)
		}
	}
}

func TestNilCallbacksRejected(t *testing.T) {
	db := openCanonicalTestDB(t)
	ctx := context.Background()
	if err := db.PrivateRead(ctx, nil); !errors.Is(err, ErrNilPrivateCallback) {
		t.Fatalf("DB.PrivateRead(nil) = %v", err)
	}
	if err := db.WriteTx(ctx, func(tx *Tx) error {
		if err := tx.Private(ctx, nil); !errors.Is(err, ErrNilPrivateCallback) {
			t.Fatalf("Tx.Private(nil) = %v", err)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestPrivateReadCannotWrite(t *testing.T) {
	db := openCanonicalTestDB(t)
	ctx := context.Background()
	err := db.PrivateRead(ctx, func(reader PrivateReader) error {
		var id int64
		return reader.QueryRow(
			"UPDATE codex_adapter_state SET home_fingerprint='forbidden',binding_status='ready' WHERE id=1 RETURNING id",
		).Scan(&id)
	})
	if err == nil {
		t.Fatal("PrivateRead allowed a durable mutation")
	}
	var binding, fingerprint sql.NullString
	if err := db.readers.QueryRowContext(
		ctx,
		"SELECT binding_status,home_fingerprint FROM codex_adapter_state WHERE id=1",
	).Scan(&binding, &fingerprint); err != nil {
		t.Fatal(err)
	}
	if binding.String != "unbound" || fingerprint.Valid {
		t.Fatalf("PrivateRead changed state: binding=%+v fingerprint=%+v", binding, fingerprint)
	}
}

func TestUsageActivationCanonicalProjection(t *testing.T) {
	ctx := context.Background()
	db := canonicalUsageDB(t)
	active := canonicalUsageEvent()
	active.EstimatedCostNanosUSD = canonicalIntPointer(4)
	active.CreatedAtMS = 10
	build := active
	build.CreatedAtMS = 999
	if err := db.WriteTx(ctx, func(tx *Tx) error {
		if _, err := tx.WriteUsageEvent(ctx, domain.SourceCodex, 1, active); err != nil {
			return err
		}
		if _, err := tx.WriteUsageEvent(ctx, domain.SourceCodex, 2, build); err != nil {
			return err
		}
		equal, err := tx.CanonicalUsageProjectionEqual(ctx, domain.SourceCodex, 1, 2)
		if err != nil {
			return err
		}
		if !equal {
			t.Fatal("CreatedAt-only difference changed the visibility projection")
		}
		extra := build
		extra.EventID = "extra-event"
		extra.Model = "different"
		if _, err := tx.WriteUsageEvent(ctx, domain.SourceCodex, 2, extra); err != nil {
			return err
		}
		equal, err = tx.CanonicalUsageProjectionEqual(ctx, domain.SourceCodex, 1, 2)
		if err != nil {
			return err
		}
		if equal {
			t.Fatal("Model difference was omitted from the visibility projection")
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestUsageProjectionIncludesEstimatedCost(t *testing.T) {
	ctx := context.Background()
	db := canonicalUsageDB(t)
	active := canonicalUsageEvent()
	active.EstimatedCostNanosUSD = canonicalIntPointer(4)
	build := active
	build.EstimatedCostNanosUSD = canonicalIntPointer(5)
	if err := db.WriteTx(ctx, func(tx *Tx) error {
		if _, err := tx.WriteUsageEvent(ctx, domain.SourceCodex, 1, active); err != nil {
			return err
		}
		if _, err := tx.WriteUsageEvent(ctx, domain.SourceCodex, 2, build); err != nil {
			return err
		}
		equal, err := tx.CanonicalUsageProjectionEqual(ctx, domain.SourceCodex, 1, 2)
		if err != nil {
			return err
		}
		if equal {
			t.Fatal("EstimatedCost-only difference was omitted from the visibility projection")
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestUsageProjectionAllowsActiveEpochZero(t *testing.T) {
	ctx := context.Background()
	db := canonicalUsageDB(t)
	event := canonicalUsageEvent()
	if err := db.WriteTx(ctx, func(tx *Tx) error {
		if _, err := tx.WriteUsageEvent(ctx, domain.SourceCodex, 1, event); err != nil {
			return err
		}
		equal, err := tx.CanonicalUsageProjectionEqual(ctx, domain.SourceCodex, 0, 1)
		if err != nil {
			return err
		}
		if equal {
			t.Fatal("empty epoch zero compared equal to non-empty build")
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestUsageRuntimeCopyDeleteAndRebindPrimitives(t *testing.T) {
	ctx := context.Background()
	db := canonicalUsageDB(t)
	event := canonicalUsageEvent()
	if err := db.WriteTx(ctx, func(tx *Tx) error {
		if _, err := tx.WriteUsageEvent(ctx, domain.SourceCodex, 1, event); err != nil {
			return err
		}
		outcome, err := tx.CopyUsageEvent(ctx, domain.SourceCodex, 1, 2, event.EventID)
		if err != nil {
			return err
		}
		if outcome != UsageInserted {
			t.Fatalf("copy outcome = %d; want Inserted", outcome)
		}
		outcome, err = tx.CopyUsageEvent(ctx, domain.SourceCodex, 1, 2, event.EventID)
		if err != nil {
			return err
		}
		if outcome != UsageDuplicate {
			t.Fatalf("duplicate copy outcome = %d; want Duplicate", outcome)
		}
		if _, err := tx.CopyUsageEvent(ctx, domain.SourceCodex, 1, 1, event.EventID); err == nil {
			t.Fatal("copy accepted identical source and destination epochs")
		}
		rebound, err := tx.RebindUsageRoot(ctx, domain.SourceCodex, 2, event.ThreadID, event.RootSessionID)
		if err != nil {
			return err
		}
		if rebound != 1 {
			t.Fatalf("same-root Rebind rows affected = %d; want Rust primitive outcome 1", rebound)
		}
		deleted, err := tx.DeleteUsageEvents(ctx, domain.SourceCodex, 2, []string{event.EventID})
		if err != nil {
			return err
		}
		if deleted != 1 {
			t.Fatalf("delete count = %d", deleted)
		}
		deleted, err = tx.DeleteUsageEvents(ctx, domain.SourceCodex, 2, nil)
		if err != nil || deleted != 0 {
			t.Fatalf("empty delete = %d, %v", deleted, err)
		}
		if _, err := tx.CopyUsageEvent(ctx, domain.SourceCodex, 1, 2, "missing"); err == nil {
			t.Fatal("copy accepted a missing source event")
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}
