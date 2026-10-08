package storage

import (
	"context"
	"database/sql"
	"errors"
	"math"
)

type Tx struct {
	tx                   *sql.Tx
	dataRevisionBumped   bool
	statusRevisionBumped bool
	revisionDirty        bool
	committedRevision    RevisionTuple
}

func (tx *Tx) Revisions(ctx context.Context) (RevisionTuple, error) {
	var revisions RevisionTuple
	err := tx.tx.QueryRowContext(ctx,
		"SELECT data_revision,status_revision FROM app_meta WHERE id=1",
	).Scan(&revisions.DataRevision, &revisions.StatusRevision)
	if err != nil {
		return RevisionTuple{}, mapSQLiteError(err)
	}
	return revisions, nil
}

func (tx *Tx) BumpDataRevision(ctx context.Context) (int64, error) {
	if tx.dataRevisionBumped {
		revisions, err := tx.Revisions(ctx)
		if err != nil {
			return 0, err
		}
		return revisions.DataRevision, nil
	}
	revisions, err := tx.Revisions(ctx)
	if err != nil {
		return 0, err
	}
	if revisions.DataRevision == math.MaxInt64 {
		return 0, newStorageError(ErrorInvalidState, errors.New("data revision overflow"))
	}
	next := revisions.DataRevision + 1
	result, err := tx.tx.ExecContext(ctx,
		"UPDATE app_meta SET data_revision=? WHERE id=1 AND data_revision=?",
		next,
		revisions.DataRevision,
	)
	if err != nil {
		return 0, mapSQLiteError(err)
	}
	rows, err := result.RowsAffected()
	if err != nil || rows != 1 {
		return 0, newStorageError(ErrorInvalidState, errors.New("data revision CAS failed"))
	}
	tx.dataRevisionBumped = true
	tx.revisionDirty = true
	tx.committedRevision = RevisionTuple{
		DataRevision:   next,
		StatusRevision: revisions.StatusRevision,
	}
	return next, nil
}

func (tx *Tx) BumpStatusRevision(ctx context.Context) (int64, error) {
	if tx.statusRevisionBumped {
		revisions, err := tx.Revisions(ctx)
		if err != nil {
			return 0, err
		}
		return revisions.StatusRevision, nil
	}
	revisions, err := tx.Revisions(ctx)
	if err != nil {
		return 0, err
	}
	if revisions.StatusRevision == math.MaxInt64 {
		return 0, newStorageError(ErrorInvalidState, errors.New("status revision overflow"))
	}
	next := revisions.StatusRevision + 1
	result, err := tx.tx.ExecContext(ctx,
		"UPDATE app_meta SET status_revision=? WHERE id=1 AND status_revision=?",
		next,
		revisions.StatusRevision,
	)
	if err != nil {
		return 0, mapSQLiteError(err)
	}
	rows, err := result.RowsAffected()
	if err != nil || rows != 1 {
		return 0, newStorageError(ErrorInvalidState, errors.New("status revision CAS failed"))
	}
	tx.statusRevisionBumped = true
	tx.revisionDirty = true
	tx.committedRevision = RevisionTuple{
		DataRevision:   revisions.DataRevision,
		StatusRevision: next,
	}
	return next, nil
}

func (db *DB) WriteTx(ctx context.Context, fn func(*Tx) error) (err error) {
	raw, err := db.writer.BeginTx(ctx, nil)
	if err != nil {
		return mapSQLiteError(err)
	}
	defer func() { _ = raw.Rollback() }()
	defer func() {
		if value := recover(); value != nil {
			_ = raw.Rollback()
			panic(value)
		}
	}()
	wrapped := &Tx{tx: raw}
	if callbackErr := fn(wrapped); callbackErr != nil {
		rollbackErr := raw.Rollback()
		if rollbackErr != nil && !errors.Is(rollbackErr, sql.ErrTxDone) {
			return errors.Join(callbackErr, mapSQLiteError(rollbackErr))
		}
		return callbackErr
	}
	if err := raw.Commit(); err != nil {
		return mapSQLiteError(err)
	}
	if wrapped.revisionDirty {
		db.revisions.publish(wrapped.committedRevision)
	}
	return nil
}
