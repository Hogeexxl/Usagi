package storage

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"
	"unicode"

	"github.com/Hogeexxl/Usagi/internal/domain"
)

const threadSelectColumns = `thread_id,source,native_session_id,parent_thread_id,root_session_id,agent_role,title,project_name,project_path,project_kind,metadata_model,created_at_ms,updated_at_ms,archived,metadata_quality_status,metadata_resolved_at_ms`

type ThreadMutationOutcome struct {
	VisibleChanged        bool
	PreviousRootSessionID *string
	NextRootSessionID     *string
}

func scanThread(scanner interface{ Scan(...any) error }) (domain.Thread, error) {
	var thread domain.Thread
	var parent, root, title, projectName, projectPath, model sql.NullString
	var created, updated sql.NullInt64
	var archived any
	if err := scanner.Scan(&thread.ThreadID, &thread.Source, &thread.NativeSessionID,
		&parent, &root, &thread.AgentRole, &title, &projectName, &projectPath,
		&thread.ProjectKind, &model, &created, &updated, &archived,
		&thread.MetadataQualityStatus, &thread.MetadataResolvedAtMS); err != nil {
		return domain.Thread{}, mapSQLiteError(err)
	}
	archivedInteger, ok := archived.(int64)
	if !ok || (archivedInteger != 0 && archivedInteger != 1) {
		return domain.Thread{}, newStorageError(ErrorInvalidState, fmt.Errorf("archived must be SQLite integer 0 or 1"))
	}
	thread.Archived = archivedInteger == 1
	thread.ParentThreadID = threadStringPointer(parent)
	thread.RootSessionID = threadStringPointer(root)
	thread.Title = threadStringPointer(title)
	thread.ProjectName = threadStringPointer(projectName)
	thread.ProjectPath = threadStringPointer(projectPath)
	thread.MetadataModel = threadStringPointer(model)
	thread.CreatedAtMS = threadIntPointer(created)
	thread.UpdatedAtMS = threadIntPointer(updated)
	for _, err := range []error{thread.Source.Validate(), thread.AgentRole.Validate(), thread.ProjectKind.Validate(), thread.MetadataQualityStatus.Validate()} {
		if err != nil {
			return domain.Thread{}, newStorageError(ErrorInvalidState, err)
		}
	}
	return thread, nil
}

func threadStringPointer(value sql.NullString) *string {
	if !value.Valid {
		return nil
	}
	return &value.String
}

func threadIntPointer(value sql.NullInt64) *int64 {
	if !value.Valid {
		return nil
	}
	return &value.Int64
}

func (db *DB) GetThreadByID(ctx context.Context, threadID string) (domain.Thread, bool, error) {
	return readThreadResult(db.readers.QueryRowContext(ctx, "SELECT "+threadSelectColumns+" FROM threads WHERE thread_id=?", threadID))
}

func (db *DB) GetThreadBySourceNativeID(ctx context.Context, source domain.SourceID, nativeSessionID string) (domain.Thread, bool, error) {
	if err := source.Validate(); err != nil {
		return domain.Thread{}, false, newStorageError(ErrorInvalidState, err)
	}
	if err := validateThreadQueryText(nativeSessionID); err != nil {
		return domain.Thread{}, false, newStorageError(ErrorInvalidState, err)
	}
	return readThreadResult(db.readers.QueryRowContext(ctx, "SELECT "+threadSelectColumns+" FROM threads WHERE source=? AND native_session_id=?", source, nativeSessionID))
}

func validateThreadQueryText(value string) error {
	if strings.TrimSpace(value) == "" {
		return fmt.Errorf("native_session_id must not be empty")
	}
	for _, r := range value {
		if unicode.IsControl(r) {
			return fmt.Errorf("native_session_id must not contain control characters")
		}
	}
	return nil
}

func readThreadResult(row *sql.Row) (domain.Thread, bool, error) {
	thread, err := scanThread(row)
	if errors.Is(err, sql.ErrNoRows) {
		return domain.Thread{}, false, nil
	}
	if err != nil {
		return domain.Thread{}, false, err
	}
	return thread, true, nil
}

