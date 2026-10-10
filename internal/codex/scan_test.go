package codex

import (
	"bytes"
	"compress/gzip"
	"context"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/codex/rollout"
	codexusage "github.com/Hogeexxl/Usagi/internal/codex/usage"
	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
	"github.com/klauspost/compress/zstd"
	"github.com/google/uuid"
	_ "modernc.org/sqlite"
)

var _ source.Adapter = (*Adapter)(nil)

const (
	scanMainID       = "0190a001-0000-7000-8000-000000000001"
	scanSubagentID   = "0190a001-0000-7000-8000-000000000002"
	scanOtherID      = "0190a001-0000-7000-8000-000000000003"
	scanResponseID1  = "0190a002-0000-7000-8000-000000000001"
	scanResponseID2  = "0190a002-0000-7000-8000-000000000002"
	scanResponseID3  = "0190a002-0000-7000-8000-000000000003"
	scanTurnID1      = "0190a003-0000-7000-8000-000000000001"
	scanTurnID2      = "0190a003-0000-7000-8000-000000000002"
	scanTurnID3      = "0190a003-0000-7000-8000-000000000003"
	scanProjectModel = "gpt-6-luna"
)

type scanTestResolver struct {
	mu           sync.Mutex
	resolutions  []ConfigResolution
	calls        int
	lastResolved int
}

func (r *scanTestResolver) Resolve() ConfigResolution {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.calls++
	index := r.lastResolved
	if index >= len(r.resolutions) {
		index = len(r.resolutions) - 1
	}
	if index < 0 {
		return ConfigResolution{Err: errors.New("scan test resolver has no configured result")}
	}
	r.lastResolved++
	return r.resolutions[index]
}

func (r *scanTestResolver) set(resolutions ...ConfigResolution) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.resolutions = append([]ConfigResolution(nil), resolutions...)
	r.calls = 0
	r.lastResolved = 0
}

func (r *scanTestResolver) callCount() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.calls
}

type scanTestEnv struct {
	t          *testing.T
	home       string
	config     Config
	db         *storage.DB
	bound      *source.Storage
	adapter    *Adapter
	resolver   *scanTestResolver
	sequence   atomic.Int64
	clockValue atomic.Int64
}

func newScanTestEnv(t *testing.T) *scanTestEnv {
	t.Helper()
	home := filepath.Join(t.TempDir(), "codex-home")
	if err := os.MkdirAll(filepath.Join(home, "sessions"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(home, "archived_sessions"), 0o755); err != nil {
		t.Fatal(err)
	}
	createScanStateDB(t, filepath.Join(home, stateIndexFilename))
	if err := os.WriteFile(filepath.Join(home, sessionIndexFilename), nil, 0o600); err != nil {
		t.Fatal(err)
	}
	writeScanGlobalState(t, home, nil, nil)
	config, err := ResolveConfigFromHome(home)
	if err != nil {
		t.Fatal(err)
	}
	db, err := storage.Open(context.Background(), storage.Config{Path: filepath.Join(t.TempDir(), "shared-v14.sqlite3")})
	if err != nil {
		t.Fatal(err)
	}
	resolver := &scanTestResolver{}
	resolver.set(ConfigResolution{Config: config})
	adapter, err := NewAdapterWithResolver(resolver)
	if err != nil {
		_ = db.Close()
		t.Fatal(err)
	}
	env := &scanTestEnv{t: t, home: home, config: config, db: db, adapter: adapter, resolver: resolver}
	base := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC).UnixMilli()
	env.clockValue.Store(base)
	adapter.clock = func() int64 { return env.clockValue.Add(1) }
	descriptor, err := source.NewDescriptor(domain.SourceCodex, "Codex")
	if err != nil {
		t.Fatal(err)
	}
	run, err := source.NewStorageFactory(db).Context(context.Background(), "phase4-scan-1", descriptor)
	if err != nil {
		t.Fatal(err)
	}
	env.bound = run.Storage()
	t.Cleanup(func() {
		if err := db.Close(); err != nil {
			t.Error(err)
		}
	})
	return env
}

func createScanStateDB(t *testing.T, path string) {
	t.Helper()
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatal(err)
	}
	_, err = db.Exec(`CREATE TABLE threads(
		id TEXT PRIMARY KEY, rollout_path TEXT, created_at_ms INTEGER, updated_at_ms INTEGER,
		archived INTEGER, cwd TEXT, title TEXT, name TEXT, model TEXT, agent_role TEXT, agent_path TEXT
	);
	CREATE TABLE thread_spawn_edges(
		parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL,
		status TEXT, observed_at_ms INTEGER
	);`)
	if err != nil {
		_ = db.Close()
		t.Fatal(err)
	}
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
}

func writeScanGlobalState(t *testing.T, home string, projectless, assigned []string) {
	t.Helper()
	projectlessJSON, err := json.Marshal(projectless)
	if err != nil {
		t.Fatal(err)
	}
	assignments := make(map[string]string, len(assigned))
	for _, id := range assigned {
		assignments[id] = "project"
	}
	assignmentsJSON, err := json.Marshal(assignments)
	if err != nil {
		t.Fatal(err)
	}
	data := []byte(fmt.Sprintf(`{"projectless-thread-ids":%s,"thread-project-assignments":%s}`, projectlessJSON, assignmentsJSON))
	if err := os.WriteFile(filepath.Join(home, globalStateFilename), data, 0o600); err != nil {
		t.Fatal(err)
	}
}

func (e *scanTestEnv) addThread(id string, parent *string, rolloutPath, role string) {
	e.t.Helper()
	if _, err := uuid.Parse(id); err != nil {
		e.t.Fatalf("test thread ID %q is not a UUID: %v", id, err)
	}
	if parent != nil {
		if _, err := uuid.Parse(*parent); err != nil {
			e.t.Fatalf("test parent ID %q is not a UUID: %v", *parent, err)
		}
	}
	db, err := sql.Open("sqlite", e.config.Metadata.StateIndex)
	if err != nil {
		e.t.Fatal(err)
	}
	_, err = db.Exec(`INSERT INTO threads(
		id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role,agent_path
	) VALUES(?,?,1767225600000,1767225601000,0,?,?,?,?,?,?)`,
		id, rolloutPath, e.home, "Codex integration "+id, "Codex integration "+id, scanProjectModel, role, role)
	if err == nil && parent != nil {
		_, err = db.Exec(`INSERT INTO thread_spawn_edges(parent_thread_id,child_thread_id,status,observed_at_ms)
			VALUES(?,?,'active',1767225600000)`, *parent, id)
	}
	closeErr := db.Close()
	if err != nil {
		e.t.Fatal(err)
	}
	if closeErr != nil {
		e.t.Fatal(closeErr)
	}
	indexRecord := map[string]any{"id": id, "thread_name": "Codex integration " + id, "updated_at": "2026-01-01T00:00:01Z"}
	line, err := json.Marshal(indexRecord)
	if err != nil {
		e.t.Fatal(err)
	}
	file, err := os.OpenFile(e.config.Metadata.SessionIndex, os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		e.t.Fatal(err)
	}
	if _, err := file.Write(append(line, '\n')); err != nil {
		_ = file.Close()
		e.t.Fatal(err)
	}
	if err := file.Close(); err != nil {
		e.t.Fatal(err)
	}
}

func (e *scanTestEnv) rolloutPath(threadID string, segment int) string {
	name := "rollout-" + threadID
	if segment > 0 {
		name += fmt.Sprintf("_%d", segment)
	}
	return filepath.Join(e.home, "sessions", name+".jsonl")
}

func (e *scanTestEnv) writeRollout(threadID string, segment int, records ...string) string {
	e.t.Helper()
	path := e.rolloutPath(threadID, segment)
	if err := os.WriteFile(path, []byte(strings.Join(records, "\n")+"\n"), 0o600); err != nil {
		e.t.Fatal(err)
	}
	return path
}

func (e *scanTestEnv) writeRawRollout(threadID string, segment int, content []byte) string {
	e.t.Helper()
	path := e.rolloutPath(threadID, segment)
	if err := os.WriteFile(path, content, 0o600); err != nil {
		e.t.Fatal(err)
	}
	return path
}

func (e *scanTestEnv) addMain(threadID string, responseIDs ...string) string {
	e.t.Helper()
	path := e.rolloutPath(threadID, 0)
	e.addThread(threadID, nil, path, "main")
	records := []string{scanSessionMeta(threadID, "main", nil, e.home), scanTurnContext(scanTurnID1, scanProjectModel, "high", "2026-01-01T00:00:02Z")}
	for i, responseID := range responseIDs {
		turn := []string{scanTurnID1, scanTurnID2, scanTurnID3}[i%3]
		records = append(records, scanResponse(responseID, threadID, threadID, turn, int64(10+i), int64(20+i)))
	}
	return e.writeRollout(threadID, 0, records...)
}

func (e *scanTestEnv) addSubagent(threadID, parentID string, responseID string) string {
	e.t.Helper()
	path := e.rolloutPath(threadID, 0)
	e.addThread(threadID, &parentID, path, "subagent")
	records := []string{
		scanSessionMeta(threadID, "subagent", &parentID, e.home),
		scanTurnContext(scanTurnID2, scanProjectModel, "medium", "2026-01-01T00:00:03Z"),
	}
	if responseID != "" {
		records = append(records, scanResponse(responseID, threadID, threadID, scanTurnID2, 12, 22))
	}
	return e.writeRollout(threadID, 0, records...)
}

func scanSessionMeta(threadID, role string, parent *string, cwd string) string {
	value := map[string]any{
		"timestamp": "2026-01-01T00:00:00Z",
		"type":      "session_meta",
		"payload": map[string]any{
			"id": threadID, "source": "cli", "agent_role": role, "cwd": cwd, "model": scanProjectModel,
		},
	}
	if parent != nil {
		value["payload"].(map[string]any)["parent_thread_id"] = *parent
	}
	data, _ := json.Marshal(value)
	return string(data)
}

