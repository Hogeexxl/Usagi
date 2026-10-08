package storage

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/usage"
)

const usageVisibilityProjection = "event_id,event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,reasoning_effort,estimated_cost_nanos_usd,input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,quality_status"

func (tx *Tx) CanonicalUsageProjectionEqual(
	ctx context.Context,
	source domain.SourceID,
	activeEpoch int64,
	buildEpoch int64,
) (bool, error) {
	if err := source.Validate(); err != nil {
		return false, newStorageError(ErrorInvalidState, err)
	}
	if activeEpoch < 0 || buildEpoch < 1 {
		return false, newStorageError(ErrorInvalidState, fmt.Errorf("invalid usage epoch projection pair"))
	}
	different, err := tx.usageProjectionHasDifference(ctx, source, activeEpoch, buildEpoch)
	if err != nil || different {
		return false, err
	}
	different, err = tx.usageProjectionHasDifference(ctx, source, buildEpoch, activeEpoch)
	return !different, err
}

func (tx *Tx) usageProjectionHasDifference(
	ctx context.Context,
	source domain.SourceID,
	leftEpoch int64,
	rightEpoch int64,
) (bool, error) {
	query := "SELECT 1 FROM (SELECT " + usageVisibilityProjection +
		" FROM usage_events WHERE source=? AND source_epoch=? EXCEPT SELECT " + usageVisibilityProjection +
		" FROM usage_events WHERE source=? AND source_epoch=?) LIMIT 1"
	var found int
	err := tx.tx.QueryRowContext(ctx, query, source, leftEpoch, source, rightEpoch).Scan(&found)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, mapSQLiteError(err)
	}
	return true, nil
}

func (tx *Tx) CopyUsageEvent(
	ctx context.Context,
	source domain.SourceID,
	fromEpoch int64,
	toEpoch int64,
	eventID string,
) (UsageWriteOutcome, error) {
	if err := source.Validate(); err != nil {
		return UsageInserted, newStorageError(ErrorInvalidState, err)
	}
	if fromEpoch <= 0 || toEpoch <= 0 {
		return UsageInserted, newStorageError(ErrorInvalidState, fmt.Errorf("usage epochs must be positive"))
	}
	if fromEpoch == toEpoch {
		return UsageInserted, newStorageError(ErrorInvalidState, fmt.Errorf("canonical usage copy requires distinct epochs"))
	}
	var event usage.CanonicalUsageEventWrite
	var kind string
	var turnKey, reasoningEffort sql.NullString
	var estimatedCost, cacheWriteTokens sql.NullInt64
	err := tx.tx.QueryRowContext(ctx,
		"SELECT event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,reasoning_effort,estimated_cost_nanos_usd,input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,created_at_ms FROM usage_events WHERE source=? AND source_epoch=? AND event_id=?",
		source,
		fromEpoch,
		eventID,
	).Scan(
		&kind,
		&event.OccurredAtMS,
		&event.ThreadID,
		&event.RootSessionID,
		&turnKey,
		&event.Model,
		&reasoningEffort,
		&estimatedCost,
		&event.Usage.InputTokens,
		&event.Usage.CachedTokens,
		&cacheWriteTokens,
		&event.Usage.OutputTokens,
		&event.Usage.ReasoningTokens,
		&event.Usage.TotalTokens,
		&event.CreatedAtMS,
	)
	if errors.Is(err, sql.ErrNoRows) {
		return UsageInserted, newStorageError(ErrorInvalidState, fmt.Errorf("canonical usage copy source event is missing"))
	}
	if err != nil {
		return UsageInserted, mapSQLiteError(err)
	}
	event.EventID = eventID
	event.Kind = usage.EventKind(kind)
	event.TurnKey = nullableStringPointer(turnKey)
	event.ReasoningEffort = nullableStringPointer(reasoningEffort)
	event.EstimatedCostNanosUSD = nullableInt64Pointer(estimatedCost)
	event.Usage.CacheWriteTokens = nullableInt64Pointer(cacheWriteTokens)
	return tx.WriteUsageEvent(ctx, source, toEpoch, event)
}

func (tx *Tx) DeleteUsageEvents(
	ctx context.Context,
	source domain.SourceID,
	epoch int64,
	eventIDs []string,
) (int, error) {
	if len(eventIDs) == 0 {
		return 0, nil
	}
	if err := source.Validate(); err != nil {
		return 0, newStorageError(ErrorInvalidState, err)
	}
	if epoch <= 0 {
		return 0, newStorageError(ErrorInvalidState, fmt.Errorf("source_epoch must be positive"))
	}
	deleted := 0
	for _, eventID := range eventIDs {
		result, err := tx.tx.ExecContext(ctx,
			"DELETE FROM usage_events WHERE source=? AND source_epoch=? AND event_id=?",
			source,
			epoch,
			eventID,
		)
		if err != nil {
			return 0, mapSQLiteError(err)
		}
		rows, err := result.RowsAffected()
		if err != nil {
			return 0, mapSQLiteError(err)
		}
		deleted += int(rows)
	}
	return deleted, nil
}

func (tx *Tx) RebindUsageRoot(
	ctx context.Context,
	source domain.SourceID,
	epoch int64,
	threadID string,
	nextRootSessionID string,
) (int, error) {
	if err := source.Validate(); err != nil {
		return 0, newStorageError(ErrorInvalidState, err)
	}
	if epoch <= 0 {
		return 0, newStorageError(ErrorInvalidState, fmt.Errorf("source_epoch must be positive"))
	}
	result, err := tx.tx.ExecContext(ctx,
		"UPDATE usage_events SET root_session_id=? WHERE source=? AND source_epoch=? AND thread_id=?",
		nextRootSessionID,
		source,
		epoch,
		threadID,
	)
	if err != nil {
		return 0, mapSQLiteError(err)
	}
	rows, err := result.RowsAffected()
	if err != nil {
		return 0, mapSQLiteError(err)
	}
	return int(rows), nil
}

func nullableStringPointer(value sql.NullString) *string {
	if !value.Valid {
		return nil
	}
	return &value.String
}