func validateThreadIdentityPatch(identity domain.SessionIdentity, patch domain.ResolvedThreadPatch) error {
	if err := identity.Validate(); err != nil {
		return newStorageError(ErrorInvalidState, err)
	}
	if err := patch.Validate(); err != nil {
		return newStorageError(ErrorInvalidState, err)
	}
	if identity.ThreadID != patch.ThreadID || identity.Source != patch.Source || identity.NativeSessionID != patch.NativeSessionID {
		return newStorageError(ErrorInvalidState, fmt.Errorf("thread identity and patch identity differ"))
	}
	return nil
}

func buildInsertedThread(identity domain.SessionIdentity, patch domain.ResolvedThreadPatch) (domain.Thread, error) {
	if err := validateThreadIdentityPatch(identity, patch); err != nil {
		return domain.Thread{}, err
	}
	return applyThreadPatch(domain.Thread{
		ThreadID: identity.ThreadID, Source: identity.Source, NativeSessionID: identity.NativeSessionID,
		AgentRole: "unknown", ProjectKind: "unknown",
	}, patch)
}

func applyThreadPatch(current domain.Thread, patch domain.ResolvedThreadPatch) (domain.Thread, error) {
	next := current
	next.ParentThreadID = applyNullableThreadPatch(current.ParentThreadID, patch.ParentThreadID)
	next.RootSessionID = applyNullableThreadPatch(current.RootSessionID, patch.RootSessionID)
	if value, ok := patch.AgentRole.Value(); ok {
		next.AgentRole = value
	}
	next.Title = applyNullableThreadPatch(current.Title, patch.Title)
	next.ProjectName = applyNullableThreadPatch(current.ProjectName, patch.ProjectName)
	next.ProjectPath = applyNullableThreadPatch(current.ProjectPath, patch.ProjectPath)
	if value, ok := patch.ProjectKind.Value(); ok {
		next.ProjectKind = value
	}
	next.MetadataModel = applyNullableThreadPatch(current.MetadataModel, patch.MetadataModel)
	next.CreatedAtMS = applyNullableThreadPatch(current.CreatedAtMS, patch.CreatedAtMS)
	next.UpdatedAtMS = applyNullableThreadPatch(current.UpdatedAtMS, patch.UpdatedAtMS)
	if value, ok := patch.Archived.Value(); ok {
		next.Archived = value
	}
	next.MetadataQualityStatus = patch.MetadataQualityStatus
	next.MetadataResolvedAtMS = patch.ResolvedAtMS
	if role, ok := patch.AgentRole.Value(); ok && role == "main" && next.RootSessionID == nil {
		next.RootSessionID = &next.ThreadID
	}
	if err := validateThreadRole(next); err != nil {
		return domain.Thread{}, err
	}
	return next, nil
}

func applyNullableThreadPatch[T any](current *T, patch domain.Patch[T]) *T {
	switch patch.Kind() {
	case domain.PatchSet:
		value, _ := patch.Value()
		return &value
	case domain.PatchClear:
		return nil
	default:
		return current
	}
}

func validateThreadRole(thread domain.Thread) error {
	switch thread.AgentRole {
	case "main":
		if thread.ParentThreadID != nil || thread.RootSessionID == nil || *thread.RootSessionID != thread.ThreadID {
			return newStorageError(ErrorInvalidState, fmt.Errorf("main thread must have no parent and root equal to itself"))
		}
	case "subagent":
		if thread.ParentThreadID == nil {
			return newStorageError(ErrorInvalidState, fmt.Errorf("subagent thread must have a parent"))
		}
	case "unknown":
		if thread.RootSessionID != nil {
			return newStorageError(ErrorInvalidState, fmt.Errorf("unknown thread must have no root"))
		}
	}
	return nil
}

func validateRelatedThreadSources(ctx context.Context, tx *sql.Tx, thread domain.Thread) error {
	for _, id := range []*string{thread.ParentThreadID, thread.RootSessionID} {
		if id == nil {
			continue
		}
		var source domain.SourceID
		err := tx.QueryRowContext(ctx, "SELECT source FROM threads WHERE thread_id=?", *id).Scan(&source)
		if errors.Is(err, sql.ErrNoRows) {
			continue
		}
		if err != nil {
			return mapSQLiteError(err)
		}
		if source != thread.Source {
			return newStorageError(ErrorInvalidState, fmt.Errorf("related thread %q has a different source", *id))
		}
	}
	return nil
}