func scanTurnContext(turnID, model, effort, timestamp string) string {
	data, _ := json.Marshal(map[string]any{
		"timestamp": timestamp, "type": "turn_context",
		"payload": map[string]any{"turn_id": turnID, "model": model, "effort": effort},
	})
	return string(data)
}

func scanResponse(responseID, threadID, sessionID, turnID string, input, output int64) string {
	data, _ := json.Marshal(map[string]any{
		"timestamp": "2026-01-01T00:00:04Z", "type": "token_usage_record",
		"payload": map[string]any{
			"response_id": responseID, "thread_id": threadID, "session_id": sessionID, "turn_id": turnID,
			"usage": map[string]any{
				"input_tokens": input, "cached_input_tokens": int64(0), "output_tokens": output,
				"reasoning_output_tokens": int64(0), "total_tokens": input + output,
			},
		},
	})
	return string(data)
}

func (e *scanTestEnv) runScan(ctx context.Context) error {
	e.t.Helper()
	sequence := e.sequence.Add(1)
	descriptor, err := source.NewDescriptor(domain.SourceCodex, "Codex")
	if err != nil {
		e.t.Fatal(err)
	}
	run, err := source.NewStorageFactory(e.db).Context(ctx, fmt.Sprintf("phase4-scan-%d", sequence), descriptor)
	if err != nil {
		e.t.Fatal(err)
	}
	return e.adapter.RunScan(ctx, run)
}

func (e *scanTestEnv) mustRunScan() {
	e.t.Helper()
	if err := e.runScan(context.Background()); err != nil {
		e.t.Fatal(err)
	}
}

func (e *scanTestEnv) assertRunCode(want string) error {
	e.t.Helper()
	err := e.runScan(context.Background())
	if got := source.ErrorCode(err); got != want {
		e.t.Fatalf("RunScan error code = %q, err=%v; want %q", got, err, want)
	}
	return err
}

func (e *scanTestEnv) sourceRows() []scanSourceRow {
	e.t.Helper()
	rows := make([]scanSourceRow, 0)
	err := e.bound.PrivateRead(func(reader storage.PrivateReader) error {
		result, err := reader.Query(`SELECT source_file_id,thread_id,current_path,source_area,device_id,inode,file_generation,
		observed_size,observed_mtime_ns,file_status FROM codex_source_files ORDER BY source_file_id`)
		if err != nil {
			return err
		}
		defer result.Close()
		for result.Next() {
			var row scanSourceRow
			if err := result.Scan(&row.ID, &row.ThreadID, &row.Path, &row.Area, &row.Device, &row.Inode,
				&row.Generation, &row.Size, &row.MTimeNS, &row.Status); err != nil {
				return err
			}
			rows = append(rows, row)
		}
		return result.Err()
	})
	if err != nil {
		e.t.Fatal(err)
	}
	return rows
}

type scanSourceRow struct {
	ID         int64
	ThreadID   sql.NullString
	Path       string
	Area       string
	Device     int64
	Inode      int64
	Generation int64
	Size       int64
	MTimeNS    int64
	Status     string
}

func (e *scanTestEnv) sourceRowByPath(path string) scanSourceRow {
	e.t.Helper()
	canonical, err := filepath.Abs(path)
	if err != nil {
		e.t.Fatal(err)
	}
	for _, row := range e.sourceRows() {
		if row.Path == canonical {
			return row
		}
	}
	e.t.Fatalf("source catalog has no path %q", canonical)
	return scanSourceRow{}
}

func (e *scanTestEnv) scalar(query string, args ...any) int64 {
	e.t.Helper()
	var value int64
	if err := e.bound.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow(query, args...).Scan(&value)
	}); err != nil {
		e.t.Fatal(err)
	}
	return value
}

func (e *scanTestEnv) checkpoint(sourceID int64, consumer string) scanCheckpoint {
	e.t.Helper()
	var got scanCheckpoint
	if err := e.bound.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow(`SELECT parser_version,committed_offset,guard_hash,processing_status,
		last_successful_scan_at_ms,last_error_code FROM codex_source_checkpoints
		WHERE source_file_id=? AND consumer_kind=?`, sourceID, consumer).
			Scan(&got.Parser, &got.Offset, &got.Guard, &got.Status, &got.LastSuccess, &got.LastError)
	}); err != nil {
		e.t.Fatal(err)
	}
	return got
}

type scanCheckpoint struct {
	Parser      int64
	Offset      int64
	Guard       []byte
	Status      string
	LastSuccess sql.NullInt64
	LastError   sql.NullString
}

func (e *scanTestEnv) usageEpoch() domain.SourceUsageEpochState {
	e.t.Helper()
	state, found, err := e.bound.LoadUsageEpoch()
	if err != nil || !found {
		e.t.Fatalf("LoadUsageEpoch found=%t err=%v", found, err)
	}
	return state
}

type scanThreadProjection struct {
	ThreadID             string  `json:"thread_id"`
	Source               string  `json:"source"`
	NativeSessionID      string  `json:"native_session_id"`
	ParentThreadID       *string `json:"parent_thread_id"`
	RootSessionID        *string `json:"root_session_id"`
	AgentRole            string  `json:"agent_role"`
	Title                *string `json:"title"`
	ProjectName          *string `json:"project_name"`
	ProjectPath          *string `json:"project_path"`
	ProjectKind          string  `json:"project_kind"`
	MetadataModel        *string `json:"metadata_model"`
	CreatedAtMS          *int64  `json:"created_at_ms"`
	UpdatedAtMS          *int64  `json:"updated_at_ms"`
	Archived             bool    `json:"archived"`
	MetadataQualityStatus string  `json:"metadata_quality_status"`
}

type scanUsageProjection struct {
	EventID          string  `json:"event_id"`
	EventKind        string  `json:"event_kind"`
	OccurredAtMS     int64   `json:"occurred_at_ms"`
	ThreadID         string  `json:"thread_id"`
	RootSessionID    string  `json:"root_session_id"`
	TurnKey          *string `json:"turn_key"`
	Model            string  `json:"model"`
	ReasoningEffort  *string `json:"reasoning_effort"`
	InputTokens      int64   `json:"input_tokens"`
	CachedTokens     int64   `json:"cached_tokens"`
	CacheWriteTokens *int64  `json:"cache_write_tokens"`
	OutputTokens     int64   `json:"output_tokens"`
	ReasoningTokens  int64   `json:"reasoning_tokens"`
	TotalTokens      int64   `json:"total_tokens"`
	EstimatedCost    *int64  `json:"estimated_cost_nanos_usd"`
}

type scanCompactionEventProjection struct {
	EventID        string  `json:"event_id"`
	ThreadID       string  `json:"thread_id"`
	RootSessionID  string  `json:"root_session_id"`
	Model          string  `json:"model"`
	ReasoningEffort *string `json:"reasoning_effort"`
	OccurredAtMS   int64   `json:"occurred_at_ms"`
	TotalTokens    int64   `json:"total_tokens"`
}

type scanCompactionScopeProjection struct {
	ThreadID       string  `json:"thread_id"`
	RootSessionID  string  `json:"root_session_id"`
	Model          string  `json:"model"`
	ReasoningEffort *string `json:"reasoning_effort"`
	StartMS        *int64  `json:"start_ms"`
	EndMS          *int64  `json:"end_ms"`
}

type scanCompactionProjection struct {
	Ready         bool                            `json:"ready"`
	Events        []scanCompactionEventProjection `json:"events"`
	UnknownScopes []scanCompactionScopeProjection `json:"unknown_scopes"`
}

type scanSkillProjection struct {
	RootSessionID string  `json:"root_session_id"`
	OccurredAtMS  int64   `json:"occurred_at_ms"`
	Model         *string `json:"model"`
	SkillName     string  `json:"skill_name"`
	Multiplicity  int64   `json:"multiplicity"`
}

type scanQuarantineProjection struct {
	RootSessionID    string `json:"root_session_id"`
	PrimaryErrorCode string `json:"primary_error_code"`
	LastActivityAtMS int64  `json:"last_activity_at_ms"`
}

type scanFinalProjection struct {
	Threads          []scanThreadProjection          `json:"threads"`
	UsageEvents      []scanUsageProjection           `json:"usage_events"`
	VisibleCompaction scanCompactionProjection         `json:"visible_compaction"`
	Skills           []scanSkillProjection           `json:"skills"`
	Quarantine       []scanQuarantineProjection      `json:"quarantine"`
}

