package usage

import (
	"bytes"
	"database/sql"
	"fmt"
	"strings"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

const turnColumns = `raw_turn_id,started_at_ms,ended_at_ms,start_offset,end_offset,status,
	start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,
	start_total_output_tokens,start_total_reasoning_tokens,start_total_total_tokens,start_total_fingerprint,
	last_total_input_tokens,last_total_cached_tokens,last_total_cache_write_tokens,
	last_total_output_tokens,last_total_reasoning_tokens,last_total_total_tokens,last_total_fingerprint,
	accounted_input_tokens,accounted_cached_tokens,accounted_cache_write_tokens,
	accounted_output_tokens,accounted_reasoning_tokens,accounted_total_tokens,accounted_fingerprint,
	accounted_candidate_count,model_state,single_model,unresolved_model_seen,
	reasoning_effort_state,single_reasoning_effort,unresolved_reasoning_effort_seen,
	compensation_allowed,block_start_missing,block_time_missing,block_reset,block_ownership_gap,
	block_parser_gap,block_required_invalid,block_model_unresolved,quality_status,state_through_offset,updated_at_ms`

func loadOpenTurn(private storage.PrivateTx, epoch int64, state SourceState) (*TurnWrite, error) {
	rows, err := private.Query(`SELECT turn_key,thread_id,`+turnColumns+`
		FROM codex_turns WHERE ledger_epoch=? AND source_file_id=? AND file_generation=? AND status='open'
		ORDER BY start_offset`, epoch, state.SourceFileID, state.Generation)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var turn *TurnWrite
	for rows.Next() {
		if turn != nil {
			return nil, fmt.Errorf("multiple open usage turns for source")
		}
		value, err := scanTurn(rows, state.SourceFileID, state.Generation, state.OwningThreadID)
		if err != nil {
			return nil, err
		}
		turn = &value
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return turn, nil
}

type turnRowScanner interface {
	Scan(dest ...any) error
}

func scanTurn(row turnRowScanner, sourceFileID, generation int64, ownerThreadID string) (TurnWrite, error) {
	turn := TurnWrite{SourceFileID: sourceFileID, Generation: generation, ThreadID: ownerThreadID}
	var turnKey, threadID, rawID sql.NullString
	var startedAt, endedAt, endOffset sql.NullInt64
	var status, modelState, singleModel, effortState, singleEffort, quality sql.NullString
	var startInput, startCached, startCacheWrite, startOutput, startReasoning, startTotal sql.NullInt64
	var startFingerprint []byte
	var lastInput, lastCached, lastCacheWrite, lastOutput, lastReasoning, lastTotal sql.NullInt64
	var lastFingerprint []byte
	var accountedInput, accountedCached, accountedCacheWrite, accountedOutput, accountedReasoning, accountedTotal sql.NullInt64
	var accountedFingerprint []byte
	var candidateCount, unresolvedModel, unresolvedEffort sql.NullInt64
	var compensationAllowed, startMissing, timeMissing, reset, ownershipGap, parserGap, requiredInvalid, modelUnresolved sql.NullInt64
	var stateThrough, updatedAt sql.NullInt64
	err := row.Scan(
		&turnKey, &threadID, &rawID, &startedAt, &endedAt, &turn.StartOffset, &endOffset, &status,
		&startInput, &startCached, &startCacheWrite, &startOutput, &startReasoning, &startTotal, &startFingerprint,
		&lastInput, &lastCached, &lastCacheWrite, &lastOutput, &lastReasoning, &lastTotal, &lastFingerprint,
		&accountedInput, &accountedCached, &accountedCacheWrite, &accountedOutput, &accountedReasoning, &accountedTotal, &accountedFingerprint,
		&candidateCount, &modelState, &singleModel, &unresolvedModel,
		&effortState, &singleEffort, &unresolvedEffort,
		&compensationAllowed, &startMissing, &timeMissing, &reset, &ownershipGap, &parserGap, &requiredInvalid, &modelUnresolved,
		&quality, &stateThrough, &updatedAt,
	)
	if err != nil {
		return TurnWrite{}, err
	}
	if rawID.Valid {
		turn.RawTurnID = &rawID.String
	}
	if !turnKey.Valid || !threadID.Valid {
		return TurnWrite{}, ErrInvalidReconciliationCarry
	}
	turn.TurnKey = turnKey.String
	turn.ThreadID = threadID.String
	turn.StartedAtMS = nullableIntPointer(startedAt)
	turn.EndedAtMS = nullableIntPointer(endedAt)
	if endOffset.Valid {
		turn.EndOffset = &endOffset.Int64
	}
	turn.Status = TurnStatus(status.String)
	turn.StartTotal, err = decodeTurnTotal(startInput, startCached, startCacheWrite, startOutput, startReasoning, startTotal, startFingerprint, true)
	if err != nil {
		return TurnWrite{}, err
	}
	turn.LastTotal, err = decodeTurnTotal(lastInput, lastCached, lastCacheWrite, lastOutput, lastReasoning, lastTotal, lastFingerprint, true)
	if err != nil {
		return TurnWrite{}, err
	}
	accounted, err := decodeTurnTotal(accountedInput, accountedCached, accountedCacheWrite, accountedOutput, accountedReasoning, accountedTotal, accountedFingerprint, false)
	if err != nil {
		return TurnWrite{}, err
	}
	turn.Accounted = *accounted
	if !candidateCount.Valid || !modelState.Valid || !effortState.Valid || !quality.Valid || !stateThrough.Valid || !updatedAt.Valid {
		return TurnWrite{}, ErrInvalidReconciliationCarry
	}
	turn.AccountedCandidateCount = candidateCount.Int64
	turn.ModelState = TurnValueState(modelState.String)
	if singleModel.Valid {
		turn.SingleModel = &singleModel.String
	}
	turn.UnresolvedModelSeen = unresolvedModel.Int64 != 0
	turn.ReasoningEffortState = TurnValueState(effortState.String)
	if singleEffort.Valid {
		turn.SingleReasoningEffort = &singleEffort.String
	}
	turn.UnresolvedReasoningEffortSeen = unresolvedEffort.Int64 != 0
	turn.Blocks = CompensationBlocks{
		StartMissing: startMissing.Int64 != 0, TimeMissing: timeMissing.Int64 != 0,
		Reset: reset.Int64 != 0, OwnershipGap: ownershipGap.Int64 != 0,
		ParserGap: parserGap.Int64 != 0, RequiredInvalid: requiredInvalid.Int64 != 0,
		ModelUnresolved: modelUnresolved.Int64 != 0,
	}
	if compensationAllowed.Int64 != boolInt(turnCompensationAllowed(turn.Blocks)) {
		return TurnWrite{}, ErrInvalidReconciliationCarry
	}
	turn.QualityStatus = quality.String
	turn.StateThroughOffset = stateThrough.Int64
	turn.UpdatedAtMS = updatedAt.Int64
	if err := validateTurn(&turn); err != nil {
		return TurnWrite{}, err
	}
	return turn, nil
}

func decodeTurnTotal(input, cached, cacheWrite, output, reasoning, total sql.NullInt64, fingerprint []byte, optional bool) (*sharedusage.NormalizedTokenUsage, error) {
	parts := []sql.NullInt64{input, cached, output, reasoning, total}
	missing, present := true, true
	for _, value := range parts {
		missing = missing && !value.Valid
		present = present && value.Valid
	}
	if missing && !cacheWrite.Valid && len(fingerprint) == 0 {
		if optional {
			return nil, nil
		}
		return nil, ErrInvalidReconciliationCarry
	}
	if !present || len(fingerprint) != 32 {
		return nil, ErrInvalidReconciliationCarry
	}
	value := sharedusage.NormalizedTokenUsage{
		InputTokens: input.Int64, CachedTokens: cached.Int64, CacheWriteTokens: nullableIntPointer(cacheWrite),
		OutputTokens: output.Int64, ReasoningTokens: reasoning.Int64, TotalTokens: total.Int64,
	}
	if value.Validate() != nil {
		return nil, ErrInvalidReconciliationCarry
	}
	want := UsageFingerprint(value)
	if !bytes.Equal(fingerprint, want[:]) {
		return nil, ErrInvalidReconciliationCarry
	}
	return &value, nil
}

func WriteTurn(tx *source.WriteTx, target source.UsageWriteTarget, turn TurnWrite) error {
	epoch, err := tx.ResolveUsageWriteEpoch(target)
	if err != nil {
		return err
	}
	if err := validateTurn(&turn); err != nil {
		return err
	}
	values := []any{epoch, turn.SourceFileID, turn.Generation, turn.TurnKey, turn.ThreadID,
		nullableStringValue(turn.RawTurnID), nullableIntValue(turn.StartedAtMS), nullableIntValue(turn.EndedAtMS),
		turn.StartOffset, nullableIntValue(turn.EndOffset), turn.Status}
	values = appendTurnTotal(values, turn.StartTotal)
	values = appendTurnTotal(values, turn.LastTotal)
	values = appendTurnTotal(values, &turn.Accounted)
	values = append(values, turn.AccountedCandidateCount, turn.ModelState, nullableStringValue(turn.SingleModel),
		boolInt(turn.UnresolvedModelSeen), turn.ReasoningEffortState, nullableStringValue(turn.SingleReasoningEffort),
		boolInt(turn.UnresolvedReasoningEffortSeen), boolInt(turnCompensationAllowed(turn.Blocks)),
		boolInt(turn.Blocks.StartMissing), boolInt(turn.Blocks.TimeMissing), boolInt(turn.Blocks.Reset),
		boolInt(turn.Blocks.OwnershipGap), boolInt(turn.Blocks.ParserGap), boolInt(turn.Blocks.RequiredInvalid),
		boolInt(turn.Blocks.ModelUnresolved), turn.QualityStatus, turn.StateThroughOffset, turn.UpdatedAtMS)
	columnNames := []string{
		"ledger_epoch", "source_file_id", "file_generation", "turn_key", "thread_id", "raw_turn_id",
		"started_at_ms", "ended_at_ms", "start_offset", "end_offset", "status",
		"start_total_input_tokens", "start_total_cached_tokens", "start_total_cache_write_tokens", "start_total_output_tokens", "start_total_reasoning_tokens", "start_total_total_tokens", "start_total_fingerprint",
		"last_total_input_tokens", "last_total_cached_tokens", "last_total_cache_write_tokens", "last_total_output_tokens", "last_total_reasoning_tokens", "last_total_total_tokens", "last_total_fingerprint",
		"accounted_input_tokens", "accounted_cached_tokens", "accounted_cache_write_tokens", "accounted_output_tokens", "accounted_reasoning_tokens", "accounted_total_tokens", "accounted_fingerprint",
		"accounted_candidate_count", "model_state", "single_model", "unresolved_model_seen", "reasoning_effort_state", "single_reasoning_effort", "unresolved_reasoning_effort_seen",
		"compensation_allowed", "block_start_missing", "block_time_missing", "block_reset", "block_ownership_gap", "block_parser_gap", "block_required_invalid", "block_model_unresolved", "quality_status", "state_through_offset", "updated_at_ms",
	}
	placeholders := strings.TrimSuffix(strings.Repeat("?,", len(values)), ",")
	updates := make([]string, 0, len(columnNames)-4)
	for _, column := range columnNames[4:] {
		updates = append(updates, column+"=excluded."+column)
	}
	query := "INSERT INTO codex_turns (" + strings.Join(columnNames, ",") + ") VALUES (" + placeholders + ") ON CONFLICT(ledger_epoch,source_file_id,file_generation,turn_key) DO UPDATE SET " + strings.Join(updates, ",")
	return tx.Private(func(private storage.PrivateTx) error {
		_, err := private.Exec(query, values...)
		return err
	})
}

func appendTurnTotal(values []any, value *sharedusage.NormalizedTokenUsage) []any {
	if value == nil {
		return append(values, nil, nil, nil, nil, nil, nil, nil)
	}
	fingerprint := UsageFingerprint(*value)
	return append(values, value.InputTokens, value.CachedTokens, nullableIntValue(value.CacheWriteTokens),
		value.OutputTokens, value.ReasoningTokens, value.TotalTokens, fingerprint[:])
}

func validateTurn(turn *TurnWrite) error {
	if turn.SourceFileID <= 0 || turn.Generation <= 0 || !validCarryIdentity(turn.TurnKey) ||
		!validCarryIdentity(turn.ThreadID) || turn.StartOffset < 0 || turn.StateThroughOffset < turn.StartOffset ||
		turn.UpdatedAtMS < 0 || turn.AccountedCandidateCount < 0 || turn.Accounted.Validate() != nil {
		return ErrInvalidReconciliationCarry
	}
	if turn.RawTurnID != nil && !validCarryIdentity(*turn.RawTurnID) {
		return ErrInvalidReconciliationCarry
	}
	if turn.StartedAtMS != nil && *turn.StartedAtMS < 0 || turn.EndedAtMS != nil && *turn.EndedAtMS < 0 {
		return ErrInvalidReconciliationCarry
	}
	if turn.StartTotal != nil && turn.StartTotal.Validate() != nil || turn.LastTotal != nil && turn.LastTotal.Validate() != nil {
		return ErrInvalidReconciliationCarry
	}
	switch turn.Status {
	case TurnOpen:
		if turn.EndOffset != nil || turn.EndedAtMS != nil {
			return ErrInvalidReconciliationCarry
		}
	default:
		if turn.Status != TurnCompleted && turn.Status != TurnAborted && turn.Status != TurnFailed ||
			turn.EndOffset == nil || *turn.EndOffset <= turn.StartOffset {
			return ErrInvalidReconciliationCarry
		}
	}
	if turn.ModelState == TurnValueSingle {
		if turn.SingleModel == nil || !validCarryIdentity(*turn.SingleModel) {
			return ErrInvalidReconciliationCarry
		}
	} else if (turn.ModelState != TurnValueNone && turn.ModelState != TurnValueMixed) || turn.SingleModel != nil {
		return ErrInvalidReconciliationCarry
	}
	if turn.ReasoningEffortState == TurnValueSingle {
		if turn.SingleReasoningEffort == nil || !validCarryIdentity(*turn.SingleReasoningEffort) {
			return ErrInvalidReconciliationCarry
		}
	} else if (turn.ReasoningEffortState != TurnValueNone && turn.ReasoningEffortState != TurnValueMixed) || turn.SingleReasoningEffort != nil {
		return ErrInvalidReconciliationCarry
	}
	if turn.QualityStatus != "complete" && turn.QualityStatus != "partial" && turn.QualityStatus != "conflict" {
		return ErrInvalidReconciliationCarry
	}
	return nil
}

func turnCompensationAllowed(blocks CompensationBlocks) bool {
	return !blocks.StartMissing && !blocks.TimeMissing && !blocks.Reset && !blocks.OwnershipGap &&
		!blocks.ParserGap && !blocks.RequiredInvalid && !blocks.ModelUnresolved
}

func boolInt(value bool) int64 {
	if value {
		return 1
	}
	return 0
}
