package storage

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"errors"
	"testing"
	"time"
)

func TestWriteTxImmediateIndependentConnection(t *testing.T) {
	db := testCurrent(t)
	params := writerURIParams()
	params.Set("_busy_timeout", "50")
	second := openTestSQLite(t, db.Path(), params)
	ctx := context.Background()
	if err := db.WriteTx(ctx, func(*Tx) error {
		deadline, cancel := context.WithTimeout(ctx, time.Second)
		defer cancel()
		tx, err := second.BeginTx(deadline, nil)
		if err == nil {
			_ = tx.Rollback()
			t.Fatal("independent Writer entered before Tx1 performed DML")
		}
		assertErrorKind(t, mapSQLiteError(err), ErrorDatabaseBusy)
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	tx, err := second.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := tx.Rollback(); err != nil {
		t.Fatal(err)
	}
}

var rollbackFailure = errors.New("injected rollback failure")

type rollbackFailureDriver struct{}
type rollbackFailureConn struct{}
type rollbackFailureTx struct{}

func (rollbackFailureDriver) Open(string) (driver.Conn, error) {
	return rollbackFailureConn{}, nil
}
func (rollbackFailureConn) Prepare(string) (driver.Stmt, error) {
	return nil, errors.New("unexpected Prepare")
}
func (rollbackFailureConn) Close() error              { return nil }
func (rollbackFailureConn) Begin() (driver.Tx, error) { return rollbackFailureTx{}, nil }
func (rollbackFailureTx) Commit() error               { return errors.New("unexpected Commit") }
func (rollbackFailureTx) Rollback() error             { return rollbackFailure }

func init() { sql.Register("usagi-test-rollback-failure", rollbackFailureDriver{}) }

func TestWriteTxCallbackErrorRollbackFailure(t *testing.T) {
	writer, err := sql.Open("usagi-test-rollback-failure", "")
	if err != nil {
		t.Fatal(err)
	}
	defer writer.Close()
	db := &DB{writer: writer}
	callbackErr := errors.New("callback failure")
	err = db.WriteTx(context.Background(), func(*Tx) error { return callbackErr })
	if !errors.Is(err, callbackErr) || !errors.Is(err, rollbackFailure) {
		t.Fatalf("joined error lost callback/rollback failure: %v", err)
	}
	joined, ok := err.(interface{ Unwrap() []error })
	if !ok || joined.Unwrap()[0] != callbackErr {
		t.Fatalf("callback error must be first: %v", err)
	}
}

func TestWriteTxPanicRollback(t *testing.T) {
	db := testCurrent(t)
	ctx := context.Background()
	var before int64
	if err := db.readers.QueryRowContext(ctx, "SELECT data_revision FROM app_meta WHERE id=1").Scan(&before); err != nil {
		t.Fatal(err)
	}
	value := &struct{ message string }{"panic identity"}
	func() {
		defer func() {
			if got := recover(); got != value {
				t.Fatalf("panic value=%v; want identical value %v", got, value)
			}
		}()
		_ = db.WriteTx(ctx, func(tx *Tx) error {
			if _, err := tx.tx.ExecContext(ctx, "UPDATE app_meta SET data_revision=data_revision+1 WHERE id=?", int64(1)); err != nil {
				t.Fatal(err)
			}
			panic(value)
		})
	}()
	var after int64
	if err := db.readers.QueryRowContext(ctx, "SELECT data_revision FROM app_meta WHERE id=1").Scan(&after); err != nil {
		t.Fatal(err)
	}
	if after != before {
		t.Fatalf("panic transaction persisted revision %d; before %d", after, before)
	}
	if err := db.WriteTx(ctx, func(*Tx) error { return nil }); err != nil {
		t.Fatalf("panic left Writer transaction locked: %v", err)
	}
}

func TestWriteTxCommitAndCallbackRollback(t *testing.T) {
	db := testCurrent(t)
	ctx := context.Background()
	if err := db.WriteTx(ctx, func(tx *Tx) error {
		_, err := tx.tx.ExecContext(ctx, "UPDATE app_meta SET data_revision=? WHERE id=?", int64(77), int64(1))
		return err
	}); err != nil {
		t.Fatal(err)
	}
	callbackErr := errors.New("rollback request")
	if err := db.WriteTx(ctx, func(tx *Tx) error {
		if _, err := tx.tx.ExecContext(ctx, "UPDATE app_meta SET data_revision=? WHERE id=?", int64(88), int64(1)); err != nil {
			return err
		}
		return callbackErr
	}); err != callbackErr {
		t.Fatalf("rollback result=%v; want original callback error", err)
	}
	var revision int64
	if err := db.readers.QueryRowContext(ctx, "SELECT data_revision FROM app_meta WHERE id=1").Scan(&revision); err != nil {
		t.Fatal(err)
	}
	if revision != 77 {
		t.Fatalf("revision=%d; committed value must survive failed callback", revision)
	}
}
