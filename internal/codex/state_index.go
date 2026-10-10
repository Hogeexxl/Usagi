package codex

import (
	"context"
	"database/sql"
	"math"
	"net/url"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

const maxReasonableEpochMS int64 = 253_402_300_799_999

type StateSourceStatus uint8

const (
	StateSourceUnavailable StateSourceStatus = iota
	StateSourceComplete
)

type SpawnEdgeSource uint8

const SpawnEdgeFromState SpawnEdgeSource = 1

type StateDiagnostic struct {
	Code     string
	ThreadID string
	Field    string
}

type StateThreadFact struct {
	ThreadID      string
	RolloutPath   *string
	CreatedAtMS   *int64
	UpdatedAtMS   *int64
	Archived      *bool
	Title         *string
	Name          *string
	CWD           *string
	MetadataModel *string
	AgentRoleHint *string
	AgentPath     *string
}

type SpawnEdgeFact struct {
	ParentThreadID string
	ChildThreadID  string
	Status         *string
	Source         SpawnEdgeSource
	ObservedAtMS   *int64
}

type StateSnapshot struct {
	Status           StateSourceStatus
	Threads          []StateThreadFact
	SpawnEdges       []SpawnEdgeFact
	SpawnEdgesStatus StateSourceStatus
	ThreadColumns    map[string]bool
	Diagnostics      []StateDiagnostic
}

func ReadStateSnapshot(path string) StateSnapshot {
	db, err := openSideSQLite(path)
	if err != nil {
		return StateSnapshot{Status: StateSourceUnavailable, SpawnEdgesStatus: StateSourceUnavailable,
			Diagnostics: []StateDiagnostic{{Code: "source_unavailable"}}}
	}
	defer db.Close()

	ctx := context.Background()
	conn, err := db.Conn(ctx)
	if err != nil {
		return unavailableStateSnapshot("source_unavailable")
	}
	defer conn.Close()
	if _, err := conn.ExecContext(ctx, "PRAGMA query_only=ON"); err != nil {
		return unavailableStateSnapshot("source_unavailable")
	}
	tx, err := conn.BeginTx(ctx, &sql.TxOptions{ReadOnly: true})
	if err != nil {
		return unavailableStateSnapshot("source_unavailable")
	}
	defer tx.Rollback()

	threadColumns, err := sideTableColumns(ctx, tx, "threads")
	if err != nil || len(threadColumns) == 0 {
		return unavailableStateSnapshot("missing_required_table")
	}
	if !threadColumns["id"] {
		return unavailableStateSnapshot("missing_required_column")
	}
	threads, diagnostics, err := readStateThreads(ctx, tx, threadColumns)
	if err != nil {
		return unavailableStateSnapshot("threads_unreadable")
	}

	edgeColumns, err := sideTableColumns(ctx, tx, "thread_spawn_edges")
	if err != nil {
		return StateSnapshot{Status: StateSourceComplete, Threads: threads,
			SpawnEdgesStatus: StateSourceUnavailable, ThreadColumns: stateThreadColumns(threadColumns), Diagnostics: append(diagnostics, StateDiagnostic{Code: "spawn_edges_unavailable"})}
	}
	if len(edgeColumns) == 0 {
		diagnostics = append(diagnostics, StateDiagnostic{Code: "spawn_edges_absent"})
		return StateSnapshot{Status: StateSourceComplete, Threads: threads, SpawnEdgesStatus: StateSourceUnavailable, ThreadColumns: stateThreadColumns(threadColumns), Diagnostics: diagnostics}
	}
	if !edgeColumns["parent_thread_id"] || !edgeColumns["child_thread_id"] {
		diagnostics = append(diagnostics, StateDiagnostic{Code: "spawn_edge_columns_missing"})
		return StateSnapshot{Status: StateSourceComplete, Threads: threads, SpawnEdgesStatus: StateSourceUnavailable, ThreadColumns: stateThreadColumns(threadColumns), Diagnostics: diagnostics}
	}
	edges, edgeDiagnostics, err := readSpawnEdges(ctx, tx, edgeColumns)
	if err != nil {
		diagnostics = append(diagnostics, StateDiagnostic{Code: "spawn_edges_unavailable"})
		return StateSnapshot{Status: StateSourceComplete, Threads: threads, SpawnEdgesStatus: StateSourceUnavailable, ThreadColumns: stateThreadColumns(threadColumns), Diagnostics: diagnostics}
	}
	diagnostics = append(diagnostics, edgeDiagnostics...)
	if err := tx.Commit(); err != nil {
		return unavailableStateSnapshot("source_unavailable")
	}
	return StateSnapshot{Status: StateSourceComplete, Threads: threads, SpawnEdges: edges,
		SpawnEdgesStatus: StateSourceComplete, ThreadColumns: stateThreadColumns(threadColumns), Diagnostics: diagnostics}
}

func stateThreadColumns(available map[string]bool) map[string]bool {
	const allowlist = "id rollout_path created_at created_at_ms updated_at updated_at_ms archived cwd title name model agent_role agent_path"
	columns := make(map[string]bool)
	for _, column := range strings.Fields(allowlist) {
		if available[column] {
			columns[column] = true
		}
	}
	return columns
}

func unavailableStateSnapshot(code string) StateSnapshot {
	return StateSnapshot{Status: StateSourceUnavailable, SpawnEdgesStatus: StateSourceUnavailable,
		Diagnostics: []StateDiagnostic{{Code: code}}}
}

func openSideSQLite(path string) (*sql.DB, error) {
	absolute, err := filepath.Abs(path)
	if err != nil {
		return nil, err
	}
	uri := url.URL{Scheme: "file", Path: filepath.ToSlash(absolute)}
	query := url.Values{}
	query.Set("mode", "ro")
	query.Set("_query_only", "1")
	query.Set("_busy_timeout", "2000")
	uri.RawQuery = query.Encode()
	db, err := sql.Open("sqlite", uri.String())
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1)
	if err := db.Ping(); err != nil {
		db.Close()
		return nil, err
	}
	return db, nil
}

