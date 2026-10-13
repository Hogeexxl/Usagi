package api

import (
	"database/sql"
	"path/filepath"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/database"
	"github.com/Hogeexxl/Usagi/internal/query"
)

func newRuntimeQueryForTest(t *testing.T, provider query.ClientProvider) (*query.Service, *sql.DB) {
	t.Helper()
	db, err := database.Open(filepath.Join(t.TempDir(), "usagi.sqlite"))
	if err != nil {
		t.Fatalf("open test database: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return query.NewRuntimeService(db, provider, time.Local), db
}

func databaseRevisionForTest(t *testing.T, db *sql.DB) int64 {
	t.Helper()
	var revision int64
	if err := db.QueryRow("SELECT CAST(value AS INTEGER) FROM app_meta WHERE key = 'data_revision'").Scan(&revision); err != nil {
		t.Fatalf("read test database revision: %v", err)
	}
	return revision
}
