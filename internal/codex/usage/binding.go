package usage

import (
	"fmt"
	"reflect"
	"sort"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

func ReconcileMetadataUsageBinding(
	tx *source.WriteTx,
	deps BindingReconcileDeps,
	threadID string,
	previousRoot *string,
	nextRoot *string,
	bindingChangedSourceIDs []int64,
	committedAtMS int64,
) (visibleChanged bool, needsShadowBuild bool, invalidatedSourceFileIDs []int64, retryRootIDs []string, err error) {
	if !validCarryIdentity(threadID) || committedAtMS < 0 ||
		(previousRoot != nil && !validCarryIdentity(*previousRoot)) ||
		(nextRoot != nil && !validCarryIdentity(*nextRoot)) {
		return false, false, nil, nil, fmt.Errorf("invalid usage binding input")
	}
	sourceIDs, err := normalizedSourceIDs(bindingChangedSourceIDs)
	if err != nil {
		return false, false, nil, nil, err
	}
	epoch, err := tx.UsageEpochState()
	if err != nil {
		return false, false, nil, nil, err
	}
	rootChanged := !sameOptionalString(previousRoot, nextRoot)
	needsShadowBuild = nextRoot == nil
	hasBindingChange := rootChanged || len(sourceIDs) > 0
	if !hasBindingChange {
		return false, needsShadowBuild, nil, nil, nil
	}
	activeContributors := make(map[int64]bool)
	if epoch.ActiveEpoch > 0 {
		activeContributors, err = activeSourceContributors(tx, epoch.ActiveEpoch, sourceIDs)
		if err != nil {
			return false, false, nil, nil, err
		}
		for _, sourceID := range sourceIDs {
			if activeContributors[sourceID] {
				invalidatedSourceFileIDs = append(invalidatedSourceFileIDs, sourceID)
				needsShadowBuild = true
			}
		}
	}

	var before CompactionVisibilityProjection
	projectActive := epoch.ActiveEpoch > 0 && deps.ProjectCompaction != nil
	if epoch.ActiveEpoch > 0 && deps.ProjectCompaction == nil {
		return false, false, nil, nil, fmt.Errorf("active compaction projector is required")
	}
	if projectActive {
		before, err = deps.ProjectCompaction(tx, source.UsageTargetActive, threadID)
		if err != nil {
			return false, false, nil, nil, err
		}
	}

	if rootChanged && nextRoot != nil {
		if epoch.ActiveEpoch > 0 {
			canonicalChanged, countErr := countCanonicalRootChanges(tx, epoch.ActiveEpoch, threadID, previousRoot)
			if countErr != nil {
				return false, false, nil, nil, countErr
			}
			_, rebindErr := tx.RebindUsageRootNoRevision(source.UsageTargetActive, threadID, *nextRoot)
			if rebindErr != nil {
				return false, false, nil, nil, rebindErr
			}
			visibleChanged = visibleChanged || canonicalChanged > 0
			skillRowsChanged, rebindErr := rebindPrivateRoots(tx, epoch.ActiveEpoch, threadID, previousRoot, *nextRoot)
			if rebindErr != nil {
				return false, false, nil, nil, rebindErr
			}
			visibleChanged = visibleChanged || skillRowsChanged
		}
		if epoch.BuildEpoch != nil {
			if _, err := tx.RebindUsageRootNoRevision(source.UsageTargetBuild, threadID, *nextRoot); err != nil {
				return false, false, nil, nil, err
			}
			if _, err := rebindPrivateRoots(tx, *epoch.BuildEpoch, threadID, previousRoot, *nextRoot); err != nil {
				return false, false, nil, nil, err
			}
		}
	}

	if epoch.ActiveEpoch > 0 {
		deleted, quarantineSources, quarantineRoots, deleteErr := deleteIntersectingActiveQuarantines(
			tx, epoch.ActiveEpoch, previousRoot, sourceIDs,
		)
		if deleteErr != nil {
			return false, false, nil, nil, deleteErr
		}
		if deleted {
			visibleChanged = true
			needsShadowBuild = true
			invalidatedSourceFileIDs = append(invalidatedSourceFileIDs, quarantineSources...)
			retryRootIDs = append(retryRootIDs, quarantineRoots...)
		}
	}

	if epoch.BuildEpoch != nil && (rootChanged || len(sourceIDs) > 0) {
		if deps.InvalidateBuild == nil {
			return false, false, nil, nil, fmt.Errorf("build binding invalidator is required")
		}
		result, invalidateErr := deps.InvalidateBuild(tx, BuildBindingInvalidationRequest{
			ThreadID: threadID, PreviousRoot: cloneString(previousRoot), NextRoot: cloneString(nextRoot),
			BindingChangedSourceIDs: append([]int64(nil), sourceIDs...), CommittedAtMS: committedAtMS,
		})
		if invalidateErr != nil {
			return false, false, nil, nil, invalidateErr
		}
		invalidatedSourceFileIDs = append(invalidatedSourceFileIDs, result.InvalidatedSourceFileIDs...)
		retryRootIDs = append(retryRootIDs, result.RetryRootIDs...)
		if len(result.InvalidatedSourceFileIDs) > 0 || len(result.RetryRootIDs) > 0 {
			needsShadowBuild = true
		}
	}

	if projectActive {
		after, projectErr := deps.ProjectCompaction(tx, source.UsageTargetActive, threadID)
		if projectErr != nil {
			return false, false, nil, nil, projectErr
		}
		visibleChanged = visibleChanged || !reflect.DeepEqual(before, after)
	}
	invalidatedSourceFileIDs = uniqueSortedInt64(invalidatedSourceFileIDs)
	retryRootIDs = uniqueSortedStrings(retryRootIDs)
	return visibleChanged, needsShadowBuild, invalidatedSourceFileIDs, retryRootIDs, nil
}

func activeSourceContributors(tx *source.WriteTx, activeEpoch int64, sourceIDs []int64) (map[int64]bool, error) {
	contributors := make(map[int64]bool, len(sourceIDs))
	if len(sourceIDs) == 0 {
		return contributors, nil
	}
	err := tx.Private(func(private storage.PrivateTx) error {
		for _, sourceID := range sourceIDs {
			var exists int
			err := private.QueryRow(`SELECT EXISTS(
				SELECT 1 FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=? AND source_file_id=?
				UNION ALL SELECT 1 FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=?
				UNION ALL SELECT 1 FROM codex_skill_usage_events WHERE ledger_epoch=? AND source_file_id=?
				UNION ALL SELECT 1 FROM codex_usage_session_quarantine_sources WHERE ledger_epoch=? AND source_file_id=?
			)`, activeEpoch, sourceID, activeEpoch, sourceID, activeEpoch, sourceID, activeEpoch, sourceID).Scan(&exists)
			if err != nil {
				return err
			}
			contributors[sourceID] = exists != 0
		}
		return nil
	})
	return contributors, err
}

func countCanonicalRootChanges(tx *source.WriteTx, epoch int64, threadID string, previousRoot *string) (int64, error) {
	if previousRoot == nil {
		return 0, nil
	}
	var count int64
	err := tx.Private(func(private storage.PrivateTx) error {
		return private.QueryRow(`SELECT COUNT(*) FROM usage_events WHERE source='codex' AND source_epoch=? AND thread_id=? AND root_session_id=?`, epoch, threadID, *previousRoot).Scan(&count)
	})
	return count, err
}

func rebindPrivateRoots(tx *source.WriteTx, epoch int64, threadID string, previousRoot *string, nextRoot string) (bool, error) {
	if previousRoot == nil {
		return false, nil
	}
	skillsChanged := false
	err := tx.Private(func(private storage.PrivateTx) error {
		for _, table := range []struct{ name, ownerColumn string }{
			{"codex_usage_source_states", "owning_thread_id"},
			{"codex_compaction_markers", "owning_thread_id"},
			{"codex_skill_usage_events", "thread_id"},
		} {
			query := fmt.Sprintf("UPDATE %s SET root_session_id=? WHERE ledger_epoch=? AND %s=? AND root_session_id=?", table.name, table.ownerColumn)
			parameters := []any{nextRoot, epoch, threadID, *previousRoot}
			result, err := private.Exec(query, parameters...)
			if err != nil {
				return err
			}
			if table.name == "codex_skill_usage_events" {
				rows, err := result.RowsAffected()
				if err != nil {
					return err
				}
				skillsChanged = rows > 0
			}
		}
		return nil
	})
	return skillsChanged, err
}

func deleteIntersectingActiveQuarantines(
	tx *source.WriteTx,
	epoch int64,
	previousRoot *string,
	sourceIDs []int64,
) (bool, []int64, []string, error) {
	deleted := false
	affectedSources := make([]int64, 0)
	roots := make(map[string]bool)
	changedIDs := make(map[int64]bool, len(sourceIDs))
	for _, sourceID := range sourceIDs {
		changedIDs[sourceID] = true
	}
	err := tx.Private(func(private storage.PrivateTx) error {
		proofsByRoot := make(map[string][]int64)
		if previousRoot != nil {
			var exists int
			if err := private.QueryRow(`SELECT EXISTS(SELECT 1 FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?)`, epoch, *previousRoot).Scan(&exists); err != nil {
				return err
			}
			if exists != 0 {
				roots[*previousRoot] = true
			}
		}
		rows, err := private.Query(`SELECT root_session_id,source_file_id FROM codex_usage_session_quarantine_sources WHERE ledger_epoch=?`, epoch)
		if err != nil {
			return err
		}
		for rows.Next() {
			var root string
			var sourceID int64
			if err := rows.Scan(&root, &sourceID); err != nil {
				rows.Close()
				return err
			}
			if root == stringValue(previousRoot) || changedIDs[sourceID] {
				roots[root] = true
			}
			proofsByRoot[root] = append(proofsByRoot[root], sourceID)
		}
		if err := rows.Err(); err != nil {
			rows.Close()
			return err
		}
		rows.Close()
		for root := range roots {
			affectedSources = append(affectedSources, proofsByRoot[root]...)
			if _, err := private.Exec(`DELETE FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?`, epoch, root); err != nil {
				return err
			}
			deleted = true
		}
		return nil
	})
	if err != nil {
		return false, nil, nil, err
	}
	rootIDs := make([]string, 0, len(roots))
	for root := range roots {
		rootIDs = append(rootIDs, root)
	}
	return deleted, uniqueSortedInt64(affectedSources), uniqueSortedStrings(rootIDs), nil
}

func normalizedSourceIDs(ids []int64) ([]int64, error) {
	result := append([]int64(nil), ids...)
	for _, id := range result {
		if id <= 0 {
			return nil, fmt.Errorf("binding source ID must be positive")
		}
	}
	return uniqueSortedInt64(result), nil
}

func uniqueSortedInt64(values []int64) []int64 {
	if len(values) == 0 {
		return nil
	}
	sort.Slice(values, func(i, j int) bool { return values[i] < values[j] })
	out := values[:0]
	for _, value := range values {
		if len(out) == 0 || out[len(out)-1] != value {
			out = append(out, value)
		}
	}
	return out
}

func uniqueSortedStrings(values []string) []string {
	if len(values) == 0 {
		return nil
	}
	sort.Strings(values)
	out := values[:0]
	for _, value := range values {
		if len(out) == 0 || out[len(out)-1] != value {
			out = append(out, value)
		}
	}
	return out
}

func sameOptionalString(left, right *string) bool {
	return (left == nil && right == nil) || (left != nil && right != nil && *left == *right)
}
