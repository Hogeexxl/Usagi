package storage

import (
	"context"
	"database/sql"
	"errors"
	"testing"

	"modernc.org/sqlite"
)

func assertConnectionPragmas(t *testing.T, conn *sql.Conn, expected map[string]any) {
	t.Helper()
	for name, want := range expected {
		var got any
		if err := conn.QueryRowContext(context.Background(), "PRAGMA "+name).Scan(&got); err != nil {
			t.Fatal(err)
		}
		if got != want {
			t.Errorf("PRAGMA %s=%v; want %v", name, got, want)
		}
	}
}

func TestWriterPragmas(t *testing.T) {
	db := testCurrent(t)
	conn, err := db.writer.Conn(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	assertConnectionPragmas(t, conn, map[string]any{
		"journal_mode": "wal", "synchronous": int64(1),
		"foreign_keys": int64(1), "busy_timeout": int64(5000),
	})
	if err := conn.Close(); err != nil {
		t.Fatal(err)
	}
	if err := db.Validate(context.Background()); err != nil {
		t.Fatal(err)
	}
	first := db.Close()
	second := db.Close()
	if first != second || first != nil {
		t.Fatalf("Close results: first=%v second=%v", first, second)
	}
}

func TestReaderPragmasEveryConnection(t *testing.T) {
	db := testCurrent(t)
	ctx := context.Background()
	connections := make([]*sql.Conn, 0, 4)
	defer func() {
		for _, conn := range connections {
			if err := conn.Close(); err != nil {
				t.Error(err)
			}
		}
	}()
	for range 4 {
		conn, err := db.readers.Conn(ctx)
		if err != nil {
			t.Fatal(err)
		}
		connections = append(connections, conn)
	}
	if db.readers.Stats().InUse != 4 {
		t.Fatal("four physical reader connections must be held simultaneously")
	}
	for _, conn := range connections {
		assertConnectionPragmas(t, conn, map[string]any{
			"foreign_keys": int64(1), "busy_timeout": int64(5000), "query_only": int64(1),
		})
		_, err := conn.ExecContext(ctx, "UPDATE app_meta SET data_revision=data_revision+1 WHERE id=?", int64(1))
		var sqliteErr *sqlite.Error
		if !errors.As(err, &sqliteErr) || sqliteErr.Code()&0xff != 8 {
			t.Fatalf("reader write error=%v; want SQLITE_READONLY", err)
		}
	}
}
