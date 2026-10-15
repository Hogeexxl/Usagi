package database

import (
	"context"
	"database/sql"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"strings"

	_ "embed"
	_ "modernc.org/sqlite"
)

//go:embed schema.sql
var schemaSQL string

func Open(path string) (*sql.DB, error) {
	absolutePath, err := filepath.Abs(path)
	if err != nil {
		return nil, fmt.Errorf("database: resolve path: %w", err)
	}

	dsn := url.URL{Scheme: "file", Path: filepath.ToSlash(absolutePath)}
	query := url.Values{}
	query.Add("_pragma", "busy_timeout(5000)")
	query.Add("_pragma", "synchronous(FULL)")
	dsn.RawQuery = query.Encode()

	db, err := sql.Open("sqlite", dsn.String())
	if err != nil {
		return nil, fmt.Errorf("database: open: %w", err)
	}
	db.SetMaxOpenConns(4)
	db.SetMaxIdleConns(4)

	closeOnError := func(err error) (*sql.DB, error) {
		if closeErr := db.Close(); closeErr != nil {
			err = fmt.Errorf("%w; close database: %v", err, closeErr)
		}
		return nil, err
	}

	ctx := context.Background()
	if err := db.PingContext(ctx); err != nil {
		return closeOnError(fmt.Errorf("database: connect: %w", err))
	}
	if err := os.Chmod(absolutePath, 0600); err != nil {
		return closeOnError(fmt.Errorf("database: set file permissions: %w", err))
	}

	var journalMode string
	if err := db.QueryRowContext(ctx, "PRAGMA journal_mode = WAL").Scan(&journalMode); err != nil {
		return closeOnError(fmt.Errorf("database: enable WAL: %w", err))
	}
	if !strings.EqualFold(journalMode, "wal") {
		return closeOnError(fmt.Errorf("database: journal mode is %q, want WAL", journalMode))
	}

	if err := migrate(ctx, db); err != nil {
		return closeOnError(err)
	}
	return db, nil
}

func migrate(ctx context.Context, db *sql.DB) error {
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("database: begin migration: %w", err)
	}
	defer tx.Rollback()

	var objectCount int
	if err := tx.QueryRowContext(ctx, `
		SELECT COUNT(*)
		FROM sqlite_master
		WHERE name NOT LIKE 'sqlite_%'
	`).Scan(&objectCount); err != nil {
		return fmt.Errorf("database: inspect schema: %w", err)
	}

	if objectCount == 0 {
		if _, err := tx.ExecContext(ctx, schemaSQL); err != nil {
			return fmt.Errorf("database: apply schema version 1: %w", err)
		}
	}
	var version string
	if err := tx.QueryRowContext(ctx, "SELECT value FROM app_meta WHERE key = 'schema_version'").Scan(&version); err != nil {
		return fmt.Errorf("database: read schema version: %w", err)
	}
	switch version {
	case "1":
		if err := migrateV1ToV2(ctx, tx); err != nil {
			return err
		}
		fallthrough
	case "2":
		if err := migrateV2ToV3(ctx, tx); err != nil {
			return err
		}
		fallthrough
	case "3":
		if err := migrateV3ToV4(ctx, tx); err != nil {
			return err
		}
	case "4":
	default:
		return fmt.Errorf("database: unsupported schema version %q", version)
	}

	if err := tx.Commit(); err != nil {
		return fmt.Errorf("database: commit migration: %w", err)
	}
	return nil
}

func migrateV3ToV4(ctx context.Context, tx *sql.Tx) error {
	for _, statement := range []string{
		"ALTER TABLE requests ADD COLUMN handed_on INTEGER NOT NULL DEFAULT 0",
		"ALTER TABLE requests ADD COLUMN first_reasoning_ms INTEGER NOT NULL DEFAULT 0",
		"ALTER TABLE requests ADD COLUMN reasoning_hidden INTEGER NOT NULL DEFAULT 0",
		"ALTER TABLE sync_days ADD COLUMN request_metrics_backfill_pending INTEGER NOT NULL DEFAULT 0",
		`INSERT OR IGNORE INTO sync_days (day, request_metrics_backfill_pending)
			SELECT DISTINCT local_day, 1 FROM requests`,
		`UPDATE sync_days SET request_metrics_backfill_pending = 1 WHERE EXISTS (
			SELECT 1 FROM requests WHERE requests.local_day = sync_days.day
		)`,
		"UPDATE app_meta SET value = '4' WHERE key = 'schema_version'",
	} {
		if _, err := tx.ExecContext(ctx, statement); err != nil {
			return fmt.Errorf("database: migrate schema version 3 to 4: %w", err)
		}
	}
	return nil
}

func migrateV2ToV3(ctx context.Context, tx *sql.Tx) error {
	for _, statement := range []string{
		"ALTER TABLE sessions ADD COLUMN nickname TEXT NOT NULL DEFAULT ''",
		"ALTER TABLE requests ADD COLUMN compaction INTEGER",
		"ALTER TABLE sync_days ADD COLUMN compaction_backfill_pending INTEGER NOT NULL DEFAULT 0",
		`INSERT OR IGNORE INTO sync_days (day, compaction_backfill_pending)
			SELECT DISTINCT local_day, 1 FROM requests WHERE agent = 'codex'`,
		`UPDATE sync_days SET compaction_backfill_pending = 1 WHERE EXISTS (
			SELECT 1 FROM requests WHERE requests.local_day = sync_days.day AND requests.agent = 'codex'
		)`,
		"UPDATE app_meta SET value = '3' WHERE key = 'schema_version'",
	} {
		if _, err := tx.ExecContext(ctx, statement); err != nil {
			return fmt.Errorf("database: migrate schema version 2 to 3: %w", err)
		}
	}
	return nil
}

func migrateV1ToV2(ctx context.Context, tx *sql.Tx) error {
	for _, statement := range []string{
		"ALTER TABLE sessions ADD COLUMN role TEXT NOT NULL DEFAULT ''",
		"ALTER TABLE sessions ADD COLUMN parent_session TEXT NOT NULL DEFAULT ''",
		"ALTER TABLE sessions ADD COLUMN root_session TEXT NOT NULL DEFAULT ''",
		"ALTER TABLE requests ADD COLUMN session_role TEXT NOT NULL DEFAULT ''",
		"ALTER TABLE sync_days ADD COLUMN identity_backfill_pending INTEGER NOT NULL DEFAULT 0",
		"INSERT INTO app_meta (key, value) VALUES ('session_identity_backfill_started', '0')",
		"INSERT INTO app_meta (key, value) VALUES ('session_identity_backfill_complete', '0')",
		"UPDATE app_meta SET value = '2' WHERE key = 'schema_version'",
	} {
		if _, err := tx.ExecContext(ctx, statement); err != nil {
			return fmt.Errorf("database: migrate schema version 1 to 2: %w", err)
		}
	}
	return nil
}