func (tx *Tx) UpsertThread(ctx context.Context, identity domain.SessionIdentity, patch domain.ResolvedThreadPatch) (ThreadMutationOutcome, error) {
	if err := validateThreadIdentityPatch(identity, patch); err != nil {
		return ThreadMutationOutcome{}, err
	}
	current, exists, err := readThreadResult(tx.tx.QueryRowContext(ctx, "SELECT "+threadSelectColumns+" FROM threads WHERE thread_id=?", identity.ThreadID))
	if err != nil {
		return ThreadMutationOutcome{}, err
	}
	native, nativeExists, err := readThreadResult(tx.tx.QueryRowContext(ctx, "SELECT "+threadSelectColumns+" FROM threads WHERE source=? AND native_session_id=?", identity.Source, identity.NativeSessionID))
	if err != nil {
		return ThreadMutationOutcome{}, err
	}
	if exists && (current.Source != identity.Source || current.NativeSessionID != identity.NativeSessionID) {
		return ThreadMutationOutcome{}, newStorageError(ErrorInvalidState, fmt.Errorf("thread source native identity is immutable"))
	}
	if nativeExists && native.ThreadID != identity.ThreadID {
		return ThreadMutationOutcome{}, newStorageError(ErrorInvalidState, fmt.Errorf("source native identity already belongs to another thread"))
	}
	previousRoot := current.RootSessionID
	var next domain.Thread
	if exists {
		next, err = applyThreadPatch(current, patch)
	} else {
		next, err = buildInsertedThread(identity, patch)
	}
	if err != nil {
		return ThreadMutationOutcome{}, err
	}
	if err := validateRelatedThreadSources(ctx, tx.tx, next); err != nil {
		return ThreadMutationOutcome{}, err
	}
	archived := int64(0)
	if next.Archived {
		archived = 1
	}
	values := []any{next.ParentThreadID, next.RootSessionID, next.AgentRole, next.Title, next.ProjectName, next.ProjectPath,
		next.ProjectKind, next.MetadataModel, next.CreatedAtMS, next.UpdatedAtMS, archived, next.MetadataQualityStatus, next.MetadataResolvedAtMS}
	if exists {
		_, err = tx.tx.ExecContext(ctx, `UPDATE threads SET parent_thread_id=?,root_session_id=?,agent_role=?,title=?,project_name=?,project_path=?,project_kind=?,metadata_model=?,created_at_ms=?,updated_at_ms=?,archived=?,metadata_quality_status=?,metadata_resolved_at_ms=? WHERE thread_id=?`, append(values, next.ThreadID)...)
	} else {
		values = append([]any{next.ThreadID, next.Source, next.NativeSessionID}, values...)
		_, err = tx.tx.ExecContext(ctx, "INSERT INTO threads ("+threadSelectColumns+") VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", values...)
	}
	if err != nil {
		return ThreadMutationOutcome{}, mapSQLiteError(err)
	}
	return ThreadMutationOutcome{VisibleChanged: !exists || !threadVisibilityEqual(current, next), PreviousRootSessionID: previousRoot, NextRootSessionID: next.RootSessionID}, nil
}

// This is the field set of Rust read_session_visibility / SessionVisibility.
func threadVisibilityEqual(a, b domain.Thread) bool {
	return a.Source == b.Source && a.NativeSessionID == b.NativeSessionID &&
		threadOptionalEqual(a.ParentThreadID, b.ParentThreadID) && threadOptionalEqual(a.RootSessionID, b.RootSessionID) &&
		a.AgentRole == b.AgentRole && threadOptionalEqual(a.Title, b.Title) && threadOptionalEqual(a.ProjectName, b.ProjectName) &&
		threadOptionalEqual(a.ProjectPath, b.ProjectPath) && a.ProjectKind == b.ProjectKind && threadOptionalEqual(a.MetadataModel, b.MetadataModel) &&
		threadOptionalEqual(a.CreatedAtMS, b.CreatedAtMS) && threadOptionalEqual(a.UpdatedAtMS, b.UpdatedAtMS) &&
		a.Archived == b.Archived && a.MetadataQualityStatus == b.MetadataQualityStatus && a.MetadataResolvedAtMS == b.MetadataResolvedAtMS
}

func threadOptionalEqual[T comparable](a, b *T) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return *a == *b
}
