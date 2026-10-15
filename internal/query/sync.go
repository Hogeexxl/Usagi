package query

import (
	"cmp"
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/pricing"
)

const (
	syncPageLimit  = 500
	syncDigestDays = 60
	syncWorkers    = 4
	syncDayTimeout = 2 * time.Minute
)

var errUnstableDay = errors.New("query: usage day changed during pagination")

type syncDayState struct {
	fingerprint               []byte
	lastSyncNS                sql.NullInt64
	frozen                    bool
	identityBackfillPending   bool
	compactionBackfillPending bool
}

type dayDigest struct {
	digest [32]byte
	empty  bool
}

type syncDayResult struct {
	day    string
	digest [32]byte
	rows   []magpie.UsageRequest
	err    error
}

// Sync refreshes session metadata and the local-day request ledger. It always
// returns the database revision, including when one of the independent writes
// committed before a later operation failed.
func (s *Service) Sync(ctx context.Context) (int64, error) {
	s.syncMu.Lock()
	defer s.syncMu.Unlock()

	var syncErr error
	identityAvailable, err := s.syncSessions(ctx)
	if err != nil {
		syncErr = errors.Join(syncErr, err)
	}
	if err := s.syncUsage(ctx, identityAvailable); err != nil {
		syncErr = errors.Join(syncErr, err)
	}
	if err := s.syncPricing(ctx); err != nil {
		slog.Warn("query: bootstrap price catalog", "error", err)
	}
	revision, err := s.dataRevision(context.Background())
	if err != nil {
		return revision, errors.Join(syncErr, err)
	}
	return revision, syncErr
}

func (s *Service) syncPricing(ctx context.Context) error {
	var exists bool
	if err := s.db.QueryRowContext(ctx, "SELECT EXISTS(SELECT 1 FROM price_versions)").Scan(&exists); err != nil {
		return fmt.Errorf("query: check price versions: %w", err)
	}
	if exists {
		return nil
	}
	response, err := s.upstream.Providers(ctx)
	if err != nil {
		return fmt.Errorf("query: read provider catalogs: %w", err)
	}
	if response.FileError != "" {
		return fmt.Errorf("query: providers file error: %s", response.FileError)
	}
	if _, err := pricing.Bootstrap(ctx, s.db, s.pricingHTTPClient, providerCatalogsOf(response.Providers), s.now()); err != nil {
		return fmt.Errorf("query: bootstrap price catalog: %w", err)
	}
	return nil
}

func providerCatalogsOf(providers []magpie.ProviderCatalog) map[string][]string {
	result := make(map[string][]string, len(providers))
	for _, provider := range providers {
		catalogs, exists := result[provider.ID]
		if !exists {
			catalogs = []string{}
			result[provider.ID] = catalogs
		}
		seen := make(map[string]bool, len(catalogs))
		for _, catalog := range catalogs {
			seen[catalog] = true
		}
		for _, catalog := range strings.FieldsFunc(strings.ToLower(provider.Catalog), func(r rune) bool {
			return r == ',' || r == ' '
		}) {
			catalog = strings.TrimSpace(catalog)
			if catalog != "" && !seen[catalog] {
				result[provider.ID] = append(result[provider.ID], catalog)
				seen[catalog] = true
			}
		}
	}
	return result
}

// InitialSyncComplete reports whether the first full local-day pass committed.
func (s *Service) InitialSyncComplete(ctx context.Context) (bool, error) {
	var value string
	if err := s.db.QueryRowContext(ctx, "SELECT value FROM app_meta WHERE key = 'initial_sync_complete'").Scan(&value); err != nil {
		return false, fmt.Errorf("query: read initial sync state: %w", err)
	}
	return value == "1", nil
}

