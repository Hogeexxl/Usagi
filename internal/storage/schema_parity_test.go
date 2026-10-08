package storage

import (
	"context"
	"path/filepath"
	"testing"
)

func TestRustV14SemanticSchemaParity(t *testing.T) {
	ctx := context.Background()
	rust := openTestSQLite(t, filepath.Join("..", "..", "tests", "fixtures", "storage", "legacy", "v14", "expected-v14.sqlite3"), inspectionURIParams())
	expected, err := inspectSchema(ctx, rust)
	if err != nil {
		t.Fatal(err)
	}
	actual, err := buildReferenceSnapshot()
	if err != nil {
		t.Fatal(err)
	}
	for n, table := range actual.tables {
		if table.name == "schema_meta" {
			actual.tables = append(actual.tables[:n], actual.tables[n+1:]...)
			break
		}
	}
	if err := compareSchemaSnapshots(expected, actual); err != nil {
		t.Fatal(err)
	}
}
