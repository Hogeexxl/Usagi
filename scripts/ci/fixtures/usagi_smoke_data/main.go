package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"time"

	"github.com/Hogeexxl/Usagi/internal/database"
)

func main() {
	if len(os.Args) != 3 {
		fatal("usage: usagi_smoke_data <seed|verify> <sqlite_path>")
	}
	switch os.Args[1] {
	case "seed":
		seed(os.Args[2])
	case "verify":
		verify(os.Args[2])
	default:
		fatal("unknown command")
	}
}

func seed(path string) {
	db, err := database.Open(path)
	if err != nil {
		fatal(err.Error())
	}
	defer db.Close()

	now := time.Now()
	day := now.Format(time.DateOnly)
	tx, err := db.BeginTx(context.Background(), nil)
	if err != nil {
		fatal(err.Error())
	}
	defer tx.Rollback()
	for _, statement := range []string{
		"DELETE FROM requests",
		"DELETE FROM sessions",
		"UPDATE app_meta SET value = '1' WHERE key = 'initial_sync_complete'",
		"UPDATE app_meta SET value = '1' WHERE key = 'data_revision'",
	} {
		if _, err := tx.ExecContext(context.Background(), statement); err != nil {
			fatal(err.Error())
		}
	}
	if _, err := tx.ExecContext(context.Background(), `
		INSERT INTO sessions (
			agent, id, cwd, title, start_ns, last_ns, first_seen_ns, last_seen_ns
		) VALUES ('codex', 'seeded-offline-session', 'C:\fixture\Offline Usagi',
			'Seeded offline SQLite session', ?, ?, ?, ?)`,
		now.UnixNano(), now.UnixNano(), now.UnixNano(), now.UnixNano(),
	); err != nil {
		fatal(err.Error())
	}
	if _, err := tx.ExecContext(context.Background(), `
		INSERT INTO requests (
			t_ns, local_day, agent, agent_name, provider, provider_name, model,
			session, source, status, input, output
		) VALUES (?, ?, 'codex', 'Codex', 'openai', 'OpenAI', 'gpt-6-luna',
			'seeded-offline-session', 'seeded-sqlite', 200, 10, 5)`,
		now.UnixNano(), day,
	); err != nil {
		fatal(err.Error())
	}
	if err := tx.Commit(); err != nil {
		fatal(err.Error())
	}
}

func verify(path string) {
	db, err := database.Open(path)
	if err != nil {
		fatal(err.Error())
	}
	defer db.Close()

	var schemaVersion, initialSync, dataRevision string
	var requestCount, input, output int
	var seededCount, seededInput, seededOutput int
	var fixtureCount, fixtureInput, fixtureOutput int
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'schema_version'").Scan(&schemaVersion); err != nil {
		fatal(err.Error())
	}
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'initial_sync_complete'").Scan(&initialSync); err != nil {
		fatal(err.Error())
	}
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'data_revision'").Scan(&dataRevision); err != nil {
		fatal(err.Error())
	}
	if err := db.QueryRow("SELECT COUNT(*), COALESCE(SUM(input), 0), COALESCE(SUM(output), 0) FROM requests").Scan(&requestCount, &input, &output); err != nil {
		fatal(err.Error())
	}
	if err := db.QueryRow(`
		SELECT COUNT(*), COALESCE(SUM(input), 0), COALESCE(SUM(output), 0)
		FROM requests WHERE source = 'seeded-sqlite'`,
	).Scan(&seededCount, &seededInput, &seededOutput); err != nil {
		fatal(err.Error())
	}
	if err := db.QueryRow(`
		SELECT COUNT(*), COALESCE(SUM(input), 0), COALESCE(SUM(output), 0)
		FROM requests WHERE source = 'magpie-windows-fixture'`,
	).Scan(&fixtureCount, &fixtureInput, &fixtureOutput); err != nil {
		fatal(err.Error())
	}
	if err := json.NewEncoder(os.Stdout).Encode(map[string]any{
		"schema_version":        schemaVersion,
		"initial_sync_complete": initialSync,
		"data_revision":         dataRevision,
		"request_count":         requestCount,
		"input":                 input,
		"output":                output,
		"seeded_count":          seededCount,
		"seeded_input":          seededInput,
		"seeded_output":         seededOutput,
		"fixture_count":         fixtureCount,
		"fixture_input":         fixtureInput,
		"fixture_output":        fixtureOutput,
	}); err != nil {
		fatal(err.Error())
	}
}

func fatal(message string) {
	fmt.Fprintln(os.Stderr, message)
	os.Exit(1)
}
