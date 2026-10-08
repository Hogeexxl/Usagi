package storage

import (
	"context"
	"database/sql"
	"errors"
	"sync"

	_ "modernc.org/sqlite"
)

type Config struct {
	Path string
}

type DB struct {
	writer    *sql.DB
	readers   *sql.DB
	path      string
	closeOnce sync.Once
	closeErr  error
}

func openWriter(path string) (*sql.DB, error) {
	uri, err := buildSQLiteURI(path, writerURIParams())
	if err != nil {
		return nil, err
	}
	db, err := sql.Open("sqlite", uri)
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	if err := db.PingContext(context.Background()); err != nil {
		return nil, closeAfterOpenError(db, err)
	}
	return db, nil
}

func openReaders(path string) (*sql.DB, error) {
	uri, err := buildSQLiteURI(path, readerURIParams())
	if err != nil {
		return nil, err
	}
	db, err := sql.Open("sqlite", uri)
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	db.SetMaxOpenConns(4)
	db.SetMaxIdleConns(4)
	if err := db.PingContext(context.Background()); err != nil {
		return nil, closeAfterOpenError(db, err)
	}
	return db, nil
}

func openInspection(path string) (*sql.DB, error) {
	uri, err := buildSQLiteURI(path, inspectionURIParams())
	if err != nil {
		return nil, err
	}
	db, err := sql.Open("sqlite", uri)
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	return db, nil
}

func closeAfterOpenError(db *sql.DB, openErr error) error {
	closeErr := db.Close()
	if closeErr != nil {
		return errors.Join(mapSQLiteError(openErr), mapSQLiteError(closeErr))
	}
	return mapSQLiteError(openErr)
}

func (db *DB) Close() error {
	db.closeOnce.Do(func() {
		readerErr := db.readers.Close()
		writerErr := db.writer.Close()
		if readerErr != nil {
			readerErr = mapSQLiteError(readerErr)
		}
		if writerErr != nil {
			writerErr = mapSQLiteError(writerErr)
		}
		db.closeErr = errors.Join(readerErr, writerErr)
	})
	return db.closeErr
}

func (db *DB) Read(ctx context.Context, fn func(*sql.Conn) error) error {
	conn, err := db.readers.Conn(ctx)
	if err != nil {
		return mapSQLiteError(err)
	}
	defer conn.Close()
	return fn(conn)
}

func (db *DB) ReadTx(ctx context.Context, fn func(*sql.Tx) error) error {
	tx, err := db.readers.BeginTx(ctx, &sql.TxOptions{ReadOnly: true})
	if err != nil {
		return mapSQLiteError(err)
	}
	defer func() { _ = tx.Rollback() }()
	if callbackErr := fn(tx); callbackErr != nil {
		rollbackErr := tx.Rollback()
		if rollbackErr != nil && !errors.Is(rollbackErr, sql.ErrTxDone) {
			return errors.Join(callbackErr, mapSQLiteError(rollbackErr))
		}
		return callbackErr
	}
	return mapSQLiteError(tx.Commit())
}
