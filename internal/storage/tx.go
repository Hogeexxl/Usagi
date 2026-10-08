package storage

import (
	"context"
	"database/sql"
	"errors"
)

type Tx struct {
	tx *sql.Tx
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
	if callbackErr := fn(&Tx{tx: raw}); callbackErr != nil {
		rollbackErr := raw.Rollback()
		if rollbackErr != nil && !errors.Is(rollbackErr, sql.ErrTxDone) {
			return errors.Join(callbackErr, mapSQLiteError(rollbackErr))
		}
		return callbackErr
	}
	return mapSQLiteError(raw.Commit())
}