func (s *Service) syncSessions(ctx context.Context) (bool, error) {
	response, err := s.upstream.Sessions(ctx, 0)
	if err != nil {
		return false, fmt.Errorf("query: sync sessions: %w", err)
	}
	now := s.now().UnixNano()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return false, fmt.Errorf("query: begin session sync: %w", err)
	}
	defer tx.Rollback()

	existing := make(map[sessionKey]syncSessionState)
	rows, err := tx.QueryContext(ctx, `SELECT agent, id, cwd, title, nickname, start_ns, last_ns,
		gateway, read_only, has_path, agent_name, disappeared, role, parent_session, root_session FROM sessions`)
	if err != nil {
		return false, fmt.Errorf("query: read sessions before sync: %w", err)
	}
	for rows.Next() {
		var key sessionKey
		var state syncSessionState
		if err := rows.Scan(&key.Agent, &key.ID, &state.cwd, &state.title, &state.nickname, &state.startNS,
			&state.lastNS, &state.gateway, &state.readOnly, &state.hasPath,
			&state.agentName, &state.disappeared, &state.role, &state.parentSession, &state.rootSession); err != nil {
			rows.Close()
			return false, fmt.Errorf("query: scan existing session: %w", err)
		}
		existing[key] = state
	}
	if err := rows.Close(); err != nil {
		return false, fmt.Errorf("query: close existing sessions: %w", err)
	}
	if err := rows.Err(); err != nil {
		return false, fmt.Errorf("query: iterate existing sessions: %w", err)
	}

	seen := make(map[sessionKey]struct{}, len(response.Sessions))
	changedIdentityIDs := make(map[string]struct{})
	changed := false
	identityAvailable := false
	for _, session := range response.Sessions {
		key := sessionKey{Agent: session.Agent, ID: session.ID}
		seen[key] = struct{}{}
		state := syncSessionStateFor(session)
		if key.Agent == "codex" {
			identityAvailable = identityAvailable || state.identityAvailable
		}
		previous, ok := existing[key]
		if !ok || previous.disappeared != 0 || !state.equal(previous) {
			changed = true
		}
		if key.Agent == "codex" && state.identityAvailable && (!ok || !state.identityEqual(previous)) {
			for _, id := range []string{key.ID, previous.parentSession, previous.rootSession, state.parentSession, state.rootSession} {
				if id != "" {
					changedIdentityIDs[id] = struct{}{}
				}
			}
		}
	}
	for key, state := range existing {
		if _, ok := seen[key]; !ok && state.disappeared == 0 {
			changed = true
		}
	}

	if _, err := tx.ExecContext(ctx, "UPDATE sessions SET disappeared = 1 WHERE disappeared = 0"); err != nil {
		return false, fmt.Errorf("query: mark disappeared sessions: %w", err)
	}
	statement, err := tx.PrepareContext(ctx, `INSERT INTO sessions (
		agent, id, cwd, title, nickname, start_ns, last_ns, gateway, read_only, has_path,
		agent_name, first_seen_ns, last_seen_ns, disappeared, role, parent_session, root_session
	) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?)
	ON CONFLICT(agent, id) DO UPDATE SET
		cwd = excluded.cwd, title = excluded.title, nickname = excluded.nickname, start_ns = excluded.start_ns,
		last_ns = excluded.last_ns, gateway = excluded.gateway,
		read_only = excluded.read_only, has_path = excluded.has_path,
		agent_name = excluded.agent_name, last_seen_ns = excluded.last_seen_ns,
		disappeared = 0,
		role = CASE WHEN ? THEN excluded.role ELSE sessions.role END,
		parent_session = CASE WHEN ? THEN excluded.parent_session ELSE sessions.parent_session END,
		root_session = CASE WHEN ? THEN excluded.root_session ELSE sessions.root_session END`)
	if err != nil {
		return false, fmt.Errorf("query: prepare session upsert: %w", err)
	}
	defer statement.Close()
	for _, session := range response.Sessions {
		state := syncSessionStateFor(session)
		if _, err := statement.ExecContext(ctx, session.Agent, session.ID, session.Cwd, session.Title,
			session.Nickname, nullableTimeNS(session.Start), nullableTimeNS(session.Last), state.gateway,
			state.readOnly, state.hasPath, state.agentName, now, now,
			state.role, state.parentSession, state.rootSession,
			boolInt(state.identityAvailable), boolInt(state.identityAvailable), boolInt(state.identityAvailable)); err != nil {
			return false, fmt.Errorf("query: upsert session %s:%s: %w", session.Agent, session.ID, err)
		}
	}
	if len(changedIdentityIDs) != 0 {
		var started string
		if err := tx.QueryRowContext(ctx, "SELECT value FROM app_meta WHERE key = 'session_identity_backfill_started'").Scan(&started); err != nil {
			return false, fmt.Errorf("query: read session identity backfill state: %w", err)
		}
		if started == "1" {
			if err := markChangedSessionDays(ctx, tx, changedIdentityIDs); err != nil {
				return false, err
			}
		}
	}
	if changed {
		if err := incrementDataRevision(ctx, tx); err != nil {
			return false, err
		}
	}
	if err := tx.Commit(); err != nil {
		return false, fmt.Errorf("query: commit session sync: %w", err)
	}
	return identityAvailable, nil
}

type syncSessionState struct {
	cwd, title, nickname, agentName  string
	role, parentSession, rootSession string
	startNS, lastNS                  sql.NullInt64
	gateway, readOnly                int
	hasPath, disappeared             int
	identityAvailable                bool
}