func sideTableColumns(ctx context.Context, tx *sql.Tx, table string) (map[string]bool, error) {
	rows, err := tx.QueryContext(ctx, "PRAGMA table_info("+table+")")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	columns := make(map[string]bool)
	for rows.Next() {
		var seq, notNull, primaryKey int
		var name, columnType string
		var defaultValue any
		if err := rows.Scan(&seq, &name, &columnType, &notNull, &defaultValue, &primaryKey); err != nil {
			return nil, err
		}
		columns[name] = true
	}
	return columns, rows.Err()
}

func readStateThreads(ctx context.Context, tx *sql.Tx, available map[string]bool) ([]StateThreadFact, []StateDiagnostic, error) {
	allowlist := []string{"id", "rollout_path", "created_at", "created_at_ms", "updated_at", "updated_at_ms", "archived", "cwd", "title", "name", "model", "agent_role", "agent_path"}
	columns := make([]string, 0, len(allowlist))
	for _, column := range allowlist {
		if available[column] {
			columns = append(columns, column)
		}
	}
	sort.Strings(columns)
	positions := make(map[string]int, len(columns))
	for i, column := range columns {
		positions[column] = i
	}
	rows, err := tx.QueryContext(ctx, "SELECT "+strings.Join(columns, ",")+" FROM threads ORDER BY id")
	if err != nil {
		return nil, nil, err
	}
	defer rows.Close()
	facts := make([]StateThreadFact, 0)
	diagnostics := make([]StateDiagnostic, 0)
	for rows.Next() {
		values := make([]any, len(columns))
		destinations := make([]any, len(columns))
		for i := range values {
			destinations[i] = &values[i]
		}
		if err := rows.Scan(destinations...); err != nil {
			return nil, nil, err
		}
		id, ok := sideString(stateValue(values, positions, "id"))
		id = strings.TrimSpace(id)
		if !ok || !validMetadataID(id) {
			diagnostics = append(diagnostics, StateDiagnostic{Code: "invalid_thread_id", Field: "threads.id"})
			continue
		}
		fact := StateThreadFact{ThreadID: id}
		fact.RolloutPath = statePathValue(stateValue(values, positions, "rollout_path"), &diagnostics, id, "rollout_path")
		fact.CWD = statePathValue(stateValue(values, positions, "cwd"), &diagnostics, id, "cwd")
		fact.CreatedAtMS = stateTimeValue(stateValue(values, positions, "created_at_ms"), stateValue(values, positions, "created_at"), true, &diagnostics, id, "created_at")
		fact.UpdatedAtMS = stateTimeValue(stateValue(values, positions, "updated_at_ms"), stateValue(values, positions, "updated_at"), true, &diagnostics, id, "updated_at")
		fact.Archived = stateBoolValue(stateValue(values, positions, "archived"), &diagnostics, id)
		fact.Title = stateTextValue(stateValue(values, positions, "title"), &diagnostics, id, "title")
		fact.Name = stateTextValue(stateValue(values, positions, "name"), &diagnostics, id, "name")
		fact.MetadataModel = stateTextValue(stateValue(values, positions, "model"), &diagnostics, id, "model")
		fact.AgentRoleHint = stateTextValue(stateValue(values, positions, "agent_role"), &diagnostics, id, "agent_role")
		fact.AgentPath = stateAgentPathValue(stateValue(values, positions, "agent_path"), &diagnostics, id)
		facts = append(facts, fact)
	}
	return facts, diagnostics, rows.Err()
}

