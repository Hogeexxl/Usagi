package api

import (
	"path/filepath"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/database"
	"github.com/Hogeexxl/Usagi/internal/query"
)

func newReadyRuntimeQueryForTest(t *testing.T, provider query.ClientProvider) *query.Service {
	t.Helper()
	db, err := database.Open(filepath.Join(t.TempDir(), "usagi.sqlite"))
	if err != nil {
		t.Fatalf("open test database: %v", err)
	}
	if _, err := db.Exec("UPDATE app_meta SET value = '1' WHERE key = 'initial_sync_complete'"); err != nil {
		_ = db.Close()
		t.Fatalf("mark test database initially synced: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return query.NewRuntimeService(db, provider, time.Local)
}
