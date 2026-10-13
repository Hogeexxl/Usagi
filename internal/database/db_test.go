package database

import (
	"database/sql"
	"net/url"
	"path/filepath"
	"reflect"
	"testing"
)

func TestSchemaCreatesAndReopens(t *testing.T) {
	path := filepath.Join(t.TempDir(), "usagi.sqlite")
	wantTables := []string{"app_meta", "price_versions", "prices", "requests", "session_links", "sessions", "sync_days"}

	for openNumber := range 2 {
		db, err := Open(path)
		if err != nil {
			t.Fatalf("Open() error = %v", err)
		}

		var version, revision, initialSync string
		if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'schema_version'").Scan(&version); err != nil {
			t.Fatalf("read schema version: %v", err)
		}
		if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'data_revision'").Scan(&revision); err != nil {
			t.Fatalf("read data revision: %v", err)
		}
		if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'initial_sync_complete'").Scan(&initialSync); err != nil {
			t.Fatalf("read initial sync state: %v", err)
		}
		if version != "1" || revision != "0" || initialSync != "0" {
			t.Fatalf("app_meta = schema %q, revision %q, initial sync %q", version, revision, initialSync)
		}

		rows, err := db.Query("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
		if err != nil {
			t.Fatalf("list schema tables: %v", err)
		}
		var tables []string
		for rows.Next() {
			var name string
			if err := rows.Scan(&name); err != nil {
				rows.Close()
				t.Fatalf("read schema table: %v", err)
			}
			tables = append(tables, name)
		}
		if err := rows.Close(); err != nil {
			t.Fatalf("close schema rows: %v", err)
		}
		if err := rows.Err(); err != nil {
			t.Fatalf("iterate schema tables: %v", err)
		}
		if !reflect.DeepEqual(tables, wantTables) {
			t.Fatalf("schema tables = %v, want %v", tables, wantTables)
		}
		if openNumber == 0 {
			if _, err := db.Exec("INSERT INTO sync_days (day, row_count) VALUES ('2026-01-02', 3)"); err != nil {
				t.Fatalf("write sync day before reopen: %v", err)
			}
		} else {
			var rowCount int
			if err := db.QueryRow("SELECT row_count FROM sync_days WHERE day = '2026-01-02'").Scan(&rowCount); err != nil {
				t.Fatalf("read sync day after reopen: %v", err)
			}
			if rowCount != 3 {
				t.Fatalf("reopened sync day row_count = %d, want 3", rowCount)
			}
		}

		if err := db.Close(); err != nil {
			t.Fatalf("Close() error = %v", err)
		}
	}
}

func TestMigrationFailureRollsBack(t *testing.T) {
	path := filepath.Join(t.TempDir(), "usagi.sqlite")
	originalSchema := schemaSQL
	schemaSQL += "\nCREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);"
	t.Cleanup(func() { schemaSQL = originalSchema })

	db, err := Open(path)
	if err == nil {
		_ = db.Close()
		t.Fatal("Open() error = nil, want schema migration failure")
	}

	fileURL := url.URL{Scheme: "file", Path: filepath.ToSlash(path)}
	checkDB, err := sql.Open("sqlite", fileURL.String())
	if err != nil {
		t.Fatalf("open database after failed migration: %v", err)
	}
	defer checkDB.Close()

	var tableCount int
	if err := checkDB.QueryRow("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'").Scan(&tableCount); err != nil {
		t.Fatalf("count tables after failed migration: %v", err)
	}
	if tableCount != 0 {
		t.Fatalf("tables after failed migration = %d, want none", tableCount)
	}
}
