package storage

import (
	"context"
	"errors"
	"path/filepath"
	"strings"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/domain"
)

func openCanonicalTestDB(t *testing.T) *DB {
	t.Helper()
	db, err := Open(context.Background(), Config{Path: filepath.Join(t.TempDir(), "usagi.sqlite3")})
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

func canonicalThreadPatch(t *testing.T, id string, source domain.SourceID) (domain.SessionIdentity, domain.ResolvedThreadPatch) {
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

func canonicalUpsert(t *testing.T, db *DB, identity domain.SessionIdentity, patch domain.ResolvedThreadPatch) ThreadMutationOutcome {
	t.Helper()
	var outcome ThreadMutationOutcome
	err := db.WriteTx(context.Background(), func(tx *Tx) error {
		var err error
		outcome, err = tx.UpsertThread(context.Background(), identity, patch)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return outcome
}

func requireCanonicalInvalidState(t *testing.T, err error) {
	t.Helper()
	var storageErr *Error
	if !errors.As(err, &storageErr) || storageErr.Kind != ErrorInvalidState {
		t.Fatalf("error = %v, want ErrorInvalidState", err)
	}
}

func TestThreadIdentityValidation(t *testing.T) {
	ctx := context.Background()
	db := openCanonicalTestDB(t)
	identity, patch := canonicalThreadPatch(t, "thread", domain.SourceCodex)
	canonicalUpsert(t, db, identity, patch)
	for _, test := range []struct {
		name     string
		identity domain.SessionIdentity
		patch    domain.ResolvedThreadPatch
	}{
		{"empty id", domain.SessionIdentity{Source: domain.SourceCodex, NativeSessionID: "native"}, patch},
		{"control id", domain.SessionIdentity{ThreadID: "bad\u0085id", Source: domain.SourceCodex, NativeSessionID: "native"}, patch},
		{"patch mismatch", domain.SessionIdentity{ThreadID: "other", Source: domain.SourceCodex, NativeSessionID: "other"}, patch},
	} {
		t.Run(test.name, func(t *testing.T) {
			requireCanonicalInvalidState(t, db.WriteTx(ctx, func(tx *Tx) error { _, err := tx.UpsertThread(ctx, test.identity, test.patch); return err }))
		})
	}
	conflict := identity
	conflict.Source = domain.SourceAntigravity
	conflictPatch, err := domain.NewResolvedThreadPatch(conflict, 1)
	if err != nil {
		t.Fatal(err)
	}
	requireCanonicalInvalidState(t, db.WriteTx(ctx, func(tx *Tx) error { _, err := tx.UpsertThread(ctx, conflict, conflictPatch); return err }))
	conflict = identity
	conflict.ThreadID = "other-thread"
	conflictPatch, err = domain.NewResolvedThreadPatch(conflict, 1)
	if err != nil {
		t.Fatal(err)
	}
	requireCanonicalInvalidState(t, db.WriteTx(ctx, func(tx *Tx) error { _, err := tx.UpsertThread(ctx, conflict, conflictPatch); return err }))
	row, found, err := db.GetThreadBySourceNativeID(ctx, identity.Source, identity.NativeSessionID)
	if err != nil || !found || row.ThreadID != identity.ThreadID {
		t.Fatalf("native lookup: %+v, %t, %v", row, found, err)
	}
	if _, found, err := db.GetThreadByID(ctx, "absent"); err != nil || found {
		t.Fatalf("absent lookup: %t, %v", found, err)
	}
	for _, value := range []string{"", "\u2003", "native\x00"} {
		_, _, err := db.GetThreadBySourceNativeID(ctx, domain.SourceCodex, value)
		requireCanonicalInvalidState(t, err)
	}
	_, _, err = db.GetThreadBySourceNativeID(ctx, domain.SourceID("Bad"), "native")
	requireCanonicalInvalidState(t, err)
}

func TestThreadPatchStates(t *testing.T) {
	ctx := context.Background()
	db := openCanonicalTestDB(t)
	identity, patch := canonicalThreadPatch(t, "thread", domain.SourceCodex)
	inserted := canonicalUpsert(t, db, identity, patch)
	if !inserted.VisibleChanged || inserted.PreviousRootSessionID != nil || inserted.NextRootSessionID != nil {
		t.Fatalf("insert outcome: %+v", inserted)
	}
	row, found, err := db.GetThreadByID(ctx, identity.ThreadID)
	if err != nil || !found {
		t.Fatalf("GetThreadByID: %t, %v", found, err)
	}
	if row.AgentRole != "unknown" || row.ProjectKind != "unknown" || row.Archived || row.ParentThreadID != nil || row.RootSessionID != nil ||
		row.Title != nil || row.ProjectName != nil || row.ProjectPath != nil || row.MetadataModel != nil || row.CreatedAtMS != nil || row.UpdatedAtMS != nil ||
		row.MetadataQualityStatus != "complete" || row.MetadataResolvedAtMS != 1 {
		t.Fatalf("insert defaults: %+v", row)
	}
	if canonicalUpsert(t, db, identity, patch).VisibleChanged {
		t.Fatal("identical patch reported a visible change")
	}
	patch.MetadataQualityStatus = "partial"
	if !canonicalUpsert(t, db, identity, patch).VisibleChanged {
		t.Fatal("metadata quality change was invisible")
	}
	patch.ResolvedAtMS = 2
	if !canonicalUpsert(t, db, identity, patch).VisibleChanged {
		t.Fatal("metadata resolution time change was invisible")
	}
	rootID, rootPatch := canonicalThreadPatch(t, "root", domain.SourceCodex)
	rootPatch.AgentRole = domain.Set(domain.AgentRole("main"))
	rootOutcome := canonicalUpsert(t, db, rootID, rootPatch)
	if rootOutcome.NextRootSessionID == nil || *rootOutcome.NextRootSessionID != rootID.ThreadID {
		t.Fatalf("main root default: %+v", rootOutcome)
	}
	patch.ParentThreadID = domain.Set("root")
	patch.RootSessionID = domain.Set("root")
	patch.AgentRole = domain.Set(domain.AgentRole("subagent"))
	patch.Title = domain.Set("title")
	patch.ProjectName = domain.Set("project")
	patch.ProjectPath = domain.Set(t.TempDir())
	patch.ProjectKind = domain.Set(domain.ProjectKind("project"))
	patch.MetadataModel = domain.Set("model")
	patch.CreatedAtMS = domain.Set(int64(10))
	patch.UpdatedAtMS = domain.Set(int64(20))
	patch.Archived = domain.Set(true)
	outcome := canonicalUpsert(t, db, identity, patch)
	if !outcome.VisibleChanged || outcome.NextRootSessionID == nil || *outcome.NextRootSessionID != "root" {
		t.Fatalf("Set outcome: %+v", outcome)
	}
	row, _, err = db.GetThreadByID(ctx, identity.ThreadID)
	if err != nil {
		t.Fatal(err)
	}
	path, _ := patch.ProjectPath.Value()
	if row.AgentRole != "subagent" || row.ParentThreadID == nil || *row.ParentThreadID != "root" ||
		row.RootSessionID == nil || *row.RootSessionID != "root" || row.Title == nil || *row.Title != "title" ||
		row.ProjectName == nil || *row.ProjectName != "project" || row.ProjectPath == nil || *row.ProjectPath != path ||
		row.ProjectKind != "project" || row.MetadataModel == nil || *row.MetadataModel != "model" ||
		row.CreatedAtMS == nil || *row.CreatedAtMS != 10 || row.UpdatedAtMS == nil || *row.UpdatedAtMS != 20 || !row.Archived {
		t.Fatalf("Set row: %+v", row)
	}
	keep, err := domain.NewResolvedThreadPatch(identity, patch.ResolvedAtMS)
	if err != nil {
		t.Fatal(err)
	}
	keep.MetadataQualityStatus = patch.MetadataQualityStatus
	if canonicalUpsert(t, db, identity, keep).VisibleChanged {
		t.Fatal("Keep changed values")
	}
	clear := keep
	clear.FullResolution = true
	clear.ParentThreadID = domain.Clear[string]()
	clear.RootSessionID = domain.Clear[string]()
	clear.AgentRole = domain.Set(domain.AgentRole("unknown"))
	clear.Title = domain.Clear[string]()
	clear.ProjectName = domain.Clear[string]()
	clear.ProjectPath = domain.Clear[string]()
	clear.ProjectKind = domain.Set(domain.ProjectKind("unknown"))
	clear.MetadataModel = domain.Clear[string]()
	clear.CreatedAtMS = domain.Clear[int64]()
	clear.UpdatedAtMS = domain.Clear[int64]()
	clear.Archived = domain.Set(false)
	outcome = canonicalUpsert(t, db, identity, clear)
	if outcome.PreviousRootSessionID == nil || *outcome.PreviousRootSessionID != "root" || outcome.NextRootSessionID != nil {
		t.Fatalf("Clear outcome: %+v", outcome)
	}
	row, _, err = db.GetThreadByID(ctx, identity.ThreadID)
	if err != nil {
		t.Fatal(err)
	}
	if row.ParentThreadID != nil || row.RootSessionID != nil || row.Title != nil || row.ProjectName != nil || row.ProjectPath != nil || row.MetadataModel != nil || row.CreatedAtMS != nil || row.UpdatedAtMS != nil || row.Archived {
		t.Fatalf("Clear row: %+v", row)
	}
	main := keep
	main.AgentRole = domain.Set(domain.AgentRole("main"))
	if got := canonicalUpsert(t, db, identity, main); got.NextRootSessionID == nil || *got.NextRootSessionID != identity.ThreadID {
		t.Fatalf("update main root: %+v", got)
	}

	t.Run("role and source gates", func(t *testing.T) {
		foreignID, foreign := canonicalThreadPatch(t, "foreign", domain.SourceAntigravity)
		canonicalUpsert(t, db, foreignID, foreign)
		foreignUpdate := keep
		foreignUpdate.AgentRole = domain.Set(domain.AgentRole("subagent"))
		foreignUpdate.ParentThreadID = domain.Set("foreign")
		requireCanonicalInvalidState(t, db.WriteTx(ctx, func(tx *Tx) error {
			_, err := tx.UpsertThread(ctx, identity, foreignUpdate)
			return err
		}))
		for _, change := range []func(*domain.ResolvedThreadPatch){
			func(p *domain.ResolvedThreadPatch) { p.AgentRole = domain.Set(domain.AgentRole("subagent")) },
			func(p *domain.ResolvedThreadPatch) {
				p.AgentRole = domain.Set(domain.AgentRole("main"))
				p.RootSessionID = domain.Set("root")
			},
			func(p *domain.ResolvedThreadPatch) {
				p.AgentRole = domain.Set(domain.AgentRole("subagent"))
				p.ParentThreadID = domain.Set("foreign")
			},
			func(p *domain.ResolvedThreadPatch) {
				p.AgentRole = domain.Set(domain.AgentRole("subagent"))
				p.ParentThreadID = domain.Set("root")
				p.RootSessionID = domain.Set("foreign")
			},
		} {
			id, p := canonicalThreadPatch(t, "invalid", domain.SourceCodex)
			change(&p)
			requireCanonicalInvalidState(t, db.WriteTx(ctx, func(tx *Tx) error { _, err := tx.UpsertThread(ctx, id, p); return err }))
		}
		id, unresolved := canonicalThreadPatch(t, "unresolved", domain.SourceCodex)
		unresolved.AgentRole = domain.Set(domain.AgentRole("subagent"))
		unresolved.ParentThreadID = domain.Set("not-yet-present")
		unresolved.RootSessionID = domain.Set("also-not-present")
		canonicalUpsert(t, db, id, unresolved)
		invalidUpdate := keep
		invalidUpdate.AgentRole = domain.Set(domain.AgentRole("unknown"))
		requireCanonicalInvalidState(t, db.WriteTx(ctx, func(tx *Tx) error { _, err := tx.UpsertThread(ctx, identity, invalidUpdate); return err }))
	})

	t.Run("scanner rejects unknown enums and non-integer archived", func(t *testing.T) {
		for _, replacement := range []struct{ column, value string }{
			{"source", "'Bad'"}, {"agent_role", "'worker'"}, {"project_kind", "'workspace'"},
			{"metadata_quality_status", "'invalid'"}, {"archived", "CAST(0 AS TEXT)"}, {"archived", "2"},
		} {
			columns := strings.Split(threadSelectColumns, ",")
			for i, column := range columns {
				if column == replacement.column {
					columns[i] = replacement.value
				}
			}
			_, err := scanThread(db.readers.QueryRowContext(ctx, "SELECT "+strings.Join(columns, ",")+" FROM threads WHERE thread_id=?", identity.ThreadID))
			requireCanonicalInvalidState(t, err)
		}
	})
}
