package storage

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math"

	"github.com/Hogeexxl/Usagi/internal/domain"
)

func (db *DB) GetSourceUsageEpoch(
	ctx context.Context,
	source domain.SourceID,
) (domain.SourceUsageEpochState, bool, error) {
	if err := source.Validate(); err != nil {
		return domain.SourceUsageEpochState{}, false, newStorageError(ErrorInvalidState, err)
	}
	var activeEpoch, activeParserVersion int64
	var buildEpoch, buildParserVersion sql.NullInt64
	err := db.readers.QueryRowContext(ctx,
		"SELECT active_epoch,build_epoch,active_parser_version,build_parser_version FROM source_usage_epochs WHERE source=?",
		source,
	).Scan(&activeEpoch, &buildEpoch, &activeParserVersion, &buildParserVersion)
	if errors.Is(err, sql.ErrNoRows) {
		return domain.SourceUsageEpochState{}, false, nil
	}
	if err != nil {
		return domain.SourceUsageEpochState{}, false, mapSQLiteError(err)
	}
	state, err := domain.NewSourceUsageEpochState(
		source,
		activeEpoch,
		nullableInt64Pointer(buildEpoch),
		activeParserVersion,
		nullableInt64Pointer(buildParserVersion),
	)
	if err != nil {
		return domain.SourceUsageEpochState{}, false, newStorageError(ErrorInvalidState, err)
	}
	return state, true, nil
}

func (tx *Tx) EnsureSourceUsageEpoch(ctx context.Context, source domain.SourceID) error {
	if err := source.Validate(); err != nil {
		return newStorageError(ErrorInvalidState, err)
	}
	_, err := tx.tx.ExecContext(ctx,
		"INSERT INTO source_usage_epochs(source,active_epoch,build_epoch,active_parser_version,build_parser_version) VALUES(?,0,NULL,0,NULL) ON CONFLICT(source) DO NOTHING",
		source,
	)
	return mapSQLiteError(err)
}

func (tx *Tx) SourceUsageEpoch(ctx context.Context, source domain.SourceID) (domain.SourceUsageEpochState, error) {
	if err := source.Validate(); err != nil {
		return domain.SourceUsageEpochState{}, newStorageError(ErrorInvalidState, err)
	}
	var activeEpoch, activeParserVersion int64
	var buildEpoch, buildParserVersion sql.NullInt64
	err := tx.tx.QueryRowContext(ctx,
		"SELECT active_epoch,build_epoch,active_parser_version,build_parser_version FROM source_usage_epochs WHERE source=?",
		source,
	).Scan(&activeEpoch, &buildEpoch, &activeParserVersion, &buildParserVersion)
	if errors.Is(err, sql.ErrNoRows) {
		return domain.SourceUsageEpochState{}, newStorageError(ErrorInvalidState, fmt.Errorf("source usage epoch does not exist"))
	}
	if err != nil {
		return domain.SourceUsageEpochState{}, mapSQLiteError(err)
	}
	state, err := domain.NewSourceUsageEpochState(
		source,
		activeEpoch,
		nullableInt64Pointer(buildEpoch),
		activeParserVersion,
		nullableInt64Pointer(buildParserVersion),
	)
	if err != nil {
		return domain.SourceUsageEpochState{}, newStorageError(ErrorInvalidState, err)
	}
	return state, nil
}

func (tx *Tx) BeginOrResumeSourceUsageBuild(
	ctx context.Context,
	source domain.SourceID,
	parserVersion int64,
) (int64, error) {
	if parserVersion < 0 {
		return 0, newStorageError(ErrorInvalidState, fmt.Errorf("usage parser version must be non-negative"))
	}
	if err := tx.EnsureSourceUsageEpoch(ctx, source); err != nil {
		return 0, err
	}
	state, err := tx.SourceUsageEpoch(ctx, source)
	if err != nil {
		return 0, err
	}
	if state.BuildEpoch != nil {
		if *state.BuildParserVersion == parserVersion {
			return *state.BuildEpoch, nil
		}
		return 0, newStorageError(ErrorInvalidState, fmt.Errorf("a different usage build is already active for this source"))
	}
	if state.ActiveEpoch == math.MaxInt64 {
		return 0, newStorageError(ErrorInvalidState, fmt.Errorf("usage epoch overflow"))
	}
	buildEpoch := state.ActiveEpoch + 1
	result, err := tx.tx.ExecContext(ctx,
		"UPDATE source_usage_epochs SET build_epoch=?,build_parser_version=? WHERE source=? AND active_epoch=? AND build_epoch IS NULL",
		buildEpoch,
		parserVersion,
		source,
		state.ActiveEpoch,
	)
	if err != nil {
		return 0, mapSQLiteError(err)
	}
	rows, err := result.RowsAffected()
	if err != nil {
		return 0, mapSQLiteError(err)
	}
	if rows != 1 {
		return 0, newStorageError(ErrorInvalidState, fmt.Errorf("usage epoch build CAS failed"))
	}
	return buildEpoch, nil
}

func (tx *Tx) RetargetSourceUsageBuild(
	ctx context.Context,
	source domain.SourceID,
	expectedBuildEpoch int64,
	expectedOldParserVersion int64,
	newParserVersion int64,
) error {
	if err := source.Validate(); err != nil {
		return newStorageError(ErrorInvalidState, err)
	}
	result, err := tx.tx.ExecContext(ctx,
		"UPDATE source_usage_epochs SET build_parser_version=? WHERE source=? AND build_epoch=? AND build_parser_version=?",
		newParserVersion,
		source,
		expectedBuildEpoch,
		expectedOldParserVersion,
	)
	if err != nil {
		return mapSQLiteError(err)
	}
	rows, err := result.RowsAffected()
	if err != nil {
		return mapSQLiteError(err)
	}
	if rows != 1 {
		return newStorageError(ErrorInvalidState, fmt.Errorf("usage build parser retarget CAS failed"))
	}
	return nil
}

func (tx *Tx) ActivateSourceUsageBuild(
	ctx context.Context,
	source domain.SourceID,
	expectedBuildEpoch int64,
	expectedParserVersion int64,
) error {
	if err := source.Validate(); err != nil {
		return newStorageError(ErrorInvalidState, err)
	}
	result, err := tx.tx.ExecContext(ctx,
		"UPDATE source_usage_epochs SET active_epoch=build_epoch,active_parser_version=build_parser_version,build_epoch=NULL,build_parser_version=NULL WHERE source=? AND build_epoch=? AND build_parser_version=?",
		source,
		expectedBuildEpoch,
		expectedParserVersion,
	)
	if err != nil {
		return mapSQLiteError(err)
	}
	rows, err := result.RowsAffected()
	if err != nil {
		return mapSQLiteError(err)
	}
	if rows != 1 {
		return newStorageError(ErrorInvalidState, fmt.Errorf("usage build activation CAS failed"))
	}
	return nil
}

func nullableInt64Pointer(value sql.NullInt64) *int64 {
	if !value.Valid {
		return nil
	}
	return &value.Int64
}
