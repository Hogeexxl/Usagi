package storage

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/usage"
	sqlite "modernc.org/sqlite"
	sqlite3 "modernc.org/sqlite/lib"
)

type UsageEventMatch uint8

const (
	UsageEventAbsent UsageEventMatch = iota
	UsageEventIdentical
	UsageEventConflict
)

type UsageWriteOutcome uint8

const (
	UsageInserted UsageWriteOutcome = iota
	UsageDuplicate
)

func validateUsageWriteInput(source domain.SourceID, sourceEpoch int64, event usage.CanonicalUsageEventWrite) error {
	if err := source.Validate(); err != nil {
		return newStorageError(ErrorInvalidState, err)
	}
	if sourceEpoch <= 0 {
		return newStorageError(ErrorInvalidState, fmt.Errorf("source_epoch must be positive"))
	}
	if err := event.Validate(); err != nil {
		return newStorageError(ErrorInvalidState, err)
	}
	return nil
}

func validateUsageThreadSources(ctx context.Context, tx *sql.Tx, source domain.SourceID, event usage.CanonicalUsageEventWrite) error {
	for _, id := range []string{event.ThreadID, event.RootSessionID} {
		var threadSource domain.SourceID
		err := tx.QueryRowContext(ctx, "SELECT source FROM threads WHERE thread_id=?", id).Scan(&threadSource)
		if errors.Is(err, sql.ErrNoRows) {
			return newStorageError(ErrorInvalidState, fmt.Errorf("usage thread %q does not exist", id))
		}
		if err != nil {
			return mapSQLiteError(err)
		}
		if threadSource != source {
			return newStorageError(ErrorInvalidState, fmt.Errorf("usage thread %q has a different source", id))
		}
	}
	return nil
}

const canonicalUsageColumns = `event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,reasoning_effort,input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,quality_status`

type canonicalUsagePayload struct {
	kind             usage.EventKind
	occurredAtMS     int64
	threadID         string
	rootSessionID    string
	turnKey          sql.NullString
	model            string
	reasoningEffort  sql.NullString
	inputTokens      int64
	cachedTokens     int64
	cacheWriteTokens sql.NullInt64
	outputTokens     int64
	reasoningTokens  int64
	totalTokens      int64
	qualityStatus    string
}

func payloadFromInput(event usage.CanonicalUsageEventWrite) canonicalUsagePayload {
	quality := deriveUsageQuality(event.Usage.CacheWriteTokens)
	payload := canonicalUsagePayload{
		kind: event.Kind, occurredAtMS: event.OccurredAtMS, threadID: event.ThreadID, rootSessionID: event.RootSessionID,
		model: event.Model, inputTokens: event.Usage.InputTokens, cachedTokens: event.Usage.CachedTokens,
		outputTokens: event.Usage.OutputTokens, reasoningTokens: event.Usage.ReasoningTokens, totalTokens: event.Usage.TotalTokens,
		qualityStatus: quality,
	}
	if event.TurnKey != nil {
		payload.turnKey = sql.NullString{String: *event.TurnKey, Valid: true}
	}
	if event.ReasoningEffort != nil {
		payload.reasoningEffort = sql.NullString{String: *event.ReasoningEffort, Valid: true}
	}
	if event.Usage.CacheWriteTokens != nil {
		payload.cacheWriteTokens = sql.NullInt64{Int64: *event.Usage.CacheWriteTokens, Valid: true}
	}
	return payload
}

func scanCanonicalUsagePayload(row *sql.Row) (canonicalUsagePayload, error) {
	var payload canonicalUsagePayload
	err := row.Scan(&payload.kind, &payload.occurredAtMS, &payload.threadID, &payload.rootSessionID,
		&payload.turnKey, &payload.model, &payload.reasoningEffort, &payload.inputTokens, &payload.cachedTokens,
		&payload.cacheWriteTokens, &payload.outputTokens, &payload.reasoningTokens, &payload.totalTokens, &payload.qualityStatus)
	return payload, err
}