func readSpawnEdges(ctx context.Context, tx *sql.Tx, available map[string]bool) ([]SpawnEdgeFact, []StateDiagnostic, error) {
	allowlist := []string{"parent_thread_id", "child_thread_id", "status", "observed_at", "observed_at_ms", "created_at", "created_at_ms", "updated_at", "updated_at_ms"}
	columns := make([]string, 0, len(allowlist))
	for _, column := range allowlist {
		if available[column] {
			columns = append(columns, column)
		}
	}
	sort.Strings(columns)
	positions := make(map[string]int, len(columns))
	for i, column := range columns {
		positions[column] = i
	}
	rows, err := tx.QueryContext(ctx, "SELECT "+strings.Join(columns, ",")+" FROM thread_spawn_edges ORDER BY parent_thread_id,child_thread_id")
	if err != nil {
		return nil, nil, err
	}
	defer rows.Close()
	facts := make([]SpawnEdgeFact, 0)
	diagnostics := make([]StateDiagnostic, 0)
	for rows.Next() {
		values := make([]any, len(columns))
		destinations := make([]any, len(columns))
		for i := range values {
			destinations[i] = &values[i]
		}
		if err := rows.Scan(destinations...); err != nil {
			return nil, nil, err
		}
		parent, parentOK := sideString(stateValue(values, positions, "parent_thread_id"))
		child, childOK := sideString(stateValue(values, positions, "child_thread_id"))
		parent, child = strings.TrimSpace(parent), strings.TrimSpace(child)
		if !parentOK || !childOK || !validMetadataID(parent) || !validMetadataID(child) {
			diagnostics = append(diagnostics, StateDiagnostic{Code: "invalid_spawn_edge_id"})
			continue
		}
		observed := stateTimeValue(stateValue(values, positions, "observed_at_ms"), stateValue(values, positions, "observed_at"), true, &diagnostics, child, "observed_at")
		if observed == nil {
			observed = stateTimeValue(stateValue(values, positions, "created_at_ms"), stateValue(values, positions, "created_at"), true, &diagnostics, child, "created_at")
		}
		if observed == nil {
			observed = stateTimeValue(stateValue(values, positions, "updated_at_ms"), stateValue(values, positions, "updated_at"), true, &diagnostics, child, "updated_at")
		}
		facts = append(facts, SpawnEdgeFact{ParentThreadID: parent, ChildThreadID: child,
			Status: stateTextValue(stateValue(values, positions, "status"), &diagnostics, child, "status"),
			Source: SpawnEdgeFromState, ObservedAtMS: observed})
	}
	return facts, diagnostics, rows.Err()
}

func stateValue(values []any, positions map[string]int, column string) any {
	if position, ok := positions[column]; ok {
		return values[position]
	}
	return nil
}

