package source

import (
	"context"
	"database/sql"
	"errors"
	"math"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/storage"
	"github.com/Hogeexxl/Usagi/internal/usage"
)

func openSourceTestDB(t *testing.T) *storage.DB {
	t.Helper()
	db, err := storage.Open(context.Background(), storage.Config{Path: filepath.Join(t.TempDir(), "source.sqlite3")})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := db.Close(); err != nil {
			t.Error(err)
		}
	})
	return db
}

func sourceRunContext(t *testing.T, db *storage.DB, scanID string, source domain.SourceID) (context.Context, context.CancelFunc, RunContext) {
	t.Helper()
	workerCtx, cancel := context.WithCancel(context.Background())
	descriptor := mustDescriptor(t, source, string(source))
	run, err := NewStorageFactory(db).Context(workerCtx, scanID, descriptor)
	if err != nil {
		cancel()
		t.Fatal(err)
	}
	return workerCtx, cancel, run
}

func sourceThread(t *testing.T, source domain.SourceID, id string) (domain.SessionIdentity, domain.ResolvedThreadPatch) {
	t.Helper()
	identity, err := domain.NewSessionIdentity(id, source, "native-"+id)
	if err != nil {
		t.Fatal(err)
	}
	patch, err := domain.NewResolvedThreadPatch(identity, 1)
	if err != nil {
		t.Fatal(err)
	}
	return identity, patch
}

func sourceEvent(id, threadID, rootSessionID string) usage.CanonicalUsageEventWrite {
	cost := int64(100)
	cacheWrite := int64(1)
	return usage.CanonicalUsageEventWrite{
		EventID:               id,
		Kind:                  usage.EventKindNormal,
		OccurredAtMS:          10,
		ThreadID:              threadID,
		RootSessionID:         rootSessionID,
		TurnKey:               sourceStringPointer("turn"),
		Model:                 "model",
		ReasoningEffort:       sourceStringPointer("high"),
		EstimatedCostNanosUSD: &cost,
		Usage: usage.NormalizedTokenUsage{
			InputTokens:      10,
			CachedTokens:     2,
			CacheWriteTokens: &cacheWrite,
			OutputTokens:     4,
			ReasoningTokens:  1,
			TotalTokens:      14,
		},
		CreatedAtMS: 11,
	}
}

func sourceStringPointer(value string) *string {
	return &value
}

func sourceIntPointer(value int64) *int64 {
	return &value
}

func TestRunContextBinding(t *testing.T) {
	db := openSourceTestDB(t)
	workerCtx, cancel := context.WithCancel(context.WithValue(context.Background(), struct{}{}, "worker-value"))
	defer cancel()
	descriptor := mustDescriptor(t, domain.SourceCodex, "Codex")
	run, err := NewStorageFactory(db).Context(workerCtx, "scan-context", descriptor)
	if err != nil {
		t.Fatal(err)
	}
	if run.ScanID() != "scan-context" || run.Source() != domain.SourceCodex ||
		run.Storage().scanID != run.ScanID() || run.Storage().source != run.Source() {
		t.Fatalf("RunContext binding mismatch: scan=%q source=%q storage=%+v", run.ScanID(), run.Source(), run.Storage())
	}
	cancel()
	state, found, err := run.Storage().LoadUsageEpoch()
	if err != nil || !found || state.Source != domain.SourceCodex {
		t.Fatalf("storage operation lost its non-cancel context: state=%+v found=%t err=%v", state, found, err)
	}

	for _, scanID := range []string{"", " \u2003", "bad\nscan"} {
		if _, err := NewStorageFactory(db).Context(context.Background(), scanID, descriptor); err == nil {
			t.Errorf("Context accepted scan ID %q", scanID)
		}
	}
	if _, err := NewStorageFactory(db).Context(nil, "scan", descriptor); err == nil {
		t.Fatal("Context accepted a nil worker context")
	}
	if _, err := NewStorageFactory(nil).Context(context.Background(), "scan", descriptor); err == nil {
		t.Fatal("nil DB factory Context succeeded")
	}
	var nilFactory *StorageFactory
	if _, err := nilFactory.Context(context.Background(), "scan", descriptor); err == nil {
		t.Fatal("nil StorageFactory receiver Context succeeded")
	}
	if _, err := NewStorageFactory(db).Context(
		context.Background(),
		"scan",
		Descriptor{ID: domain.SourceID("Invalid"), DisplayName: "Invalid"},
	); err == nil {
		t.Fatal("Context accepted an invalid Descriptor")
	}
}

