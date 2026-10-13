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
	} else {
		var version string
		if err := tx.QueryRowContext(ctx, "SELECT value FROM app_meta WHERE key = 'schema_version'").Scan(&version); err != nil {
			return fmt.Errorf("database: read schema version: %w", err)
		}
		if version != "1" {
			return fmt.Errorf("database: unsupported schema version %q", version)
		}
	}

	if err := tx.Commit(); err != nil {
		return fmt.Errorf("database: commit migration: %w", err)
	}
	return nil
}
