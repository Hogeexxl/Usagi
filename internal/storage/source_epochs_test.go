package storage

import (
	"context"
	"errors"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/domain"
)

func TestEnsureUsageEpochInTransaction(t *testing.T) {
	db := openCanonicalTestDB(t)
	ctx := context.Background()
	source := domain.SourceID("epoch-test")
	if _, found, err := db.GetSourceUsageEpoch(ctx, source); err != nil || found {
		t.Fatalf("initial epoch lookup: found=%t err=%v", found, err)
	}
	callbackErr := errors.New("rollback epoch ensure")
	err := db.WriteTx(ctx, func(tx *Tx) error {
		if err := tx.EnsureSourceUsageEpoch(ctx, source); err != nil {
			return err
		}
		state, err := tx.SourceUsageEpoch(ctx, source)
		if err != nil {
			return err
		}
		if state.Source != source || state.ActiveEpoch != 0 || state.BuildEpoch != nil ||
			state.ActiveParserVersion != 0 || state.BuildParserVersion != nil {
			t.Fatalf("initial epoch state = %+v", state)
		}
		return callbackErr
	})
	if !errors.Is(err, callbackErr) {
		t.Fatalf("WriteTx error = %v; want callback error", err)
	}
	if _, found, err := db.GetSourceUsageEpoch(ctx, source); err != nil || found {
		t.Fatalf("rolled back epoch is visible: found=%t err=%v", found, err)
	}
	if err := db.WriteTx(ctx, func(tx *Tx) error {
		return tx.EnsureSourceUsageEpoch(ctx, source)
	}); err != nil {
		t.Fatal(err)
	}
	state, found, err := db.GetSourceUsageEpoch(ctx, source)
	if err != nil || !found || state.Source != source || state.ActiveEpoch != 0 ||
		state.BuildEpoch != nil || state.ActiveParserVersion != 0 || state.BuildParserVersion != nil {
		t.Fatalf("committed epoch state = %+v found=%t err=%v", state, found, err)
	}
}

func TestBeginOrResumeUsageBuild(t *testing.T) {
	db := openCanonicalTestDB(t)
	ctx := context.Background()
	source := domain.SourceID("build-test")
	err := db.WriteTx(ctx, func(tx *Tx) error {
		if _, err := tx.BeginOrResumeSourceUsageBuild(ctx, source, -1); err == nil {
			t.Fatal("negative parser version was accepted")
		}
		var count int
		if err := tx.tx.QueryRowContext(ctx, "SELECT count(*) FROM source_usage_epochs WHERE source=?", source).Scan(&count); err != nil {
			return err
		}
		if count != 0 {
			t.Fatalf("negative parser validation created %d Epoch rows", count)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, found, err := db.GetSourceUsageEpoch(ctx, source); err != nil || found {
		t.Fatalf("negative parser call ensured an Epoch: found=%t err=%v", found, err)
	}

	err = db.WriteTx(ctx, func(tx *Tx) error {
		first, err := tx.BeginOrResumeSourceUsageBuild(ctx, source, 7)
		if err != nil {
			return err
		}
		if first != 1 {
			t.Fatalf("new build epoch = %d; want 1", first)
		}
		resumed, err := tx.BeginOrResumeSourceUsageBuild(ctx, source, 7)
		if err != nil {
			return err
		}
		if resumed != first {
			t.Fatalf("resumed build epoch = %d; want %d", resumed, first)
		}
		if _, err := tx.BeginOrResumeSourceUsageBuild(ctx, source, 8); err == nil {
			t.Fatal("BeginOrResume replaced a build with a different parser version")
		}
		state, err := tx.SourceUsageEpoch(ctx, source)
		if err != nil {
			return err
		}
		if state.BuildEpoch == nil || *state.BuildEpoch != 1 ||
			state.BuildParserVersion == nil || *state.BuildParserVersion != 7 {
			t.Fatalf("different parser changed Build Pair: %+v", state)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}

func TestRetargetUsageBuildCAS(t *testing.T) {
	db := openCanonicalTestDB(t)
	ctx := context.Background()
	source := domain.SourceID("retarget-test")
	err := db.WriteTx(ctx, func(tx *Tx) error {
		epoch, err := tx.BeginOrResumeSourceUsageBuild(ctx, source, 3)
		if err != nil {
			return err
		}
		if epoch != 1 {
			t.Fatalf("build epoch = %d", epoch)
		}
		if err := tx.RetargetSourceUsageBuild(ctx, source, 9, 3, 4); err == nil {
			t.Fatal("Retarget accepted a mismatched expected epoch")
		}
		if err := tx.RetargetSourceUsageBuild(ctx, source, 1, 8, 4); err == nil {
			t.Fatal("Retarget accepted a mismatched expected parser version")
		}
		if err := tx.RetargetSourceUsageBuild(ctx, source, 1, 3, 4); err != nil {
			return err
		}
		err = tx.RetargetSourceUsageBuild(ctx, source, 1, 4, -1)
		if err == nil {
			t.Fatal("Retarget accepted a negative parser version")
		}
		assertErrorKind(t, err, ErrorDatabase)
		state, err := tx.SourceUsageEpoch(ctx, source)
		if err != nil {
			return err
		}
		if state.BuildEpoch == nil || *state.BuildEpoch != 1 ||
			state.BuildParserVersion == nil || *state.BuildParserVersion != 4 {
			t.Fatalf("failed negative Retarget changed Build Pair: %+v", state)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}