func syncSessionStateFor(session magpie.Session) syncSessionState {
	return syncSessionState{
		cwd: session.Cwd, title: session.Title, startNS: nullableTimeNSValue(session.Start),
		lastNS: nullableTimeNSValue(session.Last), gateway: boolInt(session.Gateway),
		readOnly: boolInt(session.ReadOnly), hasPath: boolInt(session.Path != ""),
		agentName: session.Name, nickname: session.Nickname,
		role: session.Role, parentSession: session.ParentSession, rootSession: session.RootSession,
		identityAvailable: session.Role != "" || session.ParentSession != "" || session.RootSession != "",
	}
}

func (s syncSessionState) equal(other syncSessionState) bool {
	return s.cwd == other.cwd && s.title == other.title && s.nickname == other.nickname && nullIntEqual(s.startNS, other.startNS) &&
		nullIntEqual(s.lastNS, other.lastNS) && s.gateway == other.gateway &&
		s.readOnly == other.readOnly && s.hasPath == other.hasPath && s.agentName == other.agentName &&
		(!s.identityAvailable || s.identityEqual(other))
}

func (s syncSessionState) identityEqual(other syncSessionState) bool {
	return s.role == other.role && s.parentSession == other.parentSession && s.rootSession == other.rootSession
}

func markChangedSessionDays(ctx context.Context, tx *sql.Tx, sessionIDs map[string]struct{}) error {
	days := make(map[string]struct{})
	for id := range sessionIDs {
		rows, err := tx.QueryContext(ctx, `SELECT DISTINCT local_day FROM requests
			WHERE agent = 'codex' AND (session = ? OR native_session = ? OR subagent = ? OR parent_agent = ?)`, id, id, id, id)
		if err != nil {
			return fmt.Errorf("query: find days for changed session %s: %w", id, err)
		}
		for rows.Next() {
			var day string
			if err := rows.Scan(&day); err != nil {
				rows.Close()
				return fmt.Errorf("query: read day for changed session %s: %w", id, err)
			}
			days[day] = struct{}{}
		}
		if err := rows.Close(); err != nil {
			return fmt.Errorf("query: close days for changed session %s: %w", id, err)
		}
		if err := rows.Err(); err != nil {
			return fmt.Errorf("query: iterate days for changed session %s: %w", id, err)
		}
	}
	if len(days) == 0 {
		return nil
	}
	for day := range days {
		if _, err := tx.ExecContext(ctx, `INSERT INTO sync_days (day, identity_backfill_pending)
			VALUES (?, 1) ON CONFLICT(day) DO UPDATE SET identity_backfill_pending = 1`, day); err != nil {
			return fmt.Errorf("query: queue changed session day %s: %w", day, err)
		}
	}
	if _, err := tx.ExecContext(ctx, "UPDATE app_meta SET value = '0' WHERE key = 'session_identity_backfill_complete'"); err != nil {
		return fmt.Errorf("query: clear session identity backfill completion: %w", err)
	}
	return nil
}

func nullableTimeNS(value time.Time) any {
	if value.IsZero() {
		return nil
	}
	return value.UnixNano()
}

func nullableTimeNSValue(value time.Time) sql.NullInt64 {
	if value.IsZero() {
		return sql.NullInt64{}
	}
	return sql.NullInt64{Int64: value.UnixNano(), Valid: true}
}

func nullIntEqual(a, b sql.NullInt64) bool {
	return a.Valid == b.Valid && (!a.Valid || a.Int64 == b.Int64)
}

func boolInt(value bool) int {
	if value {
		return 1
	}
	return 0
}

func nullableBoolInt(value *bool) any {
	if value == nil {
		return nil
	}
	return boolInt(*value)
}