func canonicalUsageEqual(a, b canonicalUsagePayload) bool {
	return a.kind == b.kind && a.occurredAtMS == b.occurredAtMS && a.threadID == b.threadID &&
		a.rootSessionID == b.rootSessionID && a.turnKey == b.turnKey && a.model == b.model &&
		a.reasoningEffort == b.reasoningEffort && a.inputTokens == b.inputTokens && a.cachedTokens == b.cachedTokens &&
		a.cacheWriteTokens == b.cacheWriteTokens && a.outputTokens == b.outputTokens &&
		a.reasoningTokens == b.reasoningTokens && a.totalTokens == b.totalTokens && a.qualityStatus == b.qualityStatus
}

func (tx *Tx) CompareUsageEvent(ctx context.Context, source domain.SourceID, sourceEpoch int64, event usage.CanonicalUsageEventWrite) (UsageEventMatch, error) {
	if err := validateUsageWriteInput(source, sourceEpoch, event); err != nil {
		return UsageEventAbsent, err
	}
	existing, err := scanCanonicalUsagePayload(tx.tx.QueryRowContext(ctx,
		"SELECT "+canonicalUsageColumns+" FROM usage_events WHERE source=? AND source_epoch=? AND event_id=?", source, sourceEpoch, event.EventID))
	if errors.Is(err, sql.ErrNoRows) {
		return UsageEventAbsent, nil
	}
	if err != nil {
		return UsageEventAbsent, mapSQLiteError(err)
	}
	if canonicalUsageEqual(existing, payloadFromInput(event)) {
		return UsageEventIdentical, nil
	}
	return UsageEventConflict, nil
}

func (tx *Tx) WriteUsageEvent(ctx context.Context, source domain.SourceID, sourceEpoch int64, event usage.CanonicalUsageEventWrite) (UsageWriteOutcome, error) {
	match, err := tx.CompareUsageEvent(ctx, source, sourceEpoch, event)
	if err != nil {
		return UsageInserted, err
	}
	switch match {
	case UsageEventIdentical:
		return UsageDuplicate, nil
	case UsageEventConflict:
		return UsageInserted, usageConflictError(event.EventID)
	}
	if err := validateUsageThreadSources(ctx, tx.tx, source, event); err != nil {
		return UsageInserted, err
	}
	quality := deriveUsageQuality(event.Usage.CacheWriteTokens)
	_, insertErr := tx.tx.ExecContext(ctx, `INSERT INTO usage_events
  (source,source_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,reasoning_effort,estimated_cost_nanos_usd,input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,quality_status,created_at_ms)
  VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
		source, sourceEpoch, event.EventID, event.Kind, event.OccurredAtMS, event.ThreadID, event.RootSessionID,
		event.TurnKey, event.Model, event.ReasoningEffort, event.EstimatedCostNanosUSD,
		event.Usage.InputTokens, event.Usage.CachedTokens, event.Usage.CacheWriteTokens, event.Usage.OutputTokens,
		event.Usage.ReasoningTokens, event.Usage.TotalTokens, quality, event.CreatedAtMS)
	if insertErr == nil {
		return UsageInserted, nil
	}
	var sqliteErr *sqlite.Error
	if errors.As(insertErr, &sqliteErr) && (sqliteErr.Code() == sqlite3.SQLITE_CONSTRAINT_PRIMARYKEY || sqliteErr.Code() == sqlite3.SQLITE_CONSTRAINT_UNIQUE) {
		match, err := tx.CompareUsageEvent(ctx, source, sourceEpoch, event)
		if err != nil {
			return UsageInserted, err
		}
		switch match {
		case UsageEventIdentical:
			return UsageDuplicate, nil
		case UsageEventConflict:
			return UsageInserted, usageConflictError(event.EventID)
		}
	}
	return UsageInserted, mapSQLiteError(insertErr)
}

func usageConflictError(eventID string) error {
	return newStorageError(ErrorInvalidState, fmt.Errorf("canonical usage event %q conflicts with existing payload", eventID))
}

func deriveUsageQuality(cacheWriteTokens *int64) string {
	if cacheWriteTokens == nil {
		return "partial"
	}
	return "complete"
}