func finalScanProjection(t *testing.T, env *scanTestEnv) scanFinalProjection {
	t.Helper()
	projection := scanFinalProjection{
		Threads: make([]scanThreadProjection, 0), UsageEvents: make([]scanUsageProjection, 0),
		Skills: make([]scanSkillProjection, 0), Quarantine: make([]scanQuarantineProjection, 0),
	}
	err := env.bound.Write(func(tx *source.WriteTx) error {
		state, err := tx.UsageEpochState()
		if err != nil {
			return err
		}
		compaction, err := ActiveCompactionVisibilityProjection(tx)
		if err != nil {
			return err
		}
		projection.VisibleCompaction = normalizeCompactionProjection(compaction, nil)
		if err := tx.Private(func(private storage.PrivateTx) error {
			rows, err := private.Query(`SELECT thread_id,source,native_session_id,parent_thread_id,root_session_id,
			agent_role,title,project_name,project_path,project_kind,metadata_model,created_at_ms,updated_at_ms,
			archived,metadata_quality_status FROM threads WHERE source='codex' ORDER BY thread_id`)
			if err != nil {
				return err
			}
			for rows.Next() {
				var value scanThreadProjection
				var parent, root, title, projectName, projectPath, model sql.NullString
				var created, updated sql.NullInt64
				var archived int64
				if err := rows.Scan(&value.ThreadID, &value.Source, &value.NativeSessionID, &parent, &root,
					&value.AgentRole, &title, &projectName, &projectPath, &value.ProjectKind, &model,
					&created, &updated, &archived, &value.MetadataQualityStatus); err != nil {
					_ = rows.Close()
					return err
				}
				value.ParentThreadID = scanNullableString(parent)
				value.RootSessionID = scanNullableString(root)
				value.Title = scanNullableString(title)
				value.ProjectName = scanNullableString(projectName)
				value.ProjectPath = scanNullableString(projectPath)
				value.MetadataModel = scanNullableString(model)
				value.CreatedAtMS = scanNullableInt(created)
				value.UpdatedAtMS = scanNullableInt(updated)
				value.Archived = archived != 0
				projection.Threads = append(projection.Threads, value)
			}
			if err := rows.Err(); err != nil {
				_ = rows.Close()
				return err
			}
			if err := rows.Close(); err != nil {
				return err
			}
			if state.ActiveEpoch > 0 {
				rows, err = private.Query(`SELECT event_id,event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,
				model,reasoning_effort,input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,total_tokens
				FROM usage_events WHERE source='codex' AND source_epoch=? ORDER BY event_id`, state.ActiveEpoch)
				if err != nil {
					return err
				}
				for rows.Next() {
					var value scanUsageProjection
					var turn, effort sql.NullString
					var cacheWrite sql.NullInt64
					if err := rows.Scan(&value.EventID, &value.EventKind, &value.OccurredAtMS, &value.ThreadID,
						&value.RootSessionID, &turn, &value.Model, &effort, &value.InputTokens,
						&value.CachedTokens, &cacheWrite, &value.OutputTokens, &value.ReasoningTokens,
						&value.TotalTokens); err != nil {
						_ = rows.Close()
						return err
					}
					value.TurnKey = scanNullableString(turn)
					value.ReasoningEffort = scanNullableString(effort)
					value.CacheWriteTokens = scanNullableInt(cacheWrite)
					value.EstimatedCost = nil
					projection.UsageEvents = append(projection.UsageEvents, value)
				}
				if err := rows.Err(); err != nil {
					_ = rows.Close()
					return err
				}
				if err := rows.Close(); err != nil {
					return err
				}
				rows, err = private.Query(`SELECT root_session_id,occurred_at_ms,model,skill_name,COUNT(*)
				FROM codex_skill_usage_events WHERE ledger_epoch=?
				GROUP BY root_session_id,occurred_at_ms,model,skill_name
				ORDER BY root_session_id,occurred_at_ms,model,skill_name`, state.ActiveEpoch)
				if err != nil {
					return err
				}
				for rows.Next() {
					var value scanSkillProjection
					var model sql.NullString
					if err := rows.Scan(&value.RootSessionID, &value.OccurredAtMS, &model, &value.SkillName, &value.Multiplicity); err != nil {
						_ = rows.Close()
						return err
					}
					value.Model = scanNullableString(model)
					projection.Skills = append(projection.Skills, value)
				}
				if err := rows.Err(); err != nil {
					_ = rows.Close()
					return err
				}
				if err := rows.Close(); err != nil {
					return err
				}
				rows, err = private.Query(`SELECT root_session_id,primary_error_code,last_activity_at_ms
				FROM codex_usage_session_quarantine WHERE ledger_epoch=? ORDER BY root_session_id`, state.ActiveEpoch)
				if err != nil {
					return err
				}
				for rows.Next() {
					var value scanQuarantineProjection
					if err := rows.Scan(&value.RootSessionID, &value.PrimaryErrorCode, &value.LastActivityAtMS); err != nil {
						_ = rows.Close()
						return err
					}
					projection.Quarantine = append(projection.Quarantine, value)
				}
				if err := rows.Err(); err != nil {
					_ = rows.Close()
					return err
				}
				return rows.Close()
			}
			return nil
		}); err != nil {
			return err
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	sort.Slice(projection.Threads, func(i, j int) bool { return projection.Threads[i].ThreadID < projection.Threads[j].ThreadID })
	sort.Slice(projection.UsageEvents, func(i, j int) bool { return projection.UsageEvents[i].EventID < projection.UsageEvents[j].EventID })
	sort.Slice(projection.Skills, func(i, j int) bool {
		a, b := projection.Skills[i], projection.Skills[j]
		if a.RootSessionID != b.RootSessionID {
			return a.RootSessionID < b.RootSessionID
		}
		if a.OccurredAtMS != b.OccurredAtMS {
			return a.OccurredAtMS < b.OccurredAtMS
		}
		if stringPointerValue(a.Model) != stringPointerValue(b.Model) {
			return stringPointerValue(a.Model) < stringPointerValue(b.Model)
		}
		return a.SkillName < b.SkillName
	})
	sort.Slice(projection.Quarantine, func(i, j int) bool { return projection.Quarantine[i].RootSessionID < projection.Quarantine[j].RootSessionID })
	return projection
}

func normalizeCompactionProjection(value codexusage.CompactionVisibilityProjection, err error) scanCompactionProjection {
	if err != nil {
		return scanCompactionProjection{}
	}
	result := scanCompactionProjection{Ready: value.Ready,
		Events: make([]scanCompactionEventProjection, 0, len(value.Events)),
		UnknownScopes: make([]scanCompactionScopeProjection, 0, len(value.UnknownScopes))}
	for _, event := range value.Events {
		result.Events = append(result.Events, scanCompactionEventProjection{
			EventID: event.EventID, ThreadID: event.ThreadID, RootSessionID: event.RootSessionID,
			Model: event.Model, ReasoningEffort: cloneString(event.ReasoningEffort),
			OccurredAtMS: event.OccurredAtMS, TotalTokens: event.TotalTokens,
		})
	}
	for _, scope := range value.UnknownScopes {
		result.UnknownScopes = append(result.UnknownScopes, scanCompactionScopeProjection{
			ThreadID: scope.ThreadID, RootSessionID: scope.RootSessionID, Model: scope.Model,
			ReasoningEffort: cloneString(scope.ReasoningEffort), StartMS: cloneInt64(scope.StartMS), EndMS: cloneInt64(scope.EndMS),
		})
	}
	sort.Slice(result.Events, func(i, j int) bool {
		a, b := result.Events[i], result.Events[j]
		if a.ThreadID != b.ThreadID { return a.ThreadID < b.ThreadID }
		if a.Model != b.Model { return a.Model < b.Model }
		if stringPointerValue(a.ReasoningEffort) != stringPointerValue(b.ReasoningEffort) { return stringPointerValue(a.ReasoningEffort) < stringPointerValue(b.ReasoningEffort) }
		if a.OccurredAtMS != b.OccurredAtMS { return a.OccurredAtMS < b.OccurredAtMS }
		return a.EventID < b.EventID
	})
	sort.Slice(result.UnknownScopes, func(i, j int) bool {
		a, b := result.UnknownScopes[i], result.UnknownScopes[j]
		if a.ThreadID != b.ThreadID { return a.ThreadID < b.ThreadID }
		if a.RootSessionID != b.RootSessionID { return a.RootSessionID < b.RootSessionID }
		if a.Model != b.Model { return a.Model < b.Model }
		if stringPointerValue(a.ReasoningEffort) != stringPointerValue(b.ReasoningEffort) { return stringPointerValue(a.ReasoningEffort) < stringPointerValue(b.ReasoningEffort) }
		if nullableIntValue(a.StartMS) != nullableIntValue(b.StartMS) { return nullableIntValue(a.StartMS) < nullableIntValue(b.StartMS) }
		return nullableIntValue(a.EndMS) < nullableIntValue(b.EndMS)
	})
	return result
}

func scanNullableString(value sql.NullString) *string {
	if !value.Valid { return nil }
	return &value.String
}

func scanNullableInt(value sql.NullInt64) *int64 {
	if !value.Valid { return nil }
	return &value.Int64
}

func stringPointerValue(value *string) string {
	if value == nil { return "" }
	return *value
}

func nullableIntValue(value *int64) int64 {
	if value == nil { return -1 }
	return *value
}

func normalizedJSON(t *testing.T, value any) []byte {
	t.Helper()
	encoded, err := json.Marshal(value)
	if err != nil { t.Fatal(err) }
	var decoded any
	if err := json.Unmarshal(encoded, &decoded); err != nil { t.Fatal(err) }
	encoded, err = json.Marshal(decoded)
	if err != nil { t.Fatal(err) }
	return encoded
}

func assertNormalizedProjectionEqual(t *testing.T, want []byte, got any) {
	t.Helper()
	var expected any
	if err := json.Unmarshal(want, &expected); err != nil { t.Fatal(err) }
	var actual any
	if err := json.Unmarshal(normalizedJSON(t, got), &actual); err != nil { t.Fatal(err) }
	expectedBytes, err := json.Marshal(expected)
	if err != nil { t.Fatal(err) }
	actualBytes, err := json.Marshal(actual)
	if err != nil { t.Fatal(err) }
	if !bytes.Equal(expectedBytes, actualBytes) {
		t.Fatalf("normalized projection mismatch\nwant: %s\n got: %s", expectedBytes, actualBytes)
	}
}

func TestRunScanRealAdapterGatesAndHomeBinding(t *testing.T) {
	env := newScanTestEnv(t)
	path := env.addMain(scanMainID, scanResponseID1)
	if got := env.resolver.callCount(); got != 0 { t.Fatalf("resolver called during setup: %d", got) }
	initialRevision := env.db.CurrentRevision()

	cancelled, cancel := context.WithCancel(context.Background())
	cancel()
	err := env.runScan(cancelled)
	if source.ErrorCode(err) != "SCAN_CANCELLED" || env.resolver.callCount() != 0 {
		t.Fatalf("cancel gate: err=%v resolverCalls=%d", err, env.resolver.callCount())
	}

	other, err := source.NewDescriptor(domain.SourceAntigravity, "Antigravity")
	if err != nil { t.Fatal(err) }
	wrongRun, err := source.NewStorageFactory(env.db).Context(context.Background(), "phase4-wrong-source", other)
	if err != nil { t.Fatal(err) }
	if err := env.adapter.RunScan(context.Background(), wrongRun); source.ErrorCode(err) != "SOURCE_MISMATCH" || env.resolver.callCount() != 0 {
		t.Fatalf("source gate: err=%v resolverCalls=%d", err, env.resolver.callCount())
	}

	configBHome := filepath.Join(t.TempDir(), "home-b")
	createMinimalScanHome(t, configBHome)
	configB, err := ResolveConfigFromHome(configBHome)
	if err != nil { t.Fatal(err) }
	env.resolver.set(ConfigResolution{Config: env.config}, ConfigResolution{Config: configB})
	if err := env.runScan(context.Background()); err != nil { t.Fatalf("first real scan: %v", err) }
	if got := env.resolver.callCount(); got != 1 { t.Fatalf("RunScan resolver calls=%d, want one", got) }
	if got := env.sourceRowByPath(path); got.Status != "present" || !got.ThreadID.Valid || got.ThreadID.String != scanMainID {
		t.Fatalf("first real scan catalog row=%+v", got)
	}
	projection := finalScanProjection(t, env)
	if len(projection.Threads) != 1 || len(projection.UsageEvents) != 1 || projection.Threads[0].AgentRole != "main" || projection.Threads[0].RootSessionID == nil || *projection.Threads[0].RootSessionID != scanMainID {
		t.Fatalf("real Adapter.RunScan → runCodexScan projection = %+v", projection)
	}
	if env.db.CurrentRevision().StatusRevision != initialRevision.StatusRevision+1 {
		t.Fatalf("first home binding status revision=%d, initial=%d", env.db.CurrentRevision().StatusRevision, initialRevision.StatusRevision)
	}

	beforeSwitch := env.db.CurrentRevision()
	if err := env.assertRunCode("SOURCE_CHANGED"); err == nil { t.Fatal("Home A→Home B did not return SOURCE_CHANGED") }
	if got := env.resolver.callCount(); got != 1 { t.Fatalf("second RunScan resolver calls=%d, want one", got) }
	var status string
	var fingerprint sql.NullString
	if err := env.bound.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow("SELECT binding_status,home_fingerprint FROM codex_adapter_state WHERE id=1").Scan(&status, &fingerprint)
	}); err != nil { t.Fatal(err) }
	if status != "source_changed" || !fingerprint.Valid || fingerprint.String != env.config.HomeFingerprint || env.db.CurrentRevision().StatusRevision != beforeSwitch.StatusRevision+1 {
		t.Fatalf("Home switch binding=(%s,%v), revision=%+v", status, fingerprint, env.db.CurrentRevision())
	}
	if err := env.assertRunCode("SOURCE_CHANGED"); err == nil { t.Fatal("latched source_changed binding was rebound") }
	if env.db.CurrentRevision().StatusRevision != beforeSwitch.StatusRevision+1 {
		t.Fatal("latched SOURCE_CHANGED bumped status revision more than once")
	}
}

func createMinimalScanHome(t *testing.T, home string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Join(home, "sessions"), 0o755); err != nil { t.Fatal(err) }
	if err := os.MkdirAll(filepath.Join(home, "archived_sessions"), 0o755); err != nil { t.Fatal(err) }
	createScanStateDB(t, filepath.Join(home, stateIndexFilename))
	if err := os.WriteFile(filepath.Join(home, sessionIndexFilename), nil, 0o600); err != nil { t.Fatal(err) }
	writeScanGlobalState(t, home, nil, nil)
}