func (s *Service) syncUsage(ctx context.Context, identityAvailable bool) error {
	summary, err := s.upstream.Usage(ctx, "all")
	if err != nil {
		return fmt.Errorf("query: discover usage days: %w", err)
	}
	fingerprint, err := UsageFingerprint(summary)
	if err != nil {
		return err
	}
	today := s.now().In(s.location).Format(time.DateOnly)
	yesterday := s.now().In(s.location).AddDate(0, 0, -1).Format(time.DateOnly)
	summaryChanged := s.hasUsageFingerprint && s.usageFingerprint != fingerprint
	if summaryChanged {
		s.pendingRecent[today] = struct{}{}
		s.pendingRecent[yesterday] = struct{}{}
	}
	s.usageFingerprint = fingerprint
	s.hasUsageFingerprint = true
	from := today
	for _, point := range summary.Series {
		if point.Time.IsZero() {
			continue
		}
		day := point.Time.In(s.location).Format(time.DateOnly)
		if day < from {
			from = day
		}
	}
	for day := range s.pendingRecent {
		if day < from || day > today {
			delete(s.pendingRecent, day)
		}
	}
	days, err := daysBetween(from, today)
	if err != nil {
		return fmt.Errorf("query: enumerate usage days: %w", err)
	}
	if identityAvailable {
		if err := s.startSessionIdentityBackfill(ctx, days); err != nil {
			return err
		}
		days, err = s.includePendingIdentityDays(ctx, days)
		if err != nil {
			return err
		}
	}
	days, err = s.includePendingCompactionDays(ctx, days)
	if err != nil {
		return err
	}
	digests, verified, err := s.syncDigests(ctx, days)
	if err != nil {
		return err
	}
	states, err := s.syncDayStates(ctx, days)
	if err != nil {
		return err
	}
	cutoff := s.now().In(s.location).AddDate(0, 0, -7).Format(time.DateOnly)
	var targets []string
	for _, day := range days {
		state := states[day]
		digest := digests[day].digest
		if state.compactionBackfillPending {
			targets = append(targets, day)
			continue
		}
		if identityAvailable && state.identityBackfillPending {
			targets = append(targets, day)
			continue
		}
		if state.frozen {
			delete(s.pendingRecent, day)
			if verified && len(state.fingerprint) == len(digest) && !slices.Equal(state.fingerprint, digest[:]) {
				if err := s.recordFrozenDifference(ctx, day, state.fingerprint, digest); err != nil {
					return err
				}
			}
			continue
		}
		dayEnd, err := localDayEnd(day, s.location)
		if err != nil {
			return err
		}
		if day < cutoff && state.lastSyncNS.Valid && state.lastSyncNS.Int64 > dayEnd.UnixNano() {
			if err := s.freezeDay(ctx, day); err != nil {
				return err
			}
			if verified && len(state.fingerprint) == len(digest) && !slices.Equal(state.fingerprint, digest[:]) {
				if err := s.recordFrozenDifference(ctx, day, state.fingerprint, digest); err != nil {
					return err
				}
			}
			continue
		}
		stale := !state.lastSyncNS.Valid || len(state.fingerprint) != len(digest) || !verified || !slices.Equal(state.fingerprint, digest[:])
		_, pendingRecent := s.pendingRecent[day]
		recent := pendingRecent || (summaryChanged && (day == today || day == yesterday))
		mustFreeze := day < cutoff && (!state.lastSyncNS.Valid || state.lastSyncNS.Int64 <= dayEnd.UnixNano())
		if stale || recent || mustFreeze {
			targets = append(targets, day)
		}
	}

	if err := s.fetchAndSaveDays(ctx, targets, digests, identityAvailable); err != nil {
		return err
	}
	if identityAvailable {
		if err := s.finishSessionIdentityBackfill(ctx); err != nil {
			return err
		}
	}
	if err := s.markInitialSyncComplete(ctx, from, today); err != nil {
		return err
	}
	return nil
}

func (s *Service) startSessionIdentityBackfill(ctx context.Context, days []string) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("query: begin session identity backfill: %w", err)
	}
	defer tx.Rollback()
	var started string
	if err := tx.QueryRowContext(ctx, "SELECT value FROM app_meta WHERE key = 'session_identity_backfill_started'").Scan(&started); err != nil {
		return fmt.Errorf("query: read session identity backfill start: %w", err)
	}
	if started == "1" {
		return nil
	}
	for _, day := range days {
		if _, err := tx.ExecContext(ctx, `INSERT INTO sync_days (day, identity_backfill_pending)
			VALUES (?, 1) ON CONFLICT(day) DO UPDATE SET identity_backfill_pending = 1`, day); err != nil {
			return fmt.Errorf("query: queue session identity day %s: %w", day, err)
		}
	}
	if _, err := tx.ExecContext(ctx, "UPDATE app_meta SET value = '1' WHERE key = 'session_identity_backfill_started'"); err != nil {
		return fmt.Errorf("query: mark session identity backfill started: %w", err)
	}
	if _, err := tx.ExecContext(ctx, "UPDATE app_meta SET value = '0' WHERE key = 'session_identity_backfill_complete'"); err != nil {
		return fmt.Errorf("query: clear session identity backfill completion: %w", err)
	}
	if err := tx.Commit(); err != nil {
		return fmt.Errorf("query: commit session identity backfill start: %w", err)
	}
	return nil
}

