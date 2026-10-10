package codex

import (
	"database/sql"
	"path/filepath"
	"testing"
)

func TestStateIndexAllowlistMissingOptionalAndSpawnEdgeAbsent(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.sqlite")
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(`CREATE TABLE threads(id TEXT, title TEXT, prompt TEXT, created_at TEXT); INSERT INTO threads VALUES('thread-a','title','private','2026-01-02T03:04:05Z')`); err != nil {
		t.Fatal(err)
	}
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
	snapshot := ReadStateSnapshot(path)
	if snapshot.Status != StateSourceComplete || len(snapshot.Threads) != 1 {
		t.Fatalf("unexpected state snapshot: %#v", snapshot)
	}
	if snapshot.Threads[0].Title == nil || *snapshot.Threads[0].Title != "title" || snapshot.Threads[0].CreatedAtMS == nil {
		t.Fatalf("allowlisted fields missing: %#v", snapshot.Threads[0])
	}
	if snapshot.Threads[0].CWD != nil || snapshot.Threads[0].MetadataModel != nil {
		t.Fatalf("absent optional columns must remain nil: %#v", snapshot.Threads[0])
	}
	if snapshot.SpawnEdgesStatus != StateSourceUnavailable || len(snapshot.SpawnEdges) != 0 {
		t.Fatalf("absent spawn table status: %#v", snapshot)
	}
}

func TestStateIndexMissingThreadsTableOrIDIsUnavailable(t *testing.T) {
	for name, schema := range map[string]string{"missing_table": "", "missing_id": "CREATE TABLE threads(title TEXT)"} {
		t.Run(name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "state.sqlite")
			db, err := sql.Open("sqlite", path)
			if err != nil {
				t.Fatal(err)
			}
			if schema != "" {
				if _, err := db.Exec(schema); err != nil {
					t.Fatal(err)
				}
			}
			if err := db.Close(); err != nil {
				t.Fatal(err)
			}
			snapshot := ReadStateSnapshot(path)
			if snapshot.Status != StateSourceUnavailable {
				t.Fatalf("got status %v", snapshot.Status)
			}
		})
	}
}
