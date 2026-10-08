package storage

import (
	"errors"
	"fmt"

	sqlite "modernc.org/sqlite"
	sqlite3 "modernc.org/sqlite/lib"
)

type ErrorKind string

const (
	ErrorDatabase                 ErrorKind = "database"
	ErrorDatabaseBusy             ErrorKind = "database_busy"
	ErrorDatabaseCorrupt          ErrorKind = "database_corrupt"
	ErrorDataIntegrity            ErrorKind = "data_integrity"
	ErrorIO                       ErrorKind = "io"
	ErrorSchemaTooNew             ErrorKind = "schema_too_new"
	ErrorSchemaMismatch           ErrorKind = "schema_mismatch"
	ErrorLegacyRequiresConversion ErrorKind = "legacy_requires_conversion"
	ErrorInvalidState             ErrorKind = "invalid_state"
)

type Error struct {
	Kind ErrorKind
	Err  error
}

func (e *Error) Error() string {
	if e.Err == nil {
		return string(e.Kind)
	}
	return fmt.Sprintf("%s: %v", e.Kind, e.Err)
}

func (e *Error) Unwrap() error {
	return e.Err
}

func newStorageError(kind ErrorKind, err error) error {
	if err == nil {
		return nil
	}
	return &Error{Kind: kind, Err: err}
}

func mapSQLiteError(err error) error {
	if err == nil {
		return nil
	}
	var storageErr *Error
	if errors.As(err, &storageErr) {
		return err
	}
	var sqliteErr *sqlite.Error
	if errors.As(err, &sqliteErr) {
		code := sqliteErr.Code() & 0xff
		switch code {
		case sqlite3.SQLITE_BUSY, sqlite3.SQLITE_LOCKED:
			return newStorageError(ErrorDatabaseBusy, err)
		case sqlite3.SQLITE_CORRUPT, sqlite3.SQLITE_NOTADB:
			return newStorageError(ErrorDatabaseCorrupt, err)
		}
	}
	return newStorageError(ErrorDatabase, err)
}