func (s *Service) includePendingIdentityDays(ctx context.Context, days []string) ([]string, error) {
	rows, err := s.db.QueryContext(ctx, "SELECT day FROM sync_days WHERE identity_backfill_pending = 1")
	if err != nil {
		return nil, fmt.Errorf("query: read pending identity days: %w", err)
	}
	defer rows.Close()
	values := make(map[string]struct{}, len(days))
	for _, day := range days {
		values[day] = struct{}{}
	}
	for rows.Next() {
		var day string
		if err := rows.Scan(&day); err != nil {
			return nil, fmt.Errorf("query: scan pending identity day: %w", err)
		}
		values[day] = struct{}{}
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("query: iterate pending identity days: %w", err)
	}
	result := make([]string, 0, len(values))
	for day := range values {
		result = append(result, day)
	}
	slices.Sort(result)
	return result, nil
}

func (s *Service) includePendingCompactionDays(ctx context.Context, days []string) ([]string, error) {
	rows, err := s.db.QueryContext(ctx, "SELECT day FROM sync_days WHERE compaction_backfill_pending = 1")
	if err != nil {
		return nil, fmt.Errorf("query: read pending compaction days: %w", err)
	}
	defer rows.Close()
	values := make(map[string]struct{}, len(days))
	for _, day := range days {
		values[day] = struct{}{}
	}
	for rows.Next() {
		var day string
		if err := rows.Scan(&day); err != nil {
			return nil, fmt.Errorf("query: scan pending compaction day: %w", err)
		}
		values[day] = struct{}{}
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("query: iterate pending compaction days: %w", err)
	}
	result := make([]string, 0, len(values))
	for day := range values {
		result = append(result, day)
	}
	slices.Sort(result)
	return result, nil
}

func (s *Service) finishSessionIdentityBackfill(ctx context.Context) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("query: begin session identity backfill completion: %w", err)
	}
	defer tx.Rollback()
	var pending int
	if err := tx.QueryRowContext(ctx, "SELECT COUNT(*) FROM sync_days WHERE identity_backfill_pending = 1").Scan(&pending); err != nil {
		return fmt.Errorf("query: count pending session identity days: %w", err)
	}
	if pending == 0 {
		if _, err := tx.ExecContext(ctx, "UPDATE app_meta SET value = '1' WHERE key = 'session_identity_backfill_complete'"); err != nil {
			return fmt.Errorf("query: mark session identity backfill complete: %w", err)
		}
	}
	if err := tx.Commit(); err != nil {
		return fmt.Errorf("query: commit session identity backfill completion: %w", err)
	}
	return nil
}

func (s *Service) syncDigests(ctx context.Context, days []string) (map[string]dayDigest, bool, error) {
	result := make(map[string]dayDigest, len(days))
	for start := 0; start < len(days); {
		first, err := time.Parse(time.DateOnly, days[start])
		if err != nil {
			return nil, false, ErrInvalidRange
		}
		limit := first.AddDate(0, 0, syncDigestDays-1).Format(time.DateOnly)
		end := start
		for end+1 < len(days) && days[end+1] <= limit {
			end++
		}
		values, err := s.upstream.UsageDayDigests(ctx, days[start], days[end])
		if errors.Is(err, magpie.ErrDayDigestsUnavailable) {
			return result, false, nil
		}
		if err != nil {
			return nil, false, fmt.Errorf("query: usage digests %s..%s: %w", days[start], days[end], err)
		}
		for _, value := range values {
			day := value.Day
			if !value.Time.IsZero() {
				day = value.Time.In(s.location).Format(time.DateOnly)
			}
			result[day] = dayDigest{digest: value.Digest, empty: value.Calls == 0}
		}
		start = end + 1
	}
	return result, true, nil
}

func daysBetween(from, to string) ([]string, error) {
	start, err := time.Parse(time.DateOnly, from)
	if err != nil {
		return nil, ErrInvalidRange
	}
	end, err := time.Parse(time.DateOnly, to)
	if err != nil || start.After(end) {
		return nil, ErrInvalidRange
	}
	var days []string
	for day := start; !day.After(end); day = day.AddDate(0, 0, 1) {
		days = append(days, day.Format(time.DateOnly))
	}
	return days, nil
}

