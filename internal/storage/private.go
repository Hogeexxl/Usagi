package storage

import (
	"context"
	"database/sql"
	"errors"
)

type PrivateReader interface {
	Query(query string, args ...any) (*sql.Rows, error)
	QueryRow(query string, args ...any) *sql.Row
}

type PrivateTx interface {
	Exec(query string, args ...any) (sql.Result, error)
	Query(query string, args ...any) (*sql.Rows, error)
	QueryRow(query string, args ...any) *sql.Row
}

var ErrNilPrivateCallback = errors.New("private callback is nil")

type privateReader struct {
	tx  *sql.Tx
	ctx context.Context
}

func (p privateReader) Query(query string, args ...any) (*sql.Rows, error) {
	return p.tx.QueryContext(p.ctx, query, args...)
}

func (p privateReader) QueryRow(query string, args ...any) *sql.Row {
	return p.tx.QueryRowContext(p.ctx, query, args...)
}

type privateWriter struct {
	tx  *sql.Tx
	ctx context.Context
}

func (p privateWriter) Exec(query string, args ...any) (sql.Result, error) {
	return p.tx.ExecContext(p.ctx, query, args...)
}

func (p privateWriter) Query(query string, args ...any) (*sql.Rows, error) {
	return p.tx.QueryContext(p.ctx, query, args...)
}

func (p privateWriter) QueryRow(query string, args ...any) *sql.Row {
	return p.tx.QueryRowContext(p.ctx, query, args...)
}

func (db *DB) PrivateRead(ctx context.Context, fn func(PrivateReader) error) error {
	if fn == nil {
		return ErrNilPrivateCallback
	}
	return db.ReadTx(ctx, func(tx *sql.Tx) error {
		return fn(privateReader{tx: tx, ctx: ctx})
	})
}

func (tx *Tx) Private(ctx context.Context, fn func(PrivateTx) error) error {
	if fn == nil {
		return ErrNilPrivateCallback
	}
	return fn(privateWriter{tx: tx.tx, ctx: ctx})
}