func sideString(value any) (string, bool) {
	switch value := value.(type) {
	case string:
		return value, true
	case []byte:
		return string(value), true
	default:
		return "", false
	}
}

func stateTextValue(value any, diagnostics *[]StateDiagnostic, id, field string) *string {
	if value == nil {
		return nil
	}
	text, ok := sideString(value)
	text = strings.TrimSpace(text)
	if !ok || text == "" || hasControl(text) {
		*diagnostics = append(*diagnostics, StateDiagnostic{Code: "invalid_field", ThreadID: id, Field: field})
		return nil
	}
	return &text
}

func statePathValue(value any, diagnostics *[]StateDiagnostic, id, field string) *string {
	text, ok := sideString(value)
	if !ok || text == "" || hasControl(text) || !filepath.IsAbs(strings.TrimSpace(text)) {
		if value != nil {
			*diagnostics = append(*diagnostics, StateDiagnostic{Code: "invalid_path", ThreadID: id, Field: field})
		}
		return nil
	}
	clean := filepath.Clean(strings.TrimSpace(text))
	return &clean
}

func stateAgentPathValue(value any, diagnostics *[]StateDiagnostic, id string) *string {
	text, ok := sideString(value)
	if !ok || !validAgentPath(text) {
		if value != nil {
			*diagnostics = append(*diagnostics, StateDiagnostic{Code: "invalid_agent_path", ThreadID: id, Field: "agent_path"})
		}
		return nil
	}
	clean := normalizeAgentPathValue(text)
	return &clean
}

func validAgentPath(value string) bool {
	return normalizeAgentPathValue(value) != ""
}

func normalizeAgentPathValue(value string) string {
	return filepath.ToSlash(filepath.Clean(strings.TrimSpace(value)))
}

func stateBoolValue(value any, diagnostics *[]StateDiagnostic, id string) *bool {
	if value == nil {
		return nil
	}
	var result bool
	switch value := value.(type) {
	case int64:
		if value != 0 && value != 1 {
			*diagnostics = append(*diagnostics, StateDiagnostic{Code: "invalid_archived", ThreadID: id, Field: "archived"})
			return nil
		}
		result = value == 1
	case float64:
		if value != 0 && value != 1 {
			*diagnostics = append(*diagnostics, StateDiagnostic{Code: "invalid_archived", ThreadID: id, Field: "archived"})
			return nil
		}
		result = value == 1
	case bool:
		result = value
	default:
		*diagnostics = append(*diagnostics, StateDiagnostic{Code: "invalid_archived", ThreadID: id, Field: "archived"})
		return nil
	}
	return &result
}

func stateTimeValue(preferred, fallback any, preferredIsMillis bool, diagnostics *[]StateDiagnostic, id, field string) *int64 {
	if value := parseStateTime(preferred, preferredIsMillis); value != nil {
		return value
	}
	if value := parseStateTime(fallback, false); value != nil {
		return value
	}
	if preferred != nil || fallback != nil {
		*diagnostics = append(*diagnostics, StateDiagnostic{Code: "invalid_time", ThreadID: id, Field: field})
	}
	return nil
}

func parseStateTime(value any, asMillis bool) *int64 {
	var number float64
	switch value := value.(type) {
	case int64:
		number = float64(value)
	case float64:
		number = value
	case string:
		value = strings.TrimSpace(value)
		if parsed, err := time.Parse(time.RFC3339Nano, value); err == nil {
			millis := parsed.UnixMilli()
			if millis >= 0 {
				return &millis
			}
			return nil
		}
		parsed, err := strconv.ParseInt(value, 10, 64)
		if err != nil {
			return nil
		}
		number = float64(parsed)
	default:
		return nil
	}
	if math.IsNaN(number) || math.IsInf(number, 0) || number < 0 {
		return nil
	}
	if !asMillis {
		number *= 1000
	}
	if number > float64(maxReasonableEpochMS) {
		return nil
	}
	result := int64(math.Round(number))
	return &result
}

func validMetadataID(value string) bool { return value != "" && !hasControl(value) }

func hasControl(value string) bool {
	for _, r := range value {
		if r < 0x20 || r == 0x7f {
			return true
		}
	}
	return false
}