func TestSourceStorageWorkerCancellationIsolation(t *testing.T) {
	for _, test := range []struct {
		name        string
		returnError bool
	}{
		{name: "callback completes", returnError: false},
		{name: "explicit cancellation checkpoint", returnError: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			db := openSourceTestDB(t)
			workerCtx, cancel, run := sourceRunContext(t, db, "scan-cancel-"+test.name, domain.SourceCodex)
			defer cancel()
			identity, patch := sourceThread(t, domain.SourceCodex, "thread-cancel-"+test.name)
			callbackErr := error(context.Canceled)
			err := run.Storage().Write(func(tx *WriteTx) error {
				cancel()
				if _, err := tx.UpsertThreadNoRevision(identity, patch); err != nil {
					return err
				}
				if test.returnError {
					return workerCtx.Err()
				}
				return nil
			})
			if test.returnError {
				if !errors.Is(err, callbackErr) {
					t.Fatalf("Write error = %v; want context cancellation", err)
				}
				if _, found, err := db.GetThreadByID(context.Background(), identity.ThreadID); err != nil || found {
					t.Fatalf("cancelled callback committed thread: found=%t err=%v", found, err)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if _, found, err := db.GetThreadByID(context.Background(), identity.ThreadID); err != nil || !found {
				t.Fatalf("successful callback did not commit after worker cancellation: found=%t err=%v", found, err)
			}
		})
	}
}

func TestSourceWriteUsesSingleStorageTx(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-shared-tx", domain.SourceCodex)
	defer cancel()
	identity, patch := sourceThread(t, domain.SourceCodex, "thread-shared-tx")
	err := run.Storage().Write(func(tx *WriteTx) error {
		if err := tx.UpsertThread(identity, patch); err != nil {
			return err
		}
		return tx.Private(func(privateTx storage.PrivateTx) error {
			_, err := privateTx.Exec(
				"UPDATE codex_adapter_state SET home_fingerprint=?,binding_status='ready' WHERE id=1",
				"shared-transaction",
			)
			return err
		})
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, found, err := db.GetThreadByID(context.Background(), identity.ThreadID); err != nil || !found {
		t.Fatalf("canonical write missing: found=%t err=%v", found, err)
	}
	if got := readCodexBinding(t, run.Storage()); got != "ready:shared-transaction" {
		t.Fatalf("private write missing: %q", got)
	}
	if got := db.CurrentRevision().DataRevision; got != 1 {
		t.Fatalf("data_revision = %d; want one bump in the shared transaction", got)
	}

	rollbackDB := openSourceTestDB(t)
	_, rollbackCancel, rollbackRun := sourceRunContext(t, rollbackDB, "scan-shared-rollback", domain.SourceCodex)
	defer rollbackCancel()
	rollbackIdentity, rollbackPatch := sourceThread(t, domain.SourceCodex, "thread-shared-rollback")
	callbackErr := errors.New("rollback both mutations")
	err = rollbackRun.Storage().Write(func(tx *WriteTx) error {
		if _, err := tx.UpsertThreadNoRevision(rollbackIdentity, rollbackPatch); err != nil {
			return err
		}
		if err := tx.Private(func(privateTx storage.PrivateTx) error {
			_, err := privateTx.Exec(
				"UPDATE codex_adapter_state SET home_fingerprint=?,binding_status='ready' WHERE id=1",
				"must-rollback",
			)
			return err
		}); err != nil {
			return err
		}
		return callbackErr
	})
	if !errors.Is(err, callbackErr) {
		t.Fatalf("Write error = %v; want callback error", err)
	}
	if _, found, err := rollbackDB.GetThreadByID(context.Background(), rollbackIdentity.ThreadID); err != nil || found {
		t.Fatalf("rollback kept canonical mutation: found=%t err=%v", found, err)
	}
	if got := readCodexBinding(t, rollbackRun.Storage()); got != "unbound:" {
		t.Fatalf("rollback kept private mutation: %q", got)
	}
}

func readCodexBinding(t *testing.T, s *Storage) string {
	t.Helper()
	var state, fingerprint sql.NullString
	err := s.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow("SELECT binding_status,home_fingerprint FROM codex_adapter_state WHERE id=1").Scan(&state, &fingerprint)
	})
	if err != nil {
		t.Fatal(err)
	}
	if !fingerprint.Valid {
		return state.String + ":"
	}
	return state.String + ":" + fingerprint.String
}

func TestSourceWriteTxClosedAfterCallback(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-closed", domain.SourceCodex)
	defer cancel()
	var clean *WriteTx
	if err := run.Storage().Write(func(tx *WriteTx) error {
		clean = tx
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := clean.BumpDataRevision(); !errors.Is(err, ErrTransactionClosed) {
		t.Fatalf("clean escaped transaction error = %v; want Closed", err)
	}
	if clean.Source() != domain.SourceCodex || clean.ScanID() != "scan-closed" {
		t.Fatalf("metadata getters lost binding after close: source=%q scan=%q", clean.Source(), clean.ScanID())
	}

	var poisoned *WriteTx
	err := run.Storage().Write(func(tx *WriteTx) error {
		poisoned = tx
		privateErr := tx.Private(func(storage.PrivateTx) error { return errors.New("private failure") })
		if privateErr == nil {
			t.Fatal("Private callback error was lost")
		}
		if _, err := tx.BumpDataRevision(); !errors.Is(err, ErrTransactionPoisoned) {
			t.Fatalf("open poisoned transaction error = %v; want Poisoned", err)
		}
		return nil
	})
	if !errors.Is(err, ErrTransactionPoisoned) {
		t.Fatalf("Write error = %v; want Poisoned", err)
	}
	if _, err := poisoned.BumpDataRevision(); !errors.Is(err, ErrTransactionClosed) {
		t.Fatalf("closed poisoned transaction error = %v; want Closed", err)
	}
	if err := poisoned.Private(nil); !errors.Is(err, ErrTransactionClosed) {
		t.Fatalf("closed Private(nil) error = %v; want Closed precedence", err)
	}
	if poisoned.Source() != domain.SourceCodex || poisoned.ScanID() != "scan-closed" {
		t.Fatalf("poisoned metadata getters lost binding: source=%q scan=%q", poisoned.Source(), poisoned.ScanID())
	}
}

func TestPrivateErrorPoisonsTransaction(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-private-poison", domain.SourceCodex)
	defer cancel()
	identity, patch := sourceThread(t, domain.SourceCodex, "thread-private-poison")
	privateErr := errors.New("private mutation failed")
	err := run.Storage().Write(func(tx *WriteTx) error {
		if _, err := tx.UpsertThreadNoRevision(identity, patch); err != nil {
			return err
		}
		if err := tx.Private(func(privateTx storage.PrivateTx) error {
			if _, err := privateTx.Exec(
				"UPDATE codex_adapter_state SET home_fingerprint=?,binding_status='ready' WHERE id=1",
				"must-rollback",
			); err != nil {
				return err
			}
			return privateErr
		}); !errors.Is(err, privateErr) {
			t.Fatalf("Private error = %v; want original callback error", err)
		}
		return nil
	})
	if !errors.Is(err, ErrTransactionPoisoned) {
		t.Fatalf("Write error = %v; want poisoned transaction", err)
	}
	if _, found, err := db.GetThreadByID(context.Background(), identity.ThreadID); err != nil || found {
		t.Fatalf("poisoned transaction committed canonical mutation: found=%t err=%v", found, err)
	}
	if got := readCodexBinding(t, run.Storage()); got != "unbound:" {
		t.Fatalf("poisoned transaction committed private mutation: %q", got)
	}
}

func TestNilCallbacksRejected(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-nil-callback", domain.SourceCodex)
	defer cancel()
	if err := run.Storage().PrivateRead(nil); !errors.Is(err, ErrNilSourceCallback) {
		t.Fatalf("PrivateRead(nil) = %v", err)
	}
	if err := run.Storage().Write(nil); !errors.Is(err, ErrNilSourceCallback) {
		t.Fatalf("Write(nil) = %v", err)
	}
	if err := run.Storage().Write(func(tx *WriteTx) error {
		if err := tx.Private(nil); !errors.Is(err, ErrNilSourceCallback) {
			t.Fatalf("WriteTx.Private(nil) = %v", err)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().DataRevision; got != 0 {
		t.Fatalf("nil callbacks mutated data_revision: %d", got)
	}
}

func TestPrivateReadCannotWrite(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-private-readonly", domain.SourceCodex)
	defer cancel()
	err := run.Storage().PrivateRead(func(reader storage.PrivateReader) error {
		var id int64
		return reader.QueryRow(
			"UPDATE codex_adapter_state SET home_fingerprint='forbidden',binding_status='ready' WHERE id=1 RETURNING id",
		).Scan(&id)
	})
	if err == nil {
		t.Fatal("PrivateRead allowed a durable mutation")
	}
	if got := readCodexBinding(t, run.Storage()); got != "unbound:" {
		t.Fatalf("PrivateRead mutation changed state: %q", got)
	}
}

func TestPrivateSharedTableRuleIsNotRuntimeSQLParser(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-shared-table-contract", domain.SourceCodex)
	defer cancel()
	before := db.CurrentRevision()
	callbackErr := errors.New("rollback shared-table probe")
	err := run.Storage().Write(func(tx *WriteTx) error {
		if err := tx.Private(func(privateTx storage.PrivateTx) error {
			_, err := privateTx.Exec("UPDATE app_meta SET data_revision=data_revision WHERE id=1")
			return err
		}); err != nil {
			return err
		}
		return callbackErr
	})
	if !errors.Is(err, callbackErr) {
		t.Fatalf("Private shared-table probe error = %v", err)
	}
	if got := db.CurrentRevision(); got != before {
		t.Fatalf("rolled back private probe changed Revision: %+v; before %+v", got, before)
	}
}

func TestThreadVisibleChangeBumpsRevision(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-thread-revision", domain.SourceCodex)
	defer cancel()
	identity, patch := sourceThread(t, domain.SourceCodex, "thread-revision")
	if err := run.Storage().Write(func(tx *WriteTx) error {
		return tx.UpsertThread(identity, patch)
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().DataRevision; got != 1 {
		t.Fatalf("insert data_revision = %d", got)
	}
	if err := run.Storage().Write(func(tx *WriteTx) error {
		return tx.UpsertThread(identity, patch)
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().DataRevision; got != 1 {
		t.Fatalf("no-op upsert data_revision = %d", got)
	}
	patch.Title = domain.Set("visible title")
	if err := run.Storage().Write(func(tx *WriteTx) error {
		return tx.UpsertThread(identity, patch)
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().DataRevision; got != 2 {
		t.Fatalf("visible patch data_revision = %d", got)
	}
}

func TestNoRevisionBatchingProtocol(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-no-revision-batch", domain.SourceCodex)
	defer cancel()
	thread1, patch1 := sourceThread(t, domain.SourceCodex, "thread-batch-1")
	thread2, patch2 := sourceThread(t, domain.SourceCodex, "thread-batch-2")
	if err := run.Storage().Write(func(tx *WriteTx) error {
		if err := tx.UpsertThread(thread1, patch1); err != nil {
			return err
		}
		return tx.UpsertThread(thread2, patch2)
	}); err != nil {
		t.Fatal(err)
	}
	if err := run.Storage().Write(func(tx *WriteTx) error {
		if _, err := tx.BeginOrResumeUsageBuild(1); err != nil {
			return err
		}
		first := sourceEvent("event-batch-delete", thread1.ThreadID, thread1.ThreadID)
		second := sourceEvent("event-batch-rebind", thread1.ThreadID, thread1.ThreadID)
		for _, event := range []usage.CanonicalUsageEventWrite{first, second} {
			if _, err := tx.WriteUsageNoRevision(UsageTargetBuild, event); err != nil {
				return err
			}
		}
		_, err := tx.ActivateUsageBuild(1, 1)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	before := db.CurrentRevision().DataRevision
	rootChanged := thread1.ThreadID != thread2.ThreadID
	err := run.Storage().Write(func(tx *WriteTx) error {
		deleted, err := tx.DeleteUsageNoRevision(UsageTargetActive, []string{"event-batch-delete"})
		if err != nil {
			return err
		}
		if deleted != 1 {
			t.Fatalf("NoRevision delete count = %d", deleted)
		}
		if _, err := tx.RebindUsageRootNoRevision(UsageTargetActive, thread1.ThreadID, thread2.ThreadID); err != nil {
			return err
		}
		if rootChanged {
			_, err = tx.BumpDataRevision()
		}
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().DataRevision; got != before+1 {
		t.Fatalf("batched data_revision = %d; before=%d", got, before)
	}
}

func TestUsageActivationRevisionOverflowRollsBack(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-source-overflow", domain.SourceCodex)
	defer cancel()
	identity, patch := sourceThread(t, domain.SourceCodex, "thread-source-overflow")
	if err := run.Storage().Write(func(tx *WriteTx) error {
		if _, err := tx.UpsertThreadNoRevision(identity, patch); err != nil {
			return err
		}
		if _, err := tx.BeginOrResumeUsageBuild(1); err != nil {
			return err
		}
		event := sourceEvent("event-source-overflow", identity.ThreadID, identity.ThreadID)
		_, err := tx.WriteUsageNoRevision(UsageTargetBuild, event)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	setSourceDataRevision(t, db, math.MaxInt64)
	err := run.Storage().Write(func(tx *WriteTx) error {
		_, err := tx.ActivateUsageBuild(1, 1)
		return err
	})
	if err == nil {
		t.Fatal("Activation succeeded with an overflowing data_revision")
	}
	state, found, err := run.Storage().LoadUsageEpoch()
	if err != nil || !found || state.ActiveEpoch != 0 || state.BuildEpoch == nil || *state.BuildEpoch != 1 {
		t.Fatalf("overflow changed Epoch: state=%+v found=%t err=%v", state, found, err)
	}
	if got := readSourceDataRevision(t, db); got != math.MaxInt64 {
		t.Fatalf("overflow changed data_revision: %d", got)
	}
}

func setSourceDataRevision(t *testing.T, db *storage.DB, revision int64) {
	t.Helper()
	raw, err := sql.Open("sqlite", db.Path())
	if err != nil {
		t.Fatal(err)
	}
	defer raw.Close()
	if _, err := raw.ExecContext(context.Background(), "UPDATE app_meta SET data_revision=? WHERE id=1", revision); err != nil {
		t.Fatal(err)
	}
}

func readSourceDataRevision(t *testing.T, db *storage.DB) int64 {
	t.Helper()
	raw, err := sql.Open("sqlite", db.Path())
	if err != nil {
		t.Fatal(err)
	}
	defer raw.Close()
	var revision int64
	if err := raw.QueryRowContext(context.Background(), "SELECT data_revision FROM app_meta WHERE id=1").Scan(&revision); err != nil {
		t.Fatal(err)
	}
	return revision
}
func seedSourceThread(t *testing.T, run RunContext, id string) domain.SessionIdentity {
	t.Helper()
	identity, patch := sourceThread(t, run.Source(), id)
	if err := run.Storage().Write(func(tx *WriteTx) error {
		_, err := tx.UpsertThreadNoRevision(identity, patch)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	return identity
}

func beginSourceBuild(
	t *testing.T,
	run RunContext,
	parserVersion int64,
	events ...usage.CanonicalUsageEventWrite,
) int64 {
	t.Helper()
	var epoch int64
	err := run.Storage().Write(func(tx *WriteTx) error {
		var err error
		epoch, err = tx.BeginOrResumeUsageBuild(parserVersion)
		if err != nil {
			return err
		}
		for _, event := range events {
			if _, err := tx.WriteUsageNoRevision(UsageTargetBuild, event); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return epoch
}

func activateSourceBuild(
	t *testing.T,
	run RunContext,
	epoch int64,
	parserVersion int64,
) UsageActivationOutcome {
	t.Helper()
	var outcome UsageActivationOutcome
	if err := run.Storage().Write(func(tx *WriteTx) error {
		var err error
		outcome, err = tx.ActivateUsageBuild(epoch, parserVersion)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	return outcome
}

func currentSourceEpoch(t *testing.T, run RunContext) domain.SourceUsageEpochState {
	t.Helper()
	state, found, err := run.Storage().LoadUsageEpoch()
	if err != nil {
		t.Fatal(err)
	}
	if !found {
		t.Fatal("Source Usage Epoch was not found")
	}
	return state
}

func TestUsageWriteTargetGates(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-target-gates", domain.SourceCodex)
	defer cancel()
	if err := run.Storage().Write(func(tx *WriteTx) error {
		if _, err := tx.ResolveUsageWriteEpoch(UsageTargetActive); err == nil {
			t.Fatal("uninitialized Active Target resolved")
		}
		if _, err := tx.ResolveUsageWriteEpoch(UsageTargetBuild); err == nil {
			t.Fatal("missing Build Target resolved")
		}
		_, err := tx.BeginOrResumeUsageBuild(1)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if err := run.Storage().Write(func(tx *WriteTx) error {
		if epoch, err := tx.ResolveUsageWriteEpoch(UsageTargetBuild); err != nil || epoch != 1 {
			t.Fatalf("Build Target = %d, %v", epoch, err)
		}
		if _, err := tx.ResolveUsageWriteEpoch(UsageTargetActive); err == nil {
			t.Fatal("active_epoch=0 became writable")
		}
		_, err := tx.ActivateUsageBuild(1, 1)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if err := run.Storage().Write(func(tx *WriteTx) error {
		epoch, err := tx.ResolveUsageWriteEpoch(UsageTargetActive)
		if err != nil {
			return err
		}
		if epoch != 1 {
			t.Fatalf("Active Target epoch = %d; want 1", epoch)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestUsageWriteTargetZeroValueRejected(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-zero-target", domain.SourceCodex)
	defer cancel()
	epoch := beginSourceBuild(t, run, 1)
	activateSourceBuild(t, run, epoch, 1)
	beginSourceBuild(t, run, 2)
	before := currentSourceEpoch(t, run)
	revision := db.CurrentRevision()
	event := sourceEvent("zero-target-event", "thread", "thread")
	var invalidTarget UsageWriteTarget
	err := run.Storage().Write(func(tx *WriteTx) error {
		checks := []struct {
			name string
			err  error
		}{}
		_, resolveErr := tx.ResolveUsageWriteEpoch(invalidTarget)
		checks = append(checks, struct {
			name string
			err  error
		}{"Resolve", resolveErr})
		_, compareErr := tx.CompareUsageNoRevision(invalidTarget, event)
		checks = append(checks, struct {
			name string
			err  error
		}{"Compare", compareErr})
		_, writeErr := tx.WriteUsageNoRevision(invalidTarget, event)
		checks = append(checks, struct {
			name string
			err  error
		}{"Write", writeErr})
		_, copyFromErr := tx.CopyUsageNoRevision(invalidTarget, UsageTargetBuild, "event")
		checks = append(checks, struct {
			name string
			err  error
		}{"Copy from", copyFromErr})
		_, copyToErr := tx.CopyUsageNoRevision(UsageTargetActive, invalidTarget, "event")
		checks = append(checks, struct {
			name string
			err  error
		}{"Copy to", copyToErr})
		_, copyErr := tx.CopyUsage(UsageTargetBuild, invalidTarget, "event")
		checks = append(checks, struct {
			name string
			err  error
		}{"revision-aware Copy", copyErr})
		_, deleteErr := tx.DeleteUsageNoRevision(invalidTarget, []string{"event"})
		checks = append(checks, struct {
			name string
			err  error
		}{"Delete", deleteErr})
		_, deleteWithRevisionErr := tx.DeleteUsage(invalidTarget, []string{"event"})
		checks = append(checks, struct {
			name string
			err  error
		}{"revision-aware Delete", deleteWithRevisionErr})
		_, rebindErr := tx.RebindUsageRootNoRevision(invalidTarget, "thread", "root")
		checks = append(checks, struct {
			name string
			err  error
		}{"Rebind", rebindErr})
		for _, check := range checks {
			if check.err == nil {
				t.Errorf("%s accepted UsageWriteTarget zero value", check.name)
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	after := currentSourceEpoch(t, run)
	if after.ActiveEpoch != before.ActiveEpoch || after.BuildEpoch == nil ||
		before.BuildEpoch == nil || *after.BuildEpoch != *before.BuildEpoch {
		t.Fatalf("zero targets changed Epoch state: before=%+v after=%+v", before, after)
	}
	if got := db.CurrentRevision(); got != revision {
		t.Fatalf("zero targets changed Revision: before=%+v after=%+v", revision, got)
	}
}

func TestBuildWritesDoNotBumpDataRevision(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-build-invisible", domain.SourceCodex)
	defer cancel()
	thread := seedSourceThread(t, run, "thread-build-invisible")
	activeEvent := sourceEvent("event-active-build-copy", thread.ThreadID, thread.ThreadID)
	buildEvent := sourceEvent("event-build-delete", thread.ThreadID, thread.ThreadID)
	epoch := beginSourceBuild(t, run, 1, activeEvent)
	activateSourceBuild(t, run, epoch, 1)
	revision := db.CurrentRevision()
	if err := run.Storage().Write(func(tx *WriteTx) error {
		if _, err := tx.BeginOrResumeUsageBuild(2); err != nil {
			return err
		}
		if err := tx.WriteUsage(UsageTargetBuild, buildEvent); err != nil {
			return err
		}
		outcome, err := tx.CopyUsage(UsageTargetActive, UsageTargetBuild, activeEvent.EventID)
		if err != nil {
			return err
		}
		if outcome != storage.UsageInserted {
			t.Fatalf("Active-to-Build copy outcome = %d", outcome)
		}
		deleted, err := tx.DeleteUsage(UsageTargetBuild, []string{buildEvent.EventID})
		if err != nil {
			return err
		}
		if deleted != 1 {
			t.Fatalf("Build delete count = %d", deleted)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision(); got != revision {
		t.Fatalf("Build mutations changed Revision: before=%+v after=%+v", revision, got)
	}
}

func TestInactiveUsageDeleteGuard(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-inactive-delete", domain.SourceCodex)
	defer cancel()
	thread := seedSourceThread(t, run, "thread-inactive-delete")
	event := sourceEvent("event-inactive-delete", thread.ThreadID, thread.ThreadID)
	activateSourceBuild(t, run, beginSourceBuild(t, run, 1, event), 1)
	if err := run.Storage().Write(func(tx *WriteTx) error {
		if _, err := tx.BeginOrResumeUsageBuild(2); err != nil {
			return err
		}
		if _, err := tx.CopyUsageNoRevision(UsageTargetActive, UsageTargetBuild, event.EventID); err != nil {
			return err
		}
		if _, err := tx.DeleteInactiveUsageNoRevision(1, []string{event.EventID}); err == nil {
			t.Fatal("inactive delete removed the Active Epoch")
		}
		if _, err := tx.DeleteInactiveUsageNoRevision(2, []string{event.EventID}); err == nil {
			t.Fatal("inactive delete removed the Build Epoch")
		}
		_, err := tx.ActivateUsageBuild(2, 2)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	revision := db.CurrentRevision()
	if err := run.Storage().Write(func(tx *WriteTx) error {
		deleted, err := tx.DeleteInactiveUsageNoRevision(1, []string{event.EventID})
		if err != nil {
			return err
		}
		if deleted != 1 {
			t.Fatalf("inactive delete count = %d", deleted)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision(); got != revision {
		t.Fatalf("inactive deletion changed Revision: before=%+v after=%+v", revision, got)
	}
}

func TestUsageActivationCanonicalProjection(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-activation-projection", domain.SourceCodex)
	defer cancel()
	thread := seedSourceThread(t, run, "thread-activation-projection")
	event := sourceEvent("event-activation-projection", thread.ThreadID, thread.ThreadID)
	activateSourceBuild(t, run, beginSourceBuild(t, run, 1, event), 1)
	build := event
	build.CreatedAtMS++
	beginSourceBuild(t, run, 2, build)
	before := db.CurrentRevision()
	var comparatorCalls int
	var outcome UsageActivationOutcome
	err := run.Storage().Write(func(tx *WriteTx) error {
		var err error
		outcome, err = tx.ActivateUsageBuildWithPrivateVisibility(2, 2,
			func(_ storage.PrivateTx, source domain.SourceID, activeEpoch, activeParser, buildEpoch, buildParser int64) (bool, error) {
				comparatorCalls++
				if source != domain.SourceCodex || activeEpoch != 1 || activeParser != 1 ||
					buildEpoch != 2 || buildParser != 2 {
					t.Fatalf("Comparator inputs = %q/%d/%d/%d/%d", source, activeEpoch, activeParser, buildEpoch, buildParser)
				}
				return true, nil
			})
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	if comparatorCalls != 1 || outcome.VisibleChanged || outcome.ActiveEpoch != 2 ||
		outcome.DataRevision != before.DataRevision {
		t.Fatalf("equal projection activation = %+v, comparator calls=%d, before=%+v", outcome, comparatorCalls, before)
	}
}

func TestFirstUsageActivationFromEpochZero(t *testing.T) {
	t.Run("empty Build", func(t *testing.T) {
		db := openSourceTestDB(t)
		_, cancel, run := sourceRunContext(t, db, "scan-first-empty", domain.SourceCodex)
		defer cancel()
		outcome := activateSourceBuild(t, run, beginSourceBuild(t, run, 1), 1)
		if outcome.ActiveEpoch != 1 || outcome.VisibleChanged || outcome.DataRevision != 0 {
			t.Fatalf("empty first activation = %+v", outcome)
		}
		state := currentSourceEpoch(t, run)
		if state.ActiveEpoch != 1 || state.BuildEpoch != nil {
			t.Fatalf("first empty activation state = %+v", state)
		}
	})
	t.Run("non-empty Build", func(t *testing.T) {
		db := openSourceTestDB(t)
		_, cancel, run := sourceRunContext(t, db, "scan-first-nonempty", domain.SourceCodex)
		defer cancel()
		thread := seedSourceThread(t, run, "thread-first-nonempty")
		event := sourceEvent("event-first-nonempty", thread.ThreadID, thread.ThreadID)
		beginSourceBuild(t, run, 1, event)
		outcome := activateSourceBuild(t, run, 1, 1)
		if outcome.ActiveEpoch != 1 || !outcome.VisibleChanged || outcome.DataRevision != 1 ||
			db.CurrentRevision().DataRevision != 1 {
			t.Fatalf("non-empty first activation = %+v, current=%+v", outcome, db.CurrentRevision())
		}
	})
}

func TestUsageActivationIncludesEstimatedCost(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-activation-cost", domain.SourceCodex)
	defer cancel()
	thread := seedSourceThread(t, run, "thread-activation-cost")
	active := sourceEvent("event-activation-cost", thread.ThreadID, thread.ThreadID)
	activateSourceBuild(t, run, beginSourceBuild(t, run, 1, active), 1)
	before := db.CurrentRevision()
	build := active
	build.EstimatedCostNanosUSD = sourceIntPointer(200)
	beginSourceBuild(t, run, 2, build)
	outcome := activateSourceBuild(t, run, 2, 2)
	if !outcome.VisibleChanged || outcome.DataRevision != before.DataRevision+1 {
		t.Fatalf("cost-only activation = %+v; before=%+v", outcome, before)
	}
}

func TestUsageActivationExcludesCreatedAt(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-activation-created-at", domain.SourceCodex)
	defer cancel()
	thread := seedSourceThread(t, run, "thread-activation-created-at")
	active := sourceEvent("event-activation-created-at", thread.ThreadID, thread.ThreadID)
	activateSourceBuild(t, run, beginSourceBuild(t, run, 1, active), 1)
	before := db.CurrentRevision()
	build := active
	build.CreatedAtMS += 100
	beginSourceBuild(t, run, 2, build)
	outcome := activateSourceBuild(t, run, 2, 2)
	if outcome.VisibleChanged || outcome.DataRevision != before.DataRevision {
		t.Fatalf("CreatedAt-only activation = %+v; before=%+v", outcome, before)
	}
}

func TestPrivateComparatorOnlyRunsAfterCanonicalEqual(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-comparator-order", domain.SourceCodex)
	defer cancel()
	thread := seedSourceThread(t, run, "thread-comparator-order")
	active := sourceEvent("event-comparator-order", thread.ThreadID, thread.ThreadID)
	activateSourceBuild(t, run, beginSourceBuild(t, run, 1, active), 1)
	build := active
	build.Model = "different-model"
	beginSourceBuild(t, run, 2, build)
	var calls int
	var outcome UsageActivationOutcome
	if err := run.Storage().Write(func(tx *WriteTx) error {
		var err error
		outcome, err = tx.ActivateUsageBuildWithPrivateVisibility(2, 2,
			func(storage.PrivateTx, domain.SourceID, int64, int64, int64, int64) (bool, error) {
				calls++
				return true, nil
			})
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if calls != 0 || !outcome.VisibleChanged {
		t.Fatalf("canonical-different activation = %+v; comparator calls=%d", outcome, calls)
	}
}

func TestNilPrivateVisibilityComparatorRejected(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-nil-comparator", domain.SourceCodex)
	defer cancel()
	beginSourceBuild(t, run, 1)
	before := currentSourceEpoch(t, run)
	revision := db.CurrentRevision()
	err := run.Storage().Write(func(tx *WriteTx) error {
		_, err := tx.ActivateUsageBuildWithPrivateVisibility(1, 1, nil)
		if !errors.Is(err, ErrNilPrivateVisibilityComparator) {
			t.Fatalf("nil comparator error = %v", err)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	after := currentSourceEpoch(t, run)
	if !reflect.DeepEqual(before, after) {
		t.Fatalf("nil comparator changed Epoch: before=%+v after=%+v", before, after)
	}
	if got := db.CurrentRevision(); got != revision {
		t.Fatalf("nil comparator changed Revision: before=%+v after=%+v", revision, got)
	}
}

func TestUsageActivationAtomicRollback(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-activation-rollback", domain.SourceCodex)
	defer cancel()
	thread := seedSourceThread(t, run, "thread-activation-rollback")
	event := sourceEvent("event-activation-rollback", thread.ThreadID, thread.ThreadID)
	activateSourceBuild(t, run, beginSourceBuild(t, run, 1, event), 1)
	beginSourceBuild(t, run, 2, event)
	before := currentSourceEpoch(t, run)
	revision := db.CurrentRevision()
	privateErr := errors.New("private comparison failed")
	err := run.Storage().Write(func(tx *WriteTx) error {
		_, err := tx.ActivateUsageBuildWithPrivateVisibility(2, 2,
			func(privateTx storage.PrivateTx, _ domain.SourceID, _, _, _, _ int64) (bool, error) {
				if _, err := privateTx.Exec(
					"UPDATE codex_adapter_state SET home_fingerprint=?,binding_status='ready' WHERE id=1",
					"must-rollback",
				); err != nil {
					return false, err
				}
				return false, privateErr
			})
		return err
	})
	if !errors.Is(err, privateErr) {
		t.Fatalf("Activation error = %v; want comparator error", err)
	}
	after := currentSourceEpoch(t, run)
	if !reflect.DeepEqual(before, after) {
		t.Fatalf("failed Activation changed Epoch: before=%+v after=%+v", before, after)
	}
	if got := readCodexBinding(t, run.Storage()); got != "unbound:" {
		t.Fatalf("failed Activation kept comparator mutation: %q", got)
	}
	if got := db.CurrentRevision(); got != revision {
		t.Fatalf("failed Activation changed Revision: before=%+v after=%+v", revision, got)
	}

	var comparatorCompleted bool
	err = run.Storage().Write(func(tx *WriteTx) error {
		_, err := tx.ActivateUsageBuildWithPrivateVisibility(2, 2,
			func(privateTx storage.PrivateTx, _ domain.SourceID, _, _, _, _ int64) (bool, error) {
				if _, err := privateTx.Exec(
					"UPDATE codex_adapter_state SET home_fingerprint=?,binding_status='ready' WHERE id=1",
					"activation-cas-must-rollback",
				); err != nil {
					return false, err
				}
				if _, err := privateTx.Exec(
					"CREATE TEMP TRIGGER fail_source_usage_activation BEFORE UPDATE ON source_usage_epochs BEGIN SELECT RAISE(IGNORE); END",
				); err != nil {
					return false, err
				}
				comparatorCompleted = true
				return true, nil
			})
		return err
	})
	var activationErr *storage.Error
	if !errors.As(err, &activationErr) || activationErr.Kind != storage.ErrorInvalidState ||
		!strings.Contains(err.Error(), "activation CAS failed") {
		t.Fatalf("Activation CAS error = %v; want an invalid-state CAS failure", err)
	}
	if !comparatorCompleted {
		t.Fatal("private comparator did not complete before the Activation CAS failure")
	}
	after = currentSourceEpoch(t, run)
	if !reflect.DeepEqual(before, after) {
		t.Fatalf("CAS-failed Activation changed Epoch: before=%+v after=%+v", before, after)
	}
	if got := readCodexBinding(t, run.Storage()); got != "unbound:" {
		t.Fatalf("CAS-failed Activation kept comparator mutation: %q", got)
	}
	if got := db.CurrentRevision(); got != revision {
		t.Fatalf("CAS-failed Activation changed Revision: before=%+v after=%+v", revision, got)
	}
}

func TestActiveUsageInsertDuplicateRevision(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-active-usage-revision", domain.SourceCodex)
	defer cancel()
	thread := seedSourceThread(t, run, "thread-active-usage-revision")
	activateSourceBuild(t, run, beginSourceBuild(t, run, 1), 1)
	event := sourceEvent("event-active-usage-revision", thread.ThreadID, thread.ThreadID)
	before := db.CurrentRevision().DataRevision
	if err := run.Storage().Write(func(tx *WriteTx) error {
		return tx.WriteUsage(UsageTargetActive, event)
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().DataRevision; got != before+1 {
		t.Fatalf("Active insert data_revision = %d; before=%d", got, before)
	}
	if err := run.Storage().Write(func(tx *WriteTx) error {
		return tx.WriteUsage(UsageTargetActive, event)
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().DataRevision; got != before+1 {
		t.Fatalf("Active duplicate changed data_revision: %d; before=%d", got, before)
	}
}

func TestActiveDeleteRevisionAndRebindParity(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-active-delete-rebind", domain.SourceCodex)
	defer cancel()
	thread := seedSourceThread(t, run, "thread-active-delete-rebind")
	event := sourceEvent("event-active-delete-rebind", thread.ThreadID, thread.ThreadID)
	activateSourceBuild(t, run, beginSourceBuild(t, run, 1, event), 1)
	beforeDelete := db.CurrentRevision().DataRevision
	if err := run.Storage().Write(func(tx *WriteTx) error {
		deleted, err := tx.DeleteUsage(UsageTargetActive, []string{event.EventID})
		if err != nil {
			return err
		}
		if deleted != 1 {
			t.Fatalf("Active delete count = %d", deleted)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().DataRevision; got != beforeDelete+1 {
		t.Fatalf("Active delete data_revision = %d; before=%d", got, beforeDelete)
	}
	if err := run.Storage().Write(func(tx *WriteTx) error {
		return tx.WriteUsage(UsageTargetActive, event)
	}); err != nil {
		t.Fatal(err)
	}
	beforeRebind := db.CurrentRevision()
	var affected int
	if err := run.Storage().Write(func(tx *WriteTx) error {
		var err error
		affected, err = tx.RebindUsageRootNoRevision(UsageTargetActive, thread.ThreadID, thread.ThreadID)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if affected != 1 {
		t.Fatalf("same-root Rebind rows affected = %d; want the unfiltered Rust primitive result", affected)
	}
	if got := db.CurrentRevision(); got != beforeRebind {
		t.Fatalf("Rebind primitive changed Revision: before=%+v after=%+v", beforeRebind, got)
	}
	if _, ok := reflect.TypeOf((*WriteTx)(nil)).MethodByName("RebindUsageRoot"); ok {
		t.Fatal("Source WriteTx exposes a revision-aware Rebind wrapper")
	}
}

func TestCopyUsageRevisionAwareWrapper(t *testing.T) {
	db := openSourceTestDB(t)
	_, cancel, run := sourceRunContext(t, db, "scan-copy-revision", domain.SourceCodex)
	defer cancel()
	thread := seedSourceThread(t, run, "thread-copy-revision")
	activeEvent := sourceEvent("event-copy-active", thread.ThreadID, thread.ThreadID)
	activateSourceBuild(t, run, beginSourceBuild(t, run, 1, activeEvent), 1)
	buildEvent := sourceEvent("event-copy-build", thread.ThreadID, thread.ThreadID)
	beginSourceBuild(t, run, 2)
	before := db.CurrentRevision().DataRevision
	if err := run.Storage().Write(func(tx *WriteTx) error {
		outcome, err := tx.CopyUsage(UsageTargetActive, UsageTargetBuild, activeEvent.EventID)
		if err != nil {
			return err
		}
		if outcome != storage.UsageInserted {
			t.Fatalf("Copy-to-Build outcome = %d", outcome)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().DataRevision; got != before {
		t.Fatalf("Copy-to-Build changed data_revision: %d; before=%d", got, before)
	}
	if err := run.Storage().Write(func(tx *WriteTx) error {
		outcome, err := tx.CopyUsage(UsageTargetBuild, UsageTargetActive, activeEvent.EventID)
		if err != nil {
			return err
		}
		if outcome != storage.UsageDuplicate {
			t.Fatalf("Copy duplicate outcome = %d", outcome)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().DataRevision; got != before {
		t.Fatalf("duplicate Copy changed data_revision: %d; before=%d", got, before)
	}
	if err := run.Storage().Write(func(tx *WriteTx) error {
		_, err := tx.WriteUsageNoRevision(UsageTargetBuild, buildEvent)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if err := run.Storage().Write(func(tx *WriteTx) error {
		outcome, err := tx.CopyUsage(UsageTargetBuild, UsageTargetActive, buildEvent.EventID)
		if err != nil {
			return err
		}
		if outcome != storage.UsageInserted {
			t.Fatalf("Copy-to-Active outcome = %d", outcome)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().DataRevision; got != before+1 {
		t.Fatalf("Copy-to-Active insert data_revision = %d; before=%d", got, before)
	}
	if err := run.Storage().Write(func(tx *WriteTx) error {
		outcome, err := tx.CopyUsage(UsageTargetBuild, UsageTargetActive, buildEvent.EventID)
		if err != nil {
			return err
		}
		if outcome != storage.UsageDuplicate {
			t.Fatalf("repeat Copy outcome = %d", outcome)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if got := db.CurrentRevision().DataRevision; got != before+1 {
		t.Fatalf("duplicate Copy-to-Active changed data_revision: %d; before=%d", got, before)
	}
}