func TestRunScanCancellationAndInvalidConfigDoNotMutateBinding(t *testing.T) {
	env := newScanTestEnv(t)
	before := env.db.CurrentRevision()
	configErr := &ConfigError{Code: "CODEX_METADATA_HOME_MISMATCH"}
	env.resolver.set(ConfigResolution{Err: configErr})
	if err := env.assertRunCode("CODEX_METADATA_HOME_MISMATCH"); err == nil { t.Fatal("invalid config succeeded") }
	if env.db.CurrentRevision() != before { t.Fatalf("invalid config changed revision: before=%+v after=%+v", before, env.db.CurrentRevision()) }
	if got := env.scalar("SELECT COUNT(*) FROM codex_source_files"); got != 0 { t.Fatalf("invalid config reached discovery: %d catalog rows", got) }
}

func TestRunScanAppendQuorumAndIndependentConsumerCheckpoints(t *testing.T) {
	env := newScanTestEnv(t)
	path := env.addMain(scanMainID, scanResponseID1)
	env.mustRunScan()
	initial := env.sourceRowByPath(path)
	meta0 := env.checkpoint(initial.ID, rollout.ConsumerMetadata)
	usage0 := env.checkpoint(initial.ID, rollout.ConsumerUsage)
	if initial.Generation != 1 || meta0.Status != "ready" || usage0.Status != "ready" || meta0.Offset != initial.Size || usage0.Offset != initial.Size {
		t.Fatalf("initial scan proof/checkpoints: row=%+v metadata=%+v usage=%+v", initial, meta0, usage0)
	}

	appendRecord := scanResponse(scanResponseID2, scanMainID, scanMainID, scanTurnID2, 16, 26)
	appendRolloutLine(t, path, appendRecord)
	acceptedBefore := initial.Size
	if err := env.runScan(context.Background()); err != nil { t.Fatalf("plain append scan: %v", err) }
	appended := env.sourceRowByPath(path)
	if appended.Generation != initial.Generation || appended.Size <= acceptedBefore || env.checkpoint(appended.ID, "metadata").Offset != appended.Size || env.checkpoint(appended.ID, "usage").Offset != appended.Size {
		t.Fatalf("valid consumer quorum append: before=%+v after=%+v metadata=%+v usage=%+v", initial, appended,
			env.checkpoint(appended.ID, "metadata"), env.checkpoint(appended.ID, "usage"))
	}
	if got := env.scalar("SELECT COUNT(*) FROM usage_events WHERE source='codex' AND source_epoch=?", env.usageEpoch().ActiveEpoch); got != 2 {
		t.Fatalf("append produced %d active events, want 2", got)
	}

	// Corrupt only the durable Usage guard after a real successful scan. The next real scan
	// discovers a longer plain file and must invalidate the shared generation before Pass1.
	metaBefore := env.checkpoint(appended.ID, "metadata")
	if err := env.bound.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_source_checkpoints SET guard_hash=?
				WHERE source_file_id=? AND consumer_kind='usage'`, bytes.Repeat([]byte{0xa5}, 32), appended.ID)
			return err
		})
	}); err != nil { t.Fatal(err) }
	appendRolloutLine(t, path, scanResponse(scanResponseID3, scanMainID, scanMainID, scanTurnID3, 18, 28))
	if err := env.runScan(context.Background()); err != nil { t.Fatalf("guard quorum recovery scan: %v", err) }
	reset := env.sourceRowByPath(path)
	metaAfter, usageAfter := env.checkpoint(reset.ID, "metadata"), env.checkpoint(reset.ID, "usage")
	if reset.Generation != appended.Generation+1 || !reset.ThreadID.Valid || reset.ThreadID.String != scanMainID || metaAfter.Status != "ready" || usageAfter.Status != "ready" || metaAfter.Offset != reset.Size || usageAfter.Offset != reset.Size {
		t.Fatalf("guard quorum failed to invalidate and full reread: before=%+v after=%+v metadata=%+v usage=%+v", appended, reset, metaAfter, usageAfter)
	}
	if got := env.scalar("SELECT COUNT(*) FROM codex_rollout_metadata_facts WHERE source_file_id=? AND file_generation=?", reset.ID, reset.Generation-1); got != 0 {
		t.Fatalf("old generation metadata fact remains: %d", got)
	}
	if got := finalScanProjection(t, env); len(got.UsageEvents) != 3 {
		t.Fatalf("full reread projection has %d events: %+v", len(got.UsageEvents), got.UsageEvents)
	}
	if metaBefore.Status != "ready" { t.Fatalf("metadata checkpoint before quorum setup was not ready: %+v", metaBefore) }
}

func appendRolloutLine(t *testing.T, path, record string) {
	t.Helper()
	file, err := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil { t.Fatal(err) }
	if _, err := file.Write(append([]byte(record), '\n')); err != nil { _ = file.Close(); t.Fatal(err) }
	if err := file.Close(); err != nil { t.Fatal(err) }
}

func TestRunScanGoldenProjectionAgainstRustOracle(t *testing.T) {
	cases := []string{
		"metadata_relationship", "replayed_ancestor", "explicit_response", "token_count_delta_reset",
		"compaction_top_embedded", "turn_compensation", "skills", "fatal_conflicts",
	}
	for _, name := range cases {
		t.Run(name, func(t *testing.T) {
			base := filepath.Join("testdata", "golden", name)
			home := filepath.Join(base, "home")
			expectedPath := filepath.Join(base, "projection.json")
			if _, err := os.Stat(expectedPath); err != nil { t.Fatalf("Rust Oracle projection is not delivered: %v", err) }
			config, err := ResolveConfigFromHome(home)
			if err != nil { t.Fatal(err) }
			env := newScanTestEnv(t)
			env.config, env.home = config, home
			env.resolver.set(ConfigResolution{Config: config})
			if err := env.runScan(context.Background()); err != nil { t.Fatalf("real Go RunScan: %v", err) }
			actual := finalScanProjection(t, env)
			want, err := os.ReadFile(expectedPath)
			if err != nil { t.Fatal(err) }
			assertNormalizedProjectionEqual(t, want, actual)
		})
	}
}

type scanRustProofFixture struct {
	Algorithm             string             `json:"algorithm"`
	Source                string             `json:"source"`
	ActiveEpoch           int64              `json:"active_epoch"`
	ActiveParserVersion   int64              `json:"active_parser_version"`
	SourceFileID          int64              `json:"source_file_id"`
	FileGeneration        int64              `json:"file_generation"`
	SourceFileRow         scanRustTableDump   `json:"source_file_row"`
	UsageEpochRow         scanRustTableDump   `json:"usage_epoch_row"`
	CanonicalUsageEvents  scanRustTableDump   `json:"canonical_usage_events"`
	PrivateEvidenceRows   map[string]scanRustTableDump `json:"private_evidence_rows"`
	ProofCases            []scanRustProofCase `json:"proof_cases"`
}

type scanRustProofCase struct {
	Name                   string          `json:"name"`
	ReconciliationCarryJSON string          `json:"reconciliation_carry_json"`
	ExpectedHashHex        string          `json:"expected_hash_hex"`
	ActiveStateRow         scanRustTableDump `json:"active_state_row"`
}

type scanRustTableDump struct {
	Table   string            `json:"table"`
	Columns []string          `json:"columns"`
	Rows    [][]scanRustValue `json:"rows"`
}

type scanRustValue struct {
	Type       string `json:"type"`
	Value      any    `json:"value"`
	Hex        string `json:"hex"`
	IEEE754Hex string `json:"ieee754_be_hex"`
}

func TestRunScanRustCarryAndActiveSourceStateProofGolden(t *testing.T) {
	fixturePath := filepath.Join("testdata", "golden", "active_state_proof", "active_source_state_proof.json")
	fixtureBytes, err := os.ReadFile(fixturePath)
	if err != nil { t.Fatalf("Rust carry/proof fixture is not delivered: %v", err) }
	var fixture scanRustProofFixture
	if err := json.Unmarshal(fixtureBytes, &fixture); err != nil { t.Fatal(err) }
	if fixture.Algorithm != "usage-source-state-proof-v3" || fixture.Source != string(domain.SourceCodex) || fixture.ActiveEpoch <= 0 || fixture.ActiveParserVersion < 12 || fixture.SourceFileID <= 0 || fixture.FileGeneration <= 0 || len(fixture.ProofCases) != 2 {
		t.Fatalf("unexpected typed Rust proof fixture header: %+v", fixture)
	}
	config, err := ResolveConfigFromHome(filepath.Join("testdata", "golden", "active_state_proof", "home"))
	if err != nil { t.Fatal(err) }
	env := newScanTestEnv(t)
	env.home, env.config = config.Home, config
	env.resolver.set(ConfigResolution{Config: config})
	if err := env.runScan(context.Background()); err != nil { t.Fatalf("real Go proof fixture scan: %v", err) }
	for _, proofCase := range fixture.ProofCases {
		t.Run(proofCase.Name, func(t *testing.T) {
			canonical, err := codexusage.DecodeReconciliationCarryJSON([]byte(proofCase.ReconciliationCarryJSON))
			if err != nil { t.Fatalf("decode Rust carry: %v", err) }
			encoded, err := codexusage.CanonicalReconciliationCarryJSON(canonical)
			if err != nil || !bytes.Equal(encoded, []byte(proofCase.ReconciliationCarryJSON)) {
				t.Fatalf("Go carry canonicalization differs from Rust bytes: encoded=%s err=%v", encoded, err)
			}
			if err := env.bound.Write(func(tx *source.WriteTx) error {
				return tx.Private(func(private storage.PrivateTx) error {
					_, err := private.Exec(`UPDATE codex_usage_source_states SET reconciliation_state_json=?
						WHERE ledger_epoch=? AND source_file_id=?`, proofCase.ReconciliationCarryJSON, fixture.ActiveEpoch, fixture.SourceFileID)
					return err
				})
			}); err != nil { t.Fatal(err) }
			var got []byte
			if err := env.bound.Write(func(tx *source.WriteTx) error {
				var err error
				got, err = ActiveSourceStateProofV3(tx, fixture.ActiveEpoch, fixture.SourceFileID)
				return err
			}); err != nil { t.Fatal(err) }
			want, err := hex.DecodeString(proofCase.ExpectedHashHex)
			if err != nil { t.Fatal(err) }
			if !bytes.Equal(got, want) || len(got) != 32 {
				t.Fatalf("ActiveSourceStateProofV3=%x, Rust=%x", got, want)
			}
		})
	}
	valid := fixture.ProofCases[0].ReconciliationCarryJSON
	for name, mutated := range map[string]string{
		"unknown top-level": strings.Replace(valid, `"version":1`, `"version":1,"future":true`, 1),
		"unknown pending record": strings.Replace(valid, `"record":{"kind":`, `"record":{"future":true,"kind":`, 1),
		"nested unknown field": strings.Replace(valid, `"response_id":"pending-response-a"`, `"response_id":"pending-response-a","future":true`, 1),
		"noncanonical bytes": strings.Replace(valid, `"pending_response_ids":["pending-a","pending-m","pending-z"]`, `"pending_response_ids":["pending-z","pending-a","pending-m"]`, 1),
	} {
		t.Run(name, func(t *testing.T) {
			if err := env.bound.Write(func(tx *source.WriteTx) error {
				return tx.Private(func(private storage.PrivateTx) error {
					_, err := private.Exec(`UPDATE codex_usage_source_states SET reconciliation_state_json=?
						WHERE ledger_epoch=? AND source_file_id=?`, mutated, fixture.ActiveEpoch, fixture.SourceFileID)
					return err
				})
			}); err != nil { t.Fatal(err) }
			var got []byte
			err := env.bound.Write(func(tx *source.WriteTx) error {
				var err error
				got, err = ActiveSourceStateProofV3(tx, fixture.ActiveEpoch, fixture.SourceFileID)
				return err
			})
			if err == nil || got != nil { t.Fatalf("invalid durable carry produced proof %x (err=%v)", got, err) }
		})
	}
}

func TestRunScanAllTemporaryHomesUseRealSideSourcesAndValidUUIDs(t *testing.T) {
	env := newScanTestEnv(t)
	path := env.addMain(scanMainID, scanResponseID1)
	env.addSubagent(scanSubagentID, scanMainID, scanResponseID2)
	if got, err := os.Stat(env.config.Metadata.StateIndex); err != nil || !got.Mode().IsRegular() { t.Fatalf("state_5.sqlite is not a real file: %v", err) }
	if got, err := os.Stat(env.config.Metadata.SessionIndex); err != nil || !got.Mode().IsRegular() { t.Fatalf("session_index.jsonl is not a real file: %v", err) }
	if got, err := os.Stat(path); err != nil || !got.Mode().IsRegular() { t.Fatalf("rollout is not a real file: %v", err) }
	env.mustRunScan()
	projection := finalScanProjection(t, env)
	if len(projection.Threads) != 2 || len(projection.UsageEvents) != 2 { t.Fatalf("main/subagent full scan projection=%+v", projection) }
	byID := make(map[string]scanThreadProjection)
	for _, thread := range projection.Threads { byID[thread.ThreadID] = thread }
	if byID[scanMainID].AgentRole != "main" || byID[scanMainID].RootSessionID == nil || *byID[scanMainID].RootSessionID != scanMainID {
		t.Fatalf("main role/root was not resolved from explicit state facts: %+v", byID[scanMainID])
	}
	if byID[scanSubagentID].AgentRole != "subagent" || byID[scanSubagentID].ParentThreadID == nil || *byID[scanSubagentID].ParentThreadID != scanMainID || byID[scanSubagentID].RootSessionID == nil || *byID[scanSubagentID].RootSessionID != scanMainID {
		t.Fatalf("subagent role/parent/root was not resolved from explicit state facts: %+v", byID[scanSubagentID])
	}
	for _, id := range []string{scanMainID, scanSubagentID, scanResponseID1, scanResponseID2, scanTurnID1, scanTurnID2} {
		parsed, err := uuid.Parse(id)
		if err != nil || parsed.Version() != 7 { t.Fatalf("test fixture ID %q is not a valid UUIDv7: %v", id, err) }
	}
}

func zstdBytes(t *testing.T, input []byte) []byte {
	t.Helper()
	encoder, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1))
	if err != nil { t.Fatal(err) }
	defer encoder.Close()
	return encoder.EncodeAll(input, nil)
}

func writeZstdRollout(t *testing.T, plainPath string, input []byte) string {
	t.Helper()
	path := plainPath + ".zst"
	if err := os.WriteFile(path, zstdBytes(t, input), 0o600); err != nil { t.Fatal(err) }
	return path
}

func gzipBytes(t *testing.T, input []byte) []byte {
	t.Helper()
	var buffer bytes.Buffer
	writer := gzip.NewWriter(&buffer)
	if _, err := writer.Write(input); err != nil { t.Fatal(err) }
	if err := writer.Close(); err != nil { t.Fatal(err) }
	return buffer.Bytes()
}

func exactPaddedJSONLine(t *testing.T, size int, body string) []byte {
	t.Helper()
	base, err := json.Marshal(map[string]any{"type": "unknown", "padding": body})
	if err != nil { t.Fatal(err) }
	if len(base)+1 > size { t.Fatalf("base line size %d exceeds target %d", len(base)+1, size) }
	padding := strings.Repeat("x", size-len(base)-1)
	line, err := json.Marshal(map[string]any{"type": "unknown", "padding": body + padding})
	if err != nil { t.Fatal(err) }
	if len(line)+1 != size { t.Fatalf("padded JSON line size=%d, want %d", len(line)+1, size) }
	return append(line, '\n')
}

func seedActiveEpoch(t *testing.T, env *scanTestEnv, parser int64) int64 {
	t.Helper()
	var epoch int64
	if err := env.bound.Write(func(tx *source.WriteTx) error {
		if err := tx.EnsureUsageEpoch(); err != nil { return err }
		var err error
		epoch, err = tx.BeginOrResumeUsageBuild(parser)
		if err != nil { return err }
		_, err = tx.ActivateUsageBuild(epoch, parser)
		return err
	}); err != nil { t.Fatal(err) }
	return epoch
}

func seedScanCatalogRow(t *testing.T, env *scanTestEnv, sourceID int64, threadID *string, path string, identity rollout.PhysicalIdentity, generation, size, mtime int64, status string) {
	t.Helper()
	if err := env.bound.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`INSERT INTO codex_source_files(source_file_id,thread_id,current_path,source_area,
				device_id,inode,file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms)
				VALUES(?,?,?,'sessions',?,?,?,?,?,?,1)`, sourceID, threadID, path, identity.DeviceID, identity.Inode,
				generation, size, mtime, status)
			return err
		})
	}); err != nil { t.Fatal(err) }
}

func seedScanCheckpoint(t *testing.T, env *scanTestEnv, sourceID int64, consumer string, parser, offset int64, guard []byte, status string) {
	t.Helper()
	if err := env.bound.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`INSERT INTO codex_source_checkpoints(source_file_id,consumer_kind,parser_version,
				committed_offset,guard_hash,processing_status,last_successful_scan_at_ms,last_error_code)
				VALUES(?,?,?,?,?,?,7,'seeded')`, sourceID, consumer, parser, offset, guard, status)
			return err
		})
	}); err != nil { t.Fatal(err) }
}

func scanThreadIdentity(t *testing.T, id string) (domain.SessionIdentity, domain.ResolvedThreadPatch) {
	t.Helper()
	identity, err := domain.NewSessionIdentity(id, domain.SourceCodex, id)
	if err != nil { t.Fatal(err) }
	patch, err := domain.NewResolvedThreadPatch(identity, 1)
	if err != nil { t.Fatal(err) }
	patch.AgentRole = domain.Set(domain.AgentRole("main"))
	patch.RootSessionID = domain.Set(id)
	patch.ProjectKind = domain.Set(domain.ProjectKind("project"))
	patch.Archived = domain.Set(false)
	patch.MetadataQualityStatus = "complete"
	return identity, patch
}

func seedScanThread(t *testing.T, env *scanTestEnv, id string) {
	t.Helper()
	identity, patch := scanThreadIdentity(t, id)
	if err := env.bound.Write(func(tx *source.WriteTx) error {
		_, err := tx.UpsertThreadNoRevision(identity, patch)
		return err
	}); err != nil { t.Fatal(err) }
}

func scanCanonicalEvent(responseID, threadID string, input, output int64, cost *int64, created int64) sharedusage.CanonicalUsageEventWrite {
	turn := scanTurnID1
	model, effort := scanProjectModel, "high"
	return sharedusage.CanonicalUsageEventWrite{
		EventID: responseID, Kind: sharedusage.EventKindNormal, OccurredAtMS: 1767225604000,
		ThreadID: threadID, RootSessionID: threadID, TurnKey: &turn, Model: model,
		ReasoningEffort: &effort, EstimatedCostNanosUSD: cost,
		Usage: sharedusage.NormalizedTokenUsage{InputTokens: input, OutputTokens: output, TotalTokens: input + output},
		CreatedAtMS: created,
	}
}

func seedScanUsageEvent(t *testing.T, env *scanTestEnv, target source.UsageWriteTarget, event sharedusage.CanonicalUsageEventWrite) {
	t.Helper()
	if err := env.bound.Write(func(tx *source.WriteTx) error {
		_, err := tx.WriteUsageNoRevision(target, event)
		return err
	}); err != nil { t.Fatal(err) }
}

func seedActiveSourceState(t *testing.T, env *scanTestEnv, epoch, sourceID, generation, device, inode, parser, offset, observed int64, owner, root, tail string, guard []byte) {
	t.Helper()
	if err := env.bound.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`INSERT INTO codex_source_checkpoints(source_file_id,consumer_kind,parser_version,
				committed_offset,guard_hash,processing_status,last_successful_scan_at_ms,last_error_code)
				VALUES(?,'usage',?,?,?,'ready',7,NULL)`, sourceID, parser, offset, guard); err != nil { return err }
			_, err := private.Exec(`INSERT INTO codex_usage_source_states(ledger_epoch,source_file_id,file_generation,device_id,inode,
				usage_parser_version,canonical_algorithm_version,resolved_through_offset,observed_raw_size,raw_tail_status,
				raw_tail_start_offset,owning_thread_id,root_session_id,continuation_state,chain_state,updated_at_ms)
				VALUES(?,?,?,?,?,?,6,?,?,?,NULL,?,?,'owning_live','continuous',7)`,
				epoch, sourceID, generation, device, inode, parser, offset, observed, tail, owner, root)
			return err
		})
	}); err != nil { t.Fatal(err) }
}

func TestRunScanBuildBootstrapCostAndAtomicProjection(t *testing.T) {
	env := newScanTestEnv(t)
	env.addMain(scanMainID, scanResponseID1)
	env.mustRunScan()
	state := env.usageEpoch()
	if state.ActiveEpoch != 1 || state.BuildEpoch != nil || state.ActiveParserVersion != codexusage.UsageParserVersion {
		t.Fatalf("Bootstrap did not activate parser %d: %+v", codexusage.UsageParserVersion, state)
	}
	var cost sql.NullInt64
	var created int64
	if err := env.bound.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow(`SELECT estimated_cost_nanos_usd,created_at_ms FROM usage_events
			WHERE source='codex' AND source_epoch=? AND event_id=?`, state.ActiveEpoch, scanResponseID1).Scan(&cost, &created)
	}); err != nil { t.Fatal(err) }
	if cost.Valid { t.Fatalf("Bootstrap build assigned non-NULL cost: %d", cost.Int64) }
	if created <= 0 { t.Fatalf("Bootstrap event created_at_ms=%d", created) }
	if state.BuildEpoch != nil { t.Fatalf("completed Bootstrap left Build epoch: %+v", state) }
	if len(finalScanProjection(t, env).UsageEvents) != 1 { t.Fatal("Bootstrap projection missing canonical event") }
}

func TestRunScanCompressedPhysicalAndLogicalOffsets(t *testing.T) {
	env := newScanTestEnv(t)
	path := env.addMain(scanMainID)
	plain, err := os.ReadFile(path)
	if err != nil { t.Fatal(err) }
	longLine := scanResponse(scanResponseID1, scanMainID, scanMainID, scanTurnID1, 30, 40)
	payload, err := json.Marshal(map[string]any{"type": "future_record", "padding": strings.Repeat("z", 150_000)})
	if err != nil { t.Fatal(err) }
	logical := append(append(append([]byte(nil), plain...), payload...), '\n')
	logical = append(logical, longLine...)
	logical = append(logical, '\n')
	zstdPath := writeZstdRollout(t, path, logical)
	if err := os.Remove(path); err != nil { t.Fatal(err) }
	env.mustRunScan()
	var physicalOffset, observedSize, logicalEnd int64
	if err := env.bound.PrivateRead(func(reader storage.PrivateReader) error {
		if err := reader.QueryRow(`SELECT committed_offset FROM codex_source_checkpoints
			WHERE consumer_kind='usage' AND source_file_id=(SELECT source_file_id FROM codex_source_files WHERE current_path=?)`, zstdPath).Scan(&physicalOffset); err != nil { return err }
		if err := reader.QueryRow(`SELECT observed_size FROM codex_source_files WHERE current_path=?`, zstdPath).Scan(&observedSize); err != nil { return err }
		return reader.QueryRow(`SELECT MAX(source_end_offset) FROM codex_usage_event_occurrences
			WHERE ledger_epoch=?`, env.usageEpoch().ActiveEpoch).Scan(&logicalEnd)
	}); err != nil { t.Fatal(err) }
	stat, err := os.Stat(zstdPath)
	if err != nil { t.Fatal(err) }
	if physicalOffset != stat.Size() || observedSize != stat.Size() || physicalOffset > observedSize {
		t.Fatalf("compressed checkpoint is not in physical coordinates: checkpoint=%d catalog=%d file=%d", physicalOffset, observedSize, stat.Size())
	}
	if logicalEnd <= observedSize { t.Fatalf("fixture did not prove logical provenance beyond compressed bytes: logical=%d physical=%d", logicalEnd, observedSize) }
	if got := finalScanProjection(t, env); len(got.UsageEvents) != 1 { t.Fatalf("compressed full reread usage projection=%+v", got.UsageEvents) }
}

func TestRunScanPhysicalIdentityReplacementAndPathSwap(t *testing.T) {
	env := newScanTestEnv(t)
	pathX := env.addMain(scanMainID, scanResponseID1)
	pathY := env.addMain(scanOtherID, scanResponseID2)
	env.mustRunScan()
	beforeX, beforeY := env.sourceRowByPath(pathX), env.sourceRowByPath(pathY)

	// Replace the inode at X's logical path with a valid new rollout owned by a different
	// UUID. The catalog keeps its source_file_id and advances its generation.
	replacement := filepath.Join(env.home, "sessions", "replacement.jsonl")
	content := strings.Join([]string{
		scanSessionMeta(scanMainID, "main", nil, env.home),
		scanTurnContext(scanTurnID3, scanProjectModel, "high", "2026-02-01T00:00:00Z"),
		scanResponse(scanResponseID3, scanMainID, scanMainID, scanTurnID3, 19, 29),
	}, "\n") + "\n"
	if err := os.WriteFile(replacement, []byte(content), 0o600); err != nil { t.Fatal(err) }
	if err := os.Rename(replacement, pathX); err != nil { t.Fatal(err) }
	if err := os.Chtimes(pathX, time.Now().Add(2*time.Second), time.Now().Add(2*time.Second)); err != nil { t.Fatal(err) }
	if err := env.runScan(context.Background()); err != nil { t.Fatalf("same-path replacement RunScan: %v", err) }
	afterX := env.sourceRowByPath(pathX)
	if afterX.ID != beforeX.ID || afterX.Generation != beforeX.Generation+1 || afterX.Device == beforeX.Device && afterX.Inode == beforeX.Inode {
		t.Fatalf("same-path replacement did not reuse source ID and invalidate generation: before=%+v after=%+v", beforeX, afterX)
	}
	if got := env.scalar("SELECT COUNT(*) FROM codex_source_files WHERE current_path LIKE '@reconcile/%'"); got != 0 { t.Fatalf("transaction staging paths survived: %d", got) }

	// Swap two existing physical files through a temporary path. Each physical identity
	// must retain its catalog ID while the two final paths exchange owners atomically.
	swapTmp := filepath.Join(env.home, "sessions", "swap.tmp")
	if err := os.Rename(pathX, swapTmp); err != nil { t.Fatal(err) }
	if err := os.Rename(pathY, pathX); err != nil { t.Fatal(err) }
	if err := os.Rename(swapTmp, pathY); err != nil { t.Fatal(err) }
	if err := env.runScan(context.Background()); err != nil { t.Fatalf("identity path swap RunScan: %v", err) }
	rows := env.sourceRows()
	byID := make(map[int64]scanSourceRow, len(rows))
	for _, row := range rows { byID[row.ID] = row }
	if byID[beforeY.ID].Path != pathX || byID[afterX.ID].Path != pathY || byID[beforeY.ID].Generation != beforeY.Generation {
		t.Fatalf("identity continuity/path swap lost IDs: X=%+v Y=%+v", byID[afterX.ID], byID[beforeY.ID])
	}
	if got := env.scalar("SELECT COUNT(*) FROM usage_events WHERE source='codex' AND source_epoch=?", env.usageEpoch().ActiveEpoch); got != 2 {
		t.Fatalf("path swap duplicated/lost canonical contributors: count=%d", got)
	}
}

func TestRunScanZstdSameSizeRewriteInvalidatesQuarantineProof(t *testing.T) {
	env := newScanTestEnv(t)
	path := env.addMain(scanMainID, scanResponseID1)
	input, err := os.ReadFile(path)
	if err != nil { t.Fatal(err) }
	zstdPath := writeZstdRollout(t, path, input)
	if err := os.Remove(path); err != nil { t.Fatal(err) }
	env.mustRunScan()
	before := env.sourceRowByPath(zstdPath)
	compressed, err := os.ReadFile(zstdPath)
	if err != nil { t.Fatal(err) }
	changed := append([]byte(nil), input...)
	changed = append(changed, []byte(scanTurnContext(scanTurnID2, scanProjectModel, "medium", "2026-01-02T00:00:00Z")+"\n")...)
	encoded := zstdBytes(t, changed)
	if len(encoded) != len(compressed) {
		// Re-encode with a padding field so the compressed physical size stays unchanged.
		for padding := 0; padding < 10000 && len(encoded) != len(compressed); padding++ {
			candidate := append([]byte(nil), changed...)
			candidate = append(candidate, []byte(fmt.Sprintf("{\"type\":\"future\",\"padding\":\"%0*x\"}\n", padding, 0))...)
			encoded = zstdBytes(t, candidate)
		}
	}
	if len(encoded) != len(compressed) { t.Skip("zstd encoder could not produce a same-size rewrite for this runtime") }
	if err := os.WriteFile(zstdPath, encoded, 0o600); err != nil { t.Fatal(err) }
	if err := os.Chtimes(zstdPath, time.Now().Add(3*time.Second), time.Now().Add(3*time.Second)); err != nil { t.Fatal(err) }
	if err := env.runScan(context.Background()); err != nil { t.Fatalf("zstd same-size rewrite scan: %v", err) }
	after := env.sourceRowByPath(zstdPath)
	if before.Size != after.Size || before.MTimeNS == after.MTimeNS || after.Generation != before.Generation+1 {
		t.Fatalf("zstd mtime-only rewrite did not invalidate source proof: before=%+v after=%+v", before, after)
	}
}

func TestRunScanQuarantineFatalIsolationAndNoLeak(t *testing.T) {
	env := newScanTestEnv(t)
	pathA := env.rolloutPath(scanMainID, 1)
	pathB := env.rolloutPath(scanMainID, 2)
	env.addThread(scanMainID, nil, pathA, "main")
	usage := map[string]any{
		"input_tokens": int64(10), "cached_input_tokens": int64(0), "output_tokens": int64(2),
		"reasoning_output_tokens": int64(0), "total_tokens": int64(12),
	}
	response := func(value any) string {
		data, err := json.Marshal(map[string]any{"timestamp": "2026-01-01T00:00:04Z", "type": "token_usage_record", "payload": map[string]any{
			"response_id": scanResponseID1, "thread_id": scanMainID, "session_id": scanMainID, "turn_id": scanTurnID1, "usage": value,
		}})
		if err != nil { t.Fatal(err) }
		return string(data)
	}
	shared := []string{scanSessionMeta(scanMainID, "main", nil, env.home), scanTurnContext(scanTurnID1, scanProjectModel, "high", "2026-01-01T00:00:02Z")}
	if err := os.WriteFile(pathA, []byte(strings.Join(append(append([]string(nil), shared...), response(usage)), "\n")+"\n"), 0o600); err != nil { t.Fatal(err) }
	conflicting := map[string]any{"input_tokens": int64(11), "cached_input_tokens": int64(0), "output_tokens": int64(2), "reasoning_output_tokens": int64(0), "total_tokens": int64(13)}
	if err := os.WriteFile(pathB, []byte(strings.Join(append(append([]string(nil), shared...), response(conflicting)), "\n")+"\n"), 0o600); err != nil { t.Fatal(err) }
	appendRolloutLine(t, pathA, `{"type":"response_item","timestamp":"2026-01-01T00:00:05Z","payload":{"type":"function_call","name":"exec_command","arguments":{"cmd":"cat /workspace/skills/alpha/SKILL.md"}}}`)

	if err := env.runScan(context.Background()); err != nil { t.Fatalf("FatalIsolation scan: %v", err) }
	state := env.usageEpoch()
	if state.ActiveEpoch != 1 || state.BuildEpoch != nil { t.Fatalf("FatalIsolation did not activate complete Build: %+v", state) }
	projection := finalScanProjection(t, env)
	if len(projection.Quarantine) != 1 || projection.Quarantine[0].RootSessionID != scanMainID || projection.Quarantine[0].PrimaryErrorCode != string(codexusage.FatalResponseUsage) {
		t.Fatalf("fatal root projection=%+v", projection.Quarantine)
	}
	assertQuarantinedNoLeak(t, env, state.ActiveEpoch, scanMainID)
	if len(projection.Skills) != 0 || len(projection.UsageEvents) != 0 {
		t.Fatalf("fatal root leaked skill or usage rows: skills=%+v usage=%+v", projection.Skills, projection.UsageEvents)
	}
}

func assertQuarantinedNoLeak(t *testing.T, env *scanTestEnv, epoch int64, root string) {
	t.Helper()
	for _, table := range []string{"usage_events", "codex_usage_event_occurrences", "codex_usage_event_facts", "codex_compaction_markers", "codex_usage_reconciliation_windows", "codex_usage_event_holds", "codex_turns", "codex_usage_source_states", "codex_skill_usage_events"} {
		query := "SELECT COUNT(*) FROM " + table + " WHERE 1=1"
		args := []any{}
		switch table {
		case "usage_events": query += " AND source='codex' AND source_epoch=? AND root_session_id=?"; args = []any{epoch, root}
		case "codex_usage_event_facts": query += " AND source='codex' AND ledger_epoch=? AND owning_thread_id IN (SELECT thread_id FROM threads WHERE root_session_id=? OR thread_id=?)"; args = []any{epoch, root, root}
		case "codex_usage_event_occurrences", "codex_compaction_markers", "codex_usage_reconciliation_windows", "codex_usage_event_holds", "codex_usage_source_states", "codex_skill_usage_events": query += " AND ledger_epoch=? AND source_file_id IN (SELECT source_file_id FROM codex_source_files WHERE thread_id IN (SELECT thread_id FROM threads WHERE root_session_id=? OR thread_id=?))"; args = []any{epoch, root, root}
		case "codex_turns": query += " AND ledger_epoch=? AND thread_id IN (SELECT thread_id FROM threads WHERE root_session_id=? OR thread_id=?)"; args = []any{epoch, root, root}
		}
		if got := env.scalar(query, args...); got != 0 { t.Errorf("quarantined root %s has %d rows in %s", root, got, table) }
	}
}

func TestRunScanUnchangedQuarantineIsHeldWithoutSemanticMutation(t *testing.T) {
	env := newScanTestEnv(t)
	pathA := env.rolloutPath(scanMainID, 1)
	pathB := env.rolloutPath(scanMainID, 2)
	env.addThread(scanMainID, nil, pathA, "main")
	good := `{"input_tokens":10,"cached_input_tokens":0,"output_tokens":2,"reasoning_output_tokens":0,"total_tokens":12}`
	bad := `{"input_tokens":11,"cached_input_tokens":0,"output_tokens":2,"reasoning_output_tokens":0,"total_tokens":13}`
	makeResponse := func(usageJSON string) string {
		return `{"timestamp":"2026-01-01T00:00:04Z","type":"token_usage_record","payload":{"response_id":"` + scanResponseID1 + `","thread_id":"` + scanMainID + `","session_id":"` + scanMainID + `","turn_id":"` + scanTurnID1 + `","usage":` + usageJSON + `}}`
	}
	base := []string{scanSessionMeta(scanMainID, "main", nil, env.home), scanTurnContext(scanTurnID1, scanProjectModel, "high", "2026-01-01T00:00:02Z")}
	if err := os.WriteFile(pathA, []byte(strings.Join(append(append([]string(nil), base...), makeResponse(good)), "\n")+"\n"), 0o600); err != nil { t.Fatal(err) }
	if err := os.WriteFile(pathB, []byte(strings.Join(append(append([]string(nil), base...), makeResponse(bad)), "\n")+"\n"), 0o600); err != nil { t.Fatal(err) }
	if err := env.runScan(context.Background()); err != nil { t.Fatal(err) }
	before := env.db.CurrentRevision()
	beforeProjection := normalizedJSON(t, finalScanProjection(t, env))
	beforeCounts := captureSemanticCounts(t, env)
	beforeBuild := env.usageEpoch().BuildEpoch
	if err := env.runScan(context.Background()); err != nil { t.Fatalf("unchanged quarantine hold scan: %v", err) }
	if env.db.CurrentRevision() != before { t.Fatalf("unchanged quarantine changed revision: before=%+v after=%+v", before, env.db.CurrentRevision()) }
	if !bytes.Equal(beforeProjection, normalizedJSON(t, finalScanProjection(t, env))) { t.Fatal("unchanged quarantine changed final projection") }
	if !equalIntMap(beforeCounts, captureSemanticCounts(t, env)) { t.Fatal("unchanged quarantine changed semantic tables") }
	if !equalNullableInt(env.usageEpoch().BuildEpoch, beforeBuild) { t.Fatal("unchanged quarantine created or removed a Build") }
}

func captureSemanticCounts(t *testing.T, env *scanTestEnv) map[string]int64 {
	t.Helper()
	epoch := env.usageEpoch().ActiveEpoch
	counts := make(map[string]int64)
	for _, table := range []string{"usage_events", "codex_usage_event_occurrences", "codex_usage_event_facts", "codex_compaction_markers", "codex_usage_reconciliation_windows", "codex_usage_event_holds", "codex_turns", "codex_usage_source_states", "codex_skill_usage_events", "codex_usage_session_quarantine", "codex_usage_session_quarantine_sources"} {
		query := "SELECT COUNT(*) FROM " + table
		if table == "usage_events" { query += " WHERE source='codex' AND source_epoch=" + fmt.Sprint(epoch) } else { query += " WHERE ledger_epoch=" + fmt.Sprint(epoch) }
		counts[table] = env.scalar(query)
	}
	return counts
}

func equalIntMap(left, right map[string]int64) bool {
	if len(left) != len(right) { return false }
	for key, value := range left { if right[key] != value { return false } }
	return true
}

func equalNullableInt(left, right *int64) bool {
	if left == nil || right == nil { return left == nil && right == nil }
	return *left == *right
}

func TestRunScanPathAIncrementalEqualsFreshFullHome(t *testing.T) {
	finalHome := filepath.Join(t.TempDir(), "final-codex-home")
	if err := os.MkdirAll(filepath.Join(finalHome, "sessions"), 0o755); err != nil { t.Fatal(err) }
	if err := os.MkdirAll(filepath.Join(finalHome, "archived_sessions"), 0o755); err != nil { t.Fatal(err) }
	createScanStateDB(t, filepath.Join(finalHome, stateIndexFilename))
	if err := os.WriteFile(filepath.Join(finalHome, sessionIndexFilename), nil, 0o600); err != nil { t.Fatal(err) }
	writeScanGlobalState(t, finalHome, nil, nil)
	finalConfig, err := ResolveConfigFromHome(finalHome)
	if err != nil { t.Fatal(err) }
	path := filepath.Join(finalHome, "sessions", "rollout-"+scanMainID+".jsonl")
	firstRecords := []string{scanSessionMeta(scanMainID, "main", nil, finalHome), scanTurnContext(scanTurnID1, scanProjectModel, "high", "2026-01-01T00:00:02Z"), scanResponse(scanResponseID1, scanMainID, scanMainID, scanTurnID1, 10, 20)}
	if err := os.WriteFile(path, []byte(strings.Join(firstRecords, "\n")+"\n"), 0o600); err != nil { t.Fatal(err) }
	stateDB, err := sql.Open("sqlite", finalConfig.Metadata.StateIndex)
	if err != nil { t.Fatal(err) }
	if _, err := stateDB.Exec(`INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role,agent_path)
		VALUES(?,?,1767225600000,1767225601000,0,?,'Path A','','gpt-6-luna','main','main')`, scanMainID, path, finalHome); err != nil { _ = stateDB.Close(); t.Fatal(err) }
	if err := stateDB.Close(); err != nil { t.Fatal(err) }
	indexLine, _ := json.Marshal(map[string]any{"id": scanMainID, "thread_name": "Path A", "updated_at": "2026-01-01T00:00:01Z"})
	if err := os.WriteFile(finalConfig.Metadata.SessionIndex, append(indexLine, '\n'), 0o600); err != nil { t.Fatal(err) }

	pathA := newScanTestEnvWithHome(t, finalConfig)
	if err := pathA.runScan(context.Background()); err != nil { t.Fatalf("Path A initial scan: %v", err) }
	appendRolloutLine(t, path, scanResponse(scanResponseID2, scanMainID, scanMainID, scanTurnID2, 12, 22))
	if err := pathA.runScan(context.Background()); err != nil { t.Fatalf("Path A append scan: %v", err) }
	// Process restart: close and reopen the same v14 database and a fresh Adapter.
	appPath := pathA.db.Path()
	if err := pathA.db.Close(); err != nil { t.Fatal(err) }
	pathA = reopenScanTestEnv(t, finalConfig, appPath)
	appendRolloutLine(t, path, scanResponse(scanResponseID3, scanMainID, scanMainID, scanTurnID3, 14, 24))
	if err := pathA.runScan(context.Background()); err != nil { t.Fatalf("Path A post-restart append scan: %v", err) }
	plain, err := os.ReadFile(path)
	if err != nil { t.Fatal(err) }
	if err := os.Remove(path); err != nil { t.Fatal(err) }
	zstdPath := path + ".zst"
	if err := os.WriteFile(zstdPath, zstdBytes(t, plain), 0o600); err != nil { t.Fatal(err) }
	if err := pathA.runScan(context.Background()); err != nil { t.Fatalf("Path A zstd transition scan: %v", err) }
	pathAProjection := finalScanProjection(t, pathA)
	if err := pathA.db.Close(); err != nil { t.Fatal(err) }

	pathB := newScanTestEnvWithHome(t, finalConfig)
	if err := pathB.runScan(context.Background()); err != nil { t.Fatalf("Path B clean full scan: %v", err) }
	pathBProjection := finalScanProjection(t, pathB)
	assertNormalizedProjectionEqual(t, normalizedJSON(t, pathBProjection), pathAProjection)
}

func newScanTestEnvWithHome(t *testing.T, config Config) *scanTestEnv {
	t.Helper()
	env := newScanTestEnv(t)
	env.home, env.config = config.Home, config
	env.resolver.set(ConfigResolution{Config: config})
	return env
}

func reopenScanTestEnv(t *testing.T, config Config, appPath string) *scanTestEnv {
	t.Helper()
	db, err := storage.Open(context.Background(), storage.Config{Path: appPath})
	if err != nil { t.Fatal(err) }
	resolver := &scanTestResolver{}
	resolver.set(ConfigResolution{Config: config})
	adapter, err := NewAdapterWithResolver(resolver)
	if err != nil { _ = db.Close(); t.Fatal(err) }
	env := &scanTestEnv{t: t, home: config.Home, config: config, db: db, adapter: adapter, resolver: resolver}
	base := time.Date(2026, 1, 2, 0, 0, 0, 0, time.UTC).UnixMilli()
	env.clockValue.Store(base)
	adapter.clock = func() int64 { return env.clockValue.Add(1) }
	descriptor, err := source.NewDescriptor(domain.SourceCodex, "Codex")
	if err != nil { t.Fatal(err) }
	run, err := source.NewStorageFactory(db).Context(context.Background(), "phase4-restarted", descriptor)
	if err != nil { t.Fatal(err) }
	env.bound = run.Storage()
	t.Cleanup(func() { if err := db.Close(); err != nil { t.Error(err) } })
	return env
}

func TestScanTestUUIDFixtureValidation(t *testing.T) {
	for _, id := range []string{scanMainID, scanSubagentID, scanOtherID, scanResponseID1, scanResponseID2, scanResponseID3, scanTurnID1, scanTurnID2, scanTurnID3} {
		parsed, err := uuid.Parse(id)
		if err != nil || parsed.Version() != 7 { t.Fatalf("invalid fixture UUIDv7 %q: %v", id, err) }
	}
}

func TestRunScanFixedConfigResolverDoesNotReadEnvironmentMidScan(t *testing.T) {
	env := newScanTestEnv(t)
	env.addMain(scanMainID, scanResponseID1)
	otherHome := filepath.Join(t.TempDir(), "other-home")
	createMinimalScanHome(t, otherHome)
	otherConfig, err := ResolveConfigFromHome(otherHome)
	if err != nil { t.Fatal(err) }
	env.resolver.set(ConfigResolution{Config: env.config}, ConfigResolution{Config: otherConfig})
	t.Setenv("CODEX_HOME", otherHome)
	if err := env.runScan(context.Background()); err != nil { t.Fatalf("frozen config scan failed: %v", err) }
	if env.resolver.callCount() != 1 { t.Fatalf("RunScan resolved config %d times", env.resolver.callCount()) }
	if env.usageEpoch().ActiveEpoch != 1 || env.scalar("SELECT COUNT(*) FROM codex_source_files") != 1 {
		t.Fatal("RunScan did not keep using the one frozen Config result")
	}
}

var _ = gzipBytes
var _ = exactPaddedJSONLine
var _ = seedActiveEpoch
var _ = seedScanCatalogRow
var _ = seedScanCheckpoint
var _ = scanThreadIdentity
var _ = seedScanThread
var _ = scanCanonicalEvent
var _ = seedScanUsageEvent
var _ = seedActiveSourceState