func (s *Service) syncDayStates(ctx context.Context, days []string) (map[string]syncDayState, error) {
	states := make(map[string]syncDayState, len(days))
	if len(days) == 0 {
		return states, nil
	}
	rows, err := s.db.QueryContext(ctx, `SELECT day, fingerprint, last_sync_ns, frozen, identity_backfill_pending, compaction_backfill_pending
		FROM sync_days WHERE day >= ? AND day <= ?`, days[0], days[len(days)-1])
	if err != nil {
		return nil, fmt.Errorf("query: read sync days: %w", err)
	}
	defer rows.Close()
	for rows.Next() {
		var day string
		var fingerprint []byte
		var state syncDayState
		var identityPending, compactionPending int
		if err := rows.Scan(&day, &fingerprint, &state.lastSyncNS, &state.frozen, &identityPending, &compactionPending); err != nil {
			return nil, fmt.Errorf("query: scan sync day: %w", err)
		}
		state.fingerprint = slices.Clone(fingerprint)
		state.identityBackfillPending = identityPending != 0
		state.compactionBackfillPending = compactionPending != 0
		states[day] = state
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("query: iterate sync days: %w", err)
	}
	return states, nil
}

func (s *Service) fetchAndSaveDays(ctx context.Context, days []string, digests map[string]dayDigest, identityAvailable bool) error {
	if len(days) == 0 {
		return nil
	}
	tasks := make(chan string)
	results := make(chan syncDayResult, syncWorkers)
	var workers sync.WaitGroup
	for range min(syncWorkers, len(days)) {
		workers.Add(1)
		go func() {
			defer workers.Done()
			for day := range tasks {
				dayCtx, cancel := context.WithTimeout(ctx, syncDayTimeout)
				rows, err := s.fetchSyncDay(dayCtx, day)
				cancel()
				results <- syncDayResult{day: day, digest: digests[day].digest, rows: rows, err: err}
			}
		}()
	}
	go func() {
		defer close(tasks)
		for _, day := range days {
			select {
			case tasks <- day:
			case <-ctx.Done():
				return
			}
		}
	}()
	go func() {
		workers.Wait()
		close(results)
	}()
	var syncErr error
	for result := range results {
		if result.err != nil {
			syncErr = errors.Join(syncErr, fmt.Errorf("query: sync day %s: %w", result.day, result.err))
			continue
		}
		if err := s.saveSyncDay(ctx, result.day, result.digest, result.rows, identityAvailable); err != nil {
			syncErr = errors.Join(syncErr, fmt.Errorf("query: save day %s: %w", result.day, err))
		} else {
			delete(s.pendingRecent, result.day)
		}
	}
	return syncErr
}

func (s *Service) fetchSyncDay(ctx context.Context, day string) ([]magpie.UsageRequest, error) {
	query := magpie.UsageRequestsQuery{Period: day + ".." + day, Limit: syncPageLimit}
	var all []magpie.UsageRequest
	total := -1
	for {
		page, err := s.upstream.UsageRequests(ctx, query)
		if err != nil {
			return nil, fmt.Errorf("fetch page at offset %d: %w", query.Offset, err)
		}
		if total < 0 {
			total = page.Total
		} else if total != page.Total {
			return nil, errUnstableDay
		}
		all = append(all, page.Rows...)
		query.Offset += len(page.Rows)
		if query.Offset >= page.Total {
			break
		}
		if len(page.Rows) == 0 || len(all) > total {
			return nil, errUnstableDay
		}
	}
	if ctx.Err() != nil {
		return nil, ctx.Err()
	}
	if total < 0 || len(all) != total {
		return nil, errUnstableDay
	}
	check, err := s.upstream.UsageRequests(ctx, magpie.UsageRequestsQuery{
		Period: day + ".." + day, Offset: 0, Limit: 1,
	})
	if err != nil {
		return nil, fmt.Errorf("verify total: %w", err)
	}
	if check.Total != total {
		return nil, errUnstableDay
	}
	return all, nil
}

func (s *Service) saveSyncDay(ctx context.Context, day string, digest [32]byte, rows []magpie.UsageRequest, clearIdentityPending bool) error {
	now := s.now().UnixNano()
	dayEnd, err := localDayEnd(day, s.location)
	if err != nil {
		return err
	}
	frozen := day < s.now().In(s.location).AddDate(0, 0, -7).Format(time.DateOnly) && now > dayEnd.UnixNano()
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin day replacement: %w", err)
	}
	defer tx.Rollback()
	if _, err := tx.ExecContext(ctx, "DELETE FROM requests WHERE local_day = ?", day); err != nil {
		return fmt.Errorf("delete old requests: %w", err)
	}
	statement, err := tx.PrepareContext(ctx, `INSERT INTO requests (
		t_ns, local_day, agent, agent_name, provider, provider_name, model,
		requested, served, effort, kind, session, session_role, compaction, native_session, subagent,
		parent_agent, source, route_id, rid, response_id, status, rejected, error,
		tries, input, output, cache_read, cache_write, cache_write_1h, reasoning,
		ms, ttft_ms, first_text_ms, flow_ms, magpie_cost, magpie_priced
	) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) `)
	if err != nil {
		return fmt.Errorf("prepare request insert: %w", err)
	}
	for _, row := range rows {
		if _, err := statement.ExecContext(ctx,
			row.Time.UnixNano(), day, row.Agent, row.AgentName, row.Provider, row.ProviderName,
			row.Model, row.Requested, row.Served, row.Effort, row.Kind, row.Session,
			row.SessionRole, nullableBoolInt(row.Compaction), row.NativeSession, row.Subagent, row.ParentAgent, row.Source, row.RouteID,
			row.RID, row.ResponseID, row.Status, boolInt(row.Rejected), row.Error,
			row.Tries, row.Input, row.Output, row.CacheRead, row.CacheWrite,
			row.CacheWrite1h, row.Reasoning, row.Millis, row.TTFTMs, row.FirstTextMs,
			row.FlowMs, row.Cost, boolInt(row.Priced)); err != nil {
			_ = statement.Close()
			return fmt.Errorf("insert request row: %w", err)
		}
	}
	if err := statement.Close(); err != nil {
		return fmt.Errorf("close request insert: %w", err)
	}
	if _, err := tx.ExecContext(ctx, `INSERT INTO sync_days (day, fingerprint, row_count, last_sync_ns, frozen, difference)
		VALUES (?, ?, ?, ?, ?, '')
		ON CONFLICT(day) DO UPDATE SET fingerprint = excluded.fingerprint,
		row_count = excluded.row_count, last_sync_ns = excluded.last_sync_ns,
		frozen = excluded.frozen, difference = '',
		identity_backfill_pending = CASE WHEN ? THEN 0 ELSE sync_days.identity_backfill_pending END,
		compaction_backfill_pending = 0`,
		day, digest[:], len(rows), now, boolInt(frozen), boolInt(clearIdentityPending)); err != nil {
		return fmt.Errorf("save sync day state: %w", err)
	}
	if err := incrementDataRevision(ctx, tx); err != nil {
		return err
	}
	if err := tx.Commit(); err != nil {
		return fmt.Errorf("commit day replacement: %w", err)
	}
	return nil
}

func localDayEnd(day string, location *time.Location) (time.Time, error) {
	start, err := time.ParseInLocation(time.DateOnly, day, location)
	if err != nil {
		return time.Time{}, fmt.Errorf("query: parse local day %q: %w", day, err)
	}
	return start.AddDate(0, 0, 1), nil
}

func (s *Service) freezeDay(ctx context.Context, day string) error {
	if _, err := s.db.ExecContext(ctx, "UPDATE sync_days SET frozen = 1 WHERE day = ?", day); err != nil {
		return fmt.Errorf("query: freeze day %s: %w", day, err)
	}
	return nil
}

func (s *Service) recordFrozenDifference(ctx context.Context, day string, baseline []byte, current [32]byte) error {
	difference := hex.EncodeToString(baseline) + " -> " + hex.EncodeToString(current[:])
	if _, err := s.db.ExecContext(ctx, "UPDATE sync_days SET difference = ? WHERE day = ?", difference, day); err != nil {
		return fmt.Errorf("query: record frozen day difference %s: %w", day, err)
	}
	slog.Warn("frozen usage day differs from its committed baseline", "day", day,
		"baseline", hex.EncodeToString(baseline), "current", hex.EncodeToString(current[:]))
	return nil
}

func (s *Service) markInitialSyncComplete(ctx context.Context, from, today string) error {
	complete, err := s.InitialSyncComplete(ctx)
	if err != nil || complete {
		return err
	}
	days, err := daysBetween(from, today)
	if err != nil {
		return err
	}
	var missing int
	if err := s.db.QueryRowContext(ctx, `SELECT COUNT(*) FROM sync_days
		WHERE day >= ? AND day <= ? AND last_sync_ns IS NULL`, from, today).Scan(&missing); err != nil {
		return fmt.Errorf("query: check initial sync days: %w", err)
	}
	var found int
	if err := s.db.QueryRowContext(ctx, `SELECT COUNT(*) FROM sync_days
		WHERE day >= ? AND day <= ? AND last_sync_ns IS NOT NULL`, from, today).Scan(&found); err != nil {
		return fmt.Errorf("query: count initial sync days: %w", err)
	}
	if missing != 0 || found != len(days) {
		return nil
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("query: begin initial sync completion: %w", err)
	}
	defer tx.Rollback()
	result, err := tx.ExecContext(ctx, "UPDATE app_meta SET value = '1' WHERE key = 'initial_sync_complete' AND value <> '1'")
	if err != nil {
		return fmt.Errorf("query: mark initial sync complete: %w", err)
	}
	updated, err := result.RowsAffected()
	if err != nil {
		return fmt.Errorf("query: inspect initial sync completion: %w", err)
	}
	if updated != 0 {
		if err := incrementDataRevision(ctx, tx); err != nil {
			return err
		}
	}
	if err := tx.Commit(); err != nil {
		return fmt.Errorf("query: commit initial sync completion: %w", err)
	}
	return nil
}

func incrementDataRevision(ctx context.Context, tx *sql.Tx) error {
	if _, err := tx.ExecContext(ctx, "UPDATE app_meta SET value = CAST(value AS INTEGER) + 1 WHERE key = 'data_revision'"); err != nil {
		return fmt.Errorf("query: increment data revision: %w", err)
	}
	return nil
}

func (s *Service) dataRevision(ctx context.Context) (int64, error) {
	var value string
	if err := s.db.QueryRowContext(ctx, "SELECT value FROM app_meta WHERE key = 'data_revision'").Scan(&value); err != nil {
		return 0, fmt.Errorf("query: read data revision: %w", err)
	}
	revision, err := strconv.ParseInt(value, 10, 64)
	if err != nil {
		return 0, fmt.Errorf("query: parse data revision: %w", err)
	}
	return revision, nil
}

// UsageFingerprint is the canonical global summary fingerprint used to catch
// request changes that Magpie's per-day series may not expose.
func UsageFingerprint(summary magpie.UsageSummary) ([32]byte, error) {
	type totals struct {
		Calls        int
		Errors       int
		Input        int
		Output       int
		CacheRead    int
		CacheWrite   int
		CacheWrite1h int
		Reasoning    int
		Cost         float64
		Unpriced     int
	}
	type group struct {
		ID       string
		Provider string
		Model    string
		Account  string
		Agent    string
		Name     string
		Sub      string
		Icon     string
		totals
	}
	type point struct {
		Label string
		Time  time.Time
		totals
	}
	toTotals := func(value magpie.UsageTotals) totals {
		return totals{
			Calls: value.Calls, Errors: value.Errors, Input: value.Input, Output: value.Output,
			CacheRead: value.CacheRead, CacheWrite: value.CacheWrite, CacheWrite1h: value.CacheWrite1h,
			Reasoning: value.Reasoning, Cost: value.Cost, Unpriced: value.Unpriced,
		}
	}
	toGroup := func(value magpie.UsageGroup) group {
		return group{
			ID: value.ID, Provider: value.Provider, Model: value.Model, Account: value.Account,
			Agent: value.Agent, Name: value.Name, Sub: value.Sub, Icon: value.Icon,
			totals: toTotals(value.UsageTotals),
		}
	}
	type usageFacts struct {
		Totals     totals
		Series     []point
		Agents     []group
		Models     []group
		Accounts   []group
		CallerKeys []group
	}
	facts := usageFacts{Totals: toTotals(summary.UsageTotals)}
	for _, value := range summary.Series {
		facts.Series = append(facts.Series, point{Label: value.Label, Time: value.Time, totals: toTotals(value.UsageTotals)})
	}
	for _, value := range summary.Agents {
		facts.Agents = append(facts.Agents, toGroup(value))
	}
	for _, value := range summary.Models {
		facts.Models = append(facts.Models, toGroup(value))
	}
	for _, value := range summary.Accounts {
		facts.Accounts = append(facts.Accounts, toGroup(value))
	}
	for _, value := range summary.CallerKeys {
		facts.CallerKeys = append(facts.CallerKeys, toGroup(value))
	}
	sortFingerprintJSON(facts.Series)
	sortFingerprintJSON(facts.Agents)
	sortFingerprintJSON(facts.Models)
	sortFingerprintJSON(facts.Accounts)
	sortFingerprintJSON(facts.CallerKeys)
	data, err := json.Marshal(facts)
	if err != nil {
		return [32]byte{}, fmt.Errorf("query: fingerprint usage snapshot: %w", err)
	}
	return sha256.Sum256(data), nil
}

func sortFingerprintJSON[T any](values []T) {
	slices.SortFunc(values, func(a, b T) int {
		left, _ := json.Marshal(a)
		right, _ := json.Marshal(b)
		return cmp.Compare(string(left), string(right))
	})
}
