package codex

import (
	"bytes"
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/Hogeexxl/Usagi/internal/codex/rebuild"
	"github.com/Hogeexxl/Usagi/internal/codex/rollout"
	"github.com/Hogeexxl/Usagi/internal/codex/usage"
	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

const (
	ActiveRootMaxBytes      int64 = 4 * 1024 * 1024
	ActiveRootMaxLines      int64 = 4096
	ActiveRootMaxWriteUnits int64 = 2048
)

type sourceScanMode uint8

const (
	sourceActiveMode sourceScanMode = iota + 1
	sourceBuildMode
)

type rootScanDisposition uint8

const (
	rootNormalActive rootScanDisposition = iota + 1
	rootHoldQuarantine
	rootNormalBuild
	rootReuseQuarantine
)

type rootSourceRef struct {
	SourceFileID   int64
	Generation     int64
	OwningThreadID string
	RootSessionID  string
	FileStatus     rollout.SourceFileStatus
}

type scanSource struct {
	Observation rollout.SourceObservation
	Owner       string
	Root        string
	Fact        MetadataEvidence
	PriorFact   *MetadataEvidence
	Metadata    rollout.ReadResult
	HasMetadata bool
	MetaPlan    rollout.FilePlan
	UsagePlan   rollout.FilePlan
	Redundant   bool
	Winner      int64
}

type scanRecord struct {
	Parsed    usage.ParsedRecord
	Ownership rollout.Ownership
	Skill     []SkillEvent
}

type activeBudgetExceeded struct{}

func (activeBudgetExceeded) Error() string { return "active root provisional budget exceeded" }

func (a *Adapter) RunScan(ctx context.Context, run source.RunContext) error {
	if err := ctx.Err(); err != nil {
		return scanAdapterError("SCAN_CANCELLED", err)
	}
	if run.Source() != domain.SourceCodex {
		return scanAdapterError("SOURCE_MISMATCH", source.ErrSourceMismatch)
	}
	resolution := a.resolver.Resolve()
	if resolution.Err != nil {
		code := "CODEX_HOME_RESOLUTION_FAILED"
		var configErr *ConfigError
		if errors.As(resolution.Err, &configErr) && configErr.Code != "" {
			code = configErr.Code
		}
		return scanAdapterError(code, resolution.Err)
	}
	config := resolution.Config
	if err := validateScanConfig(config); err != nil {
		return scanAdapterError("CODEX_HOME_RESOLUTION_FAILED", err)
	}
	if run.Storage() == nil {
		return scanAdapterError("SOURCE_RUN_FAILED", errors.New("Codex run context has no storage"))
	}
	var binding BindingOutcome
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		binding, err = bindOrValidate(tx, config.HomeFingerprint)
		return err
	}); err != nil {
		return scanAdapterError("SOURCE_RUN_FAILED", err)
	}
	if binding == BindingSourceChanged {
		return scanAdapterError("SOURCE_CHANGED", errors.New("Codex Home binding changed"))
	}
	if err := runCodexScan(ctx, run, config, a.clock); err != nil {
		return err
	}
	return nil
}

func validateScanConfig(config Config) error {
	if config.Home == "" || config.HomeFingerprint == "" || config.Metadata.StateIndex == "" ||
		config.Metadata.SessionIndex == "" || config.Metadata.GlobalState == "" || !filepath.IsAbs(config.Home) {
		return errors.New("Codex config is incomplete")
	}
	fingerprint, err := hex.DecodeString(config.HomeFingerprint)
	if err != nil || len(fingerprint) != sha256.Size || strings.ToLower(config.HomeFingerprint) != config.HomeFingerprint {
		return errors.New("Codex Home fingerprint is invalid")
	}
	return nil
}

func scanAdapterError(code string, err error) error {
	return source.NewAdapterErrorWithCode(code, "Codex scan failed", err)
}

func scanContext(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return scanAdapterError("SCAN_CANCELLED", err)
	}
	return nil
}

func runCodexScan(ctx context.Context, run source.RunContext, config Config, clock func() int64) error {
	if err := scanContext(ctx); err != nil {
		return err
	}
	state := ReadStateSnapshot(config.Metadata.StateIndex)
	if err := scanContext(ctx); err != nil {
		return err
	}
	session := ReadSessionIndex(config.Metadata.SessionIndex)
	if err := scanContext(ctx); err != nil {
		return err
	}
	global := ReadGlobalState(config.Metadata.GlobalState)
	if err := scanContext(ctx); err != nil {
		return err
	}
	discovery := rollout.Discover(config.Home, 0)
	if err := scanContext(ctx); err != nil {
		return err
	}
	probe := rollout.ActiveCompactionVisibilityProbe(activeCompactionFingerprint)
	discovery.StartedAtMS = clock()
	observations, err := rollout.ReconcileSources(run.Storage(), discovery, MetadataParserVersion, usage.UsageParserVersion, probe)
	if err != nil {
		return classifyScanError(err, "SOURCE_RECONCILE_FAILED")
	}
	if err := scanContext(ctx); err != nil {
		return err
	}
	observations, err = rollout.PreflightConsumerGuardQuorum(run.Storage(), observations, clock(), MetadataParserVersion, usage.UsageParserVersion, probe)
	if err != nil {
		return classifyScanError(err, "SOURCE_RECONCILE_FAILED")
	}
	if err := scanContext(ctx); err != nil {
		return err
	}

	firstHard := scanSideSourceError(state, session, discovery)
	files, err := prepareScanSources(ctx, run.Storage(), observations, discovery, state, config.Home, probe, clock)
	if err != nil {
		return classifyScanError(err, "METADATA_SCAN_FAILED")
	}
	if err := scanContext(ctx); err != nil {
		return err
	}
	threadView, err := loadThreadView(run.Storage())
	if err != nil {
		return classifyScanError(err, "METADATA_SCAN_FAILED")
	}
	rolloutFacts, err := loadAllMetadataFacts(run.Storage())
	if err != nil {
		return classifyScanError(err, "METADATA_SCAN_FAILED")
	}
	redundantIDs := make(map[int64]bool)
	for _, item := range files {
		if item.Redundant {
			redundantIDs[item.Observation.SourceFileID] = true
		}
	}
	keptFacts := rolloutFacts[:0]
	for _, fact := range rolloutFacts {
		if !redundantIDs[fact.SourceFileID] {
			keptFacts = append(keptFacts, fact)
		}
	}
	rolloutFacts = keptFacts
	metadataView := ResolveThreadView(MetadataResolveInput{
		State: state, Session: session, Global: global, RolloutFacts: rolloutFacts,
		Sources: observations, Existing: threadView, ResolvedAtMS: discovery.StartedAtMS,
	})
	resolverRoots := resolvedThreadRoots(threadView, metadataView)
	metaOutcome, err := commitMetadataView(ctx, run.Storage(), files, metadataView, resolverRoots, probe, clock)
	if err != nil {
		return classifyScanError(err, "METADATA_COMMIT_FAILED")
	}
	if err := scanContext(ctx); err != nil {
		return err
	}

	threads, err := loadThreadView(run.Storage())
	if err != nil {
		return classifyScanError(err, "METADATA_SCAN_FAILED")
	}
	rootByOwner := resolvedThreadRoots(threads, metadataView)
	refs, roots, err := freezeRootSources(run.Storage(), observations, rootByOwner)
	if err != nil {
		return classifyScanError(err, "USAGE_PLAN_FAILED")
	}
	for index := range files {
		if ref, ok := roots[files[index].Observation.SourceFileID]; ok {
			files[index].Owner = ref.OwningThreadID
			files[index].Root = ref.RootSessionID
		}
	}
	mode, trigger, invalidated, retryRoots, err := prePassBuildNeed(run.Storage(), files, refs, metaOutcome)
	if err != nil {
		return classifyScanError(err, "USAGE_PLAN_FAILED")
	}
	coordinator, err := rebuild.NewCoordinator(
		VisiblePrivateEqual, ActiveSourceStateProofV3, usage.StripDeletedEventReferencesFromWindows,
	)
	if err != nil {
		return classifyScanError(err, "USAGE_PLAN_FAILED")
	}
	if mode == sourceBuildMode {
		if err := beginScanBuild(ctx, run.Storage(), coordinator, trigger, refs, roots, invalidated, retryRoots, clock); err != nil {
			return classifyScanError(err, "USAGE_BUILD_FAILED")
		}
	}
	files, err = planUsageFiles(run.Storage(), files, mode)
	if err != nil {
		return classifyScanError(err, "USAGE_PLAN_FAILED")
	}
	if err := runUsagePass(ctx, run.Storage(), files, refs, roots, mode, trigger, coordinator, clock); err != nil {
		return err
	}
	if firstHard != nil {
		return firstHard
	}
	return nil
}

func commitMetadataView(
	ctx context.Context,
	bound *source.Storage,
	files []scanSource,
	view MetadataResolveResult,
	rootsByOwner map[string]string,
	probe rollout.ActiveCompactionVisibilityProbe,
	clock func() int64,
) (MetadataCommitOutcome, error) {
	groups := make(map[string]*MetadataThreadCommit)
	for index := range view.Patches {
		patch := view.Patches[index]
		groups[patch.ThreadID] = &MetadataThreadCommit{Patch: &patch}
	}
	for _, item := range files {
		if !item.HasMetadata || item.Redundant || item.Owner == "" || item.Fact.OwnershipConfidence != "confirmed" ||
			(item.Observation.BoundThreadID != nil && *item.Observation.BoundThreadID == item.Owner) {
			continue
		}
		checkpointOffset, guard, found, err := loadMetadataCheckpoint(bound, item.Observation.SourceFileID)
		if err != nil {
			return MetadataCommitOutcome{}, err
		}
		if !found {
			return MetadataCommitOutcome{}, fmt.Errorf("metadata fact for source %d has no checkpoint", item.Observation.SourceFileID)
		}
		if item.Metadata.FixedViewExhausted {
			checkpointOffset = item.Metadata.PhysicalCommittedOffset
			if item.Observation.Compressed {
				checkpointOffset = item.Observation.DiscoveryObservedSize
			}
			guard = item.Metadata.GuardHash
		}
		group := groups[item.Owner]
		if group == nil {
			group = &MetadataThreadCommit{}
			groups[item.Owner] = group
		}
		group.Sources = append(group.Sources, MetadataSourceCommit{
			Observation:           item.Observation,
			SafeFact:              item.Fact,
			PlainCheckpointOffset: checkpointOffset,
			GuardHash:             append([]byte(nil), guard...),
			CheckpointStatus:      "ready",
		})
	}
	threadIDs := make([]string, 0, len(groups))
	for id := range groups {
		threadIDs = append(threadIDs, id)
	}
	sort.Strings(threadIDs)
	outcome := MetadataCommitOutcome{}
	for _, id := range threadIDs {
		if err := scanContext(ctx); err != nil {
			return outcome, err
		}
		bindingDeps := usage.BindingReconcileDeps{
			ProjectCompaction: func(tx *source.WriteTx, target source.UsageWriteTarget, owner string) (usage.CompactionVisibilityProjection, error) {
				return MetadataCompactionVisibilityProjection(tx, target, owner)
			},
			InvalidateBuild: func(tx *source.WriteTx, request usage.BuildBindingInvalidationRequest) (usage.BuildBindingInvalidationResult, error) {
				state, err := tx.UsageEpochState()
				if err != nil {
					return usage.BuildBindingInvalidationResult{}, err
				}
				if state.BuildEpoch == nil {
					return usage.BuildBindingInvalidationResult{}, nil
				}
				requirements, err := currentBuildRequirementsTx(tx, files, rootsByOwner)
				if err != nil {
					return usage.BuildBindingInvalidationResult{}, err
				}
				roots := make([]string, 0, 2)
				if request.PreviousRoot != nil {
					roots = append(roots, *request.PreviousRoot)
				}
				if request.NextRoot != nil {
					roots = append(roots, *request.NextRoot)
				}
				if err := rebuild.ResetBuildMembersTx(tx, *state.BuildEpoch, rebuild.SourceInvalidated,
					request.BindingChangedSourceIDs, roots, requirements, ActiveSourceStateProofV3,
					usage.StripDeletedEventReferencesFromWindows, request.CommittedAtMS); err != nil {
					return usage.BuildBindingInvalidationResult{}, err
				}
			return usage.BuildBindingInvalidationResult{
					InvalidatedSourceFileIDs: append([]int64(nil), request.BindingChangedSourceIDs...),
					RetryRootIDs:             roots,
				}, nil
			},
		}
		deps := MetadataCommitDeps{
			ProjectActiveCompaction: probe,
			ReconcileUsageBinding: func(tx *source.WriteTx, threadID string, previousRoot, nextRoot *string, sourceIDs []int64, committedAtMS int64) (bool, bool, []int64, []string, error) {
				return usage.ReconcileMetadataUsageBinding(tx, bindingDeps, threadID, previousRoot, nextRoot, sourceIDs, committedAtMS)
			},
		}
		if err := scanContext(ctx); err != nil {
			return outcome, err
		}
		committed, err := CommitMetadata(bound, MetadataCommitBatch{Threads: []MetadataThreadCommit{*groups[id]}}, deps, clock())
		if err != nil {
			return outcome, err
		}
		outcome.CommittedThreads += committed.CommittedThreads
		outcome.VisibleChanged = outcome.VisibleChanged || committed.VisibleChanged
		outcome.NeedsShadowBuild = outcome.NeedsShadowBuild || committed.NeedsShadowBuild
		outcome.InvalidatedSourceFileIDs = append(outcome.InvalidatedSourceFileIDs, committed.InvalidatedSourceFileIDs...)
		outcome.RetryRootIDs = append(outcome.RetryRootIDs, committed.RetryRootIDs...)
	}
	return outcome, nil
}

func loadMetadataCheckpoint(bound *source.Storage, sourceID int64) (int64, []byte, bool, error) {
	var offset sql.NullInt64
	var guard []byte
	var found bool
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		err := reader.QueryRow(`SELECT committed_offset,guard_hash FROM codex_source_checkpoints WHERE source_file_id=? AND consumer_kind='metadata'`, sourceID).Scan(&offset, &guard)
		if errors.Is(err, sql.ErrNoRows) {
			return nil
		}
		if err != nil {
			return err
		}
		found = offset.Valid
		return nil
	})
	if err != nil || !found {
		return 0, guard, found, err
	}
	return offset.Int64, guard, true, nil
}

func prePassBuildNeed(
	bound *source.Storage,
	files []scanSource,
	refs map[string][]rootSourceRef,
	metadata MetadataCommitOutcome,
) (sourceScanMode, rebuild.Trigger, []int64, []string, error) {
	epoch, found, err := bound.LoadUsageEpoch()
	if err != nil {
		return 0, 0, nil, nil, err
	}
	if !found || epoch.ActiveEpoch == 0 {
		return sourceBuildMode, rebuild.Bootstrap, nil, nil, nil
	}
	if epoch.ActiveParserVersion != usage.UsageParserVersion || epoch.BuildParserVersion != nil && *epoch.BuildParserVersion != usage.UsageParserVersion {
		return sourceBuildMode, rebuild.ParserChanged, nil, nil, nil
	}
	invalidated := append([]int64(nil), metadata.InvalidatedSourceFileIDs...)
	filesByID := make(map[int64]scanSource, len(files))
	for _, file := range files {
		filesByID[file.Observation.SourceFileID] = file
	}
	if err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		rows, err := reader.Query(`SELECT contributor.source_file_id,
			EXISTS(SELECT 1 FROM codex_usage_event_occurrences o WHERE o.source='codex' AND o.ledger_epoch=? AND o.source_file_id=contributor.source_file_id),
			EXISTS(SELECT 1 FROM codex_usage_source_states s WHERE s.ledger_epoch=? AND s.source_file_id=contributor.source_file_id),
			EXISTS(SELECT 1 FROM codex_skill_usage_events k WHERE k.ledger_epoch=? AND k.source_file_id=contributor.source_file_id),
			EXISTS(SELECT 1 FROM codex_usage_session_quarantine_sources q WHERE q.ledger_epoch=? AND q.source_file_id=contributor.source_file_id)
		FROM (
			SELECT source_file_id FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?
			UNION SELECT source_file_id FROM codex_usage_source_states WHERE ledger_epoch=?
			UNION SELECT source_file_id FROM codex_skill_usage_events WHERE ledger_epoch=?
			UNION SELECT source_file_id FROM codex_usage_session_quarantine_sources WHERE ledger_epoch=?
		) contributor ORDER BY contributor.source_file_id`,
			epoch.ActiveEpoch, epoch.ActiveEpoch, epoch.ActiveEpoch, epoch.ActiveEpoch,
			epoch.ActiveEpoch, epoch.ActiveEpoch, epoch.ActiveEpoch, epoch.ActiveEpoch)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var sourceID int64
			var hasOccurrences, hasState, hasSkills, hasQuarantine bool
			if err := rows.Scan(&sourceID, &hasOccurrences, &hasState, &hasSkills, &hasQuarantine); err != nil {
				return err
			}
			file, present := filesByID[sourceID]
			if !present {
				invalidated = append(invalidated, sourceID)
				continue
			}
			observation := file.Observation
			compatible, err := acceptedObservationMatches(reader, observation)
			if err != nil {
				return err
			}
			if !compatible {
				invalidated = append(invalidated, sourceID)
				continue
			}
			var generation, device, inode, observed, parser, resolved int64
			var owner, root sql.NullString
			stateErr := reader.QueryRow(`SELECT file_generation,device_id,inode,observed_raw_size,
				usage_parser_version,resolved_through_offset,owning_thread_id,root_session_id
				FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=?`, epoch.ActiveEpoch, sourceID).
				Scan(&generation, &device, &inode, &observed, &parser, &resolved, &owner, &root)
			if stateErr != nil && !errors.Is(stateErr, sql.ErrNoRows) {
				return stateErr
			}
			if errors.Is(stateErr, sql.ErrNoRows) {
				if hasOccurrences || hasSkills {
					invalidated = append(invalidated, sourceID)
					continue
				}
				if !hasQuarantine && !file.Redundant && usageCheckpointReady(reader, sourceID, epoch.ActiveParserVersion) {
					invalidated = append(invalidated, sourceID)
				}
				continue
			}
			physicalChanged := generation != observation.Generation || device != observation.Identity.DeviceID || inode != observation.Identity.Inode ||
				parser != epoch.ActiveParserVersion || observed > observation.DiscoveryObservedSize
			if observation.Compressed {
				physicalChanged = physicalChanged || observed != observation.DiscoveryObservedSize
			}
			if file.Owner != "" && owner.Valid && owner.String != file.Owner || file.Root != "" && root.Valid && root.String != file.Root {
				physicalChanged = true
			}
			var checkpointParser, checkpointOffset int64
			var checkpointStatus string
			checkpointErr := reader.QueryRow(`SELECT parser_version,committed_offset,processing_status FROM codex_source_checkpoints
				WHERE source_file_id=? AND consumer_kind='usage'`, sourceID).Scan(&checkpointParser, &checkpointOffset, &checkpointStatus)
			if checkpointErr != nil && !errors.Is(checkpointErr, sql.ErrNoRows) {
				return checkpointErr
			}
			if errors.Is(checkpointErr, sql.ErrNoRows) || checkpointStatus != "ready" || checkpointParser != epoch.ActiveParserVersion || checkpointOffset != resolved {
				physicalChanged = true
			}
			if physicalChanged || file.Redundant {
				invalidated = append(invalidated, sourceID)
			}
		}
		return rows.Err()
	}); err != nil {
		return 0, 0, nil, nil, err
	}
	if err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		rows, err := reader.Query(`SELECT source_file_id FROM codex_source_checkpoints
			WHERE consumer_kind='usage' AND processing_status='rebuild_required' ORDER BY source_file_id`)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var sourceID int64
			if err := rows.Scan(&sourceID); err != nil {
				return err
			}
			if _, present := filesByID[sourceID]; present {
				invalidated = append(invalidated, sourceID)
			}
		}
		return rows.Err()
	}); err != nil {
		return 0, 0, nil, nil, err
	}
	invalidated = uniqueInt64(invalidated)
	retryRoots := append([]string(nil), metadata.RetryRootIDs...)
	quarantineRoots, err := activeQuarantineRoots(bound, epoch.ActiveEpoch)
	if err != nil {
		return 0, 0, nil, nil, err
	}
	rootSet := make(map[string]struct{}, len(refs)+len(quarantineRoots))
	for root := range refs {
		rootSet[root] = struct{}{}
	}
	for _, root := range quarantineRoots {
		rootSet[root] = struct{}{}
	}
	for root := range rootSet {
		valid, err := activeQuarantineMatches(bound, epoch.ActiveEpoch, root, refs[root])
		if err != nil {
			return 0, 0, nil, nil, err
		}
		if valid < 0 {
			continue
		}
		if valid == 0 {
			retryRoots = append(retryRoots, root)
			for _, ref := range refs[root] {
				invalidated = append(invalidated, ref.SourceFileID)
			}
			quarantinedSources, err := activeQuarantineSources(bound, epoch.ActiveEpoch, root)
			if err != nil {
				return 0, 0, nil, nil, err
			}
			invalidated = append(invalidated, quarantinedSources...)
		}
	}
	retryRoots = uniqueStrings(retryRoots)
	if epoch.BuildEpoch != nil {
		if len(retryRoots) > 0 {
			return sourceBuildMode, rebuild.QuarantineRetry, uniqueInt64(invalidated), retryRoots, nil
		}
		return sourceBuildMode, rebuild.SourceInvalidated, uniqueInt64(invalidated), nil, nil
	}
	if len(retryRoots) > 0 {
		return sourceBuildMode, rebuild.QuarantineRetry, uniqueInt64(invalidated), retryRoots, nil
	}
	if metadata.NeedsShadowBuild || len(invalidated) > 0 {
		return sourceBuildMode, rebuild.SourceInvalidated, uniqueInt64(invalidated), nil, nil
	}
	return sourceActiveMode, 0, nil, nil, nil
}

func usageCheckpointReady(reader storage.PrivateReader, sourceID, parserVersion int64) bool {
	var parser int64
	var status string
	err := reader.QueryRow(`SELECT parser_version,processing_status FROM codex_source_checkpoints
		WHERE source_file_id=? AND consumer_kind='usage'`, sourceID).Scan(&parser, &status)
	return err == nil && parser == parserVersion && status == "ready"
}

func activeQuarantineRoots(bound *source.Storage, epoch int64) ([]string, error) {
	var roots []string
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		rows, err := reader.Query(`SELECT root_session_id FROM codex_usage_session_quarantine
			WHERE ledger_epoch=? ORDER BY root_session_id`, epoch)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var root string
			if err := rows.Scan(&root); err != nil {
				return err
			}
			roots = append(roots, root)
		}
		return rows.Err()
	})
	return roots, err
}

func activeQuarantineSources(bound *source.Storage, epoch int64, root string) ([]int64, error) {
	var ids []int64
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		rows, err := reader.Query(`SELECT source_file_id FROM codex_usage_session_quarantine_sources
			WHERE ledger_epoch=? AND root_session_id=? ORDER BY source_file_id`, epoch, root)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var id int64
			if err := rows.Scan(&id); err != nil {
				return err
			}
			ids = append(ids, id)
		}
		return rows.Err()
	})
	return ids, err
}

// activeQuarantineMatches returns -1 when no quarantine exists, 0 for a stale proof,
// and 1 when the current complete root proof matches the active quarantine.
func activeQuarantineMatches(bound *source.Storage, epoch int64, root string, refs []rootSourceRef) (int, error) {
	proofs := make([]usage.QuarantineSourceProof, 0, len(refs))
	match := false
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		var exists int
		if err := reader.QueryRow(`SELECT EXISTS(SELECT 1 FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?)`, epoch, root).Scan(&exists); err != nil {
			return err
		}
		if exists == 0 {
			return nil
		}
		for _, ref := range refs {
			var proof usage.QuarantineSourceProof
			if err := reader.QueryRow(`SELECT source_file_id,file_generation,device_id,inode,observed_size
				FROM codex_source_files WHERE source_file_id=? AND file_status='present'`, ref.SourceFileID).
				Scan(&proof.SourceFileID, &proof.Generation, &proof.DeviceID, &proof.Inode, &proof.ObservedSize); err != nil {
				if errors.Is(err, sql.ErrNoRows) {
					return rollout.ErrSourceChanged
				}
				return err
			}
			proofs = append(proofs, proof)
		}
		match, err = QuarantineStillValid(epoch, root, proofs, reader)
		return err
	})
	if err != nil {
		return 0, err
	}
	if match {
		return 1, nil
	}
	exists := false
	if err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		var count int
		if err := reader.QueryRow(`SELECT EXISTS(SELECT 1 FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?)`, epoch, root).Scan(&count); err != nil {
			return err
		}
		exists = count != 0
		return nil
	}); err != nil {
		return 0, err
	}
	if !exists {
		return -1, nil
	}
	return 0, nil
}

func acceptedObservationMatches(reader storage.PrivateReader, observation rollout.SourceObservation) (bool, error) {
	var generation, device, inode, size, mtime int64
	var status string
	err := reader.QueryRow(`SELECT file_generation,device_id,inode,observed_size,observed_mtime_ns,file_status FROM codex_source_files WHERE source_file_id=?`, observation.SourceFileID).
		Scan(&generation, &device, &inode, &size, &mtime, &status)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	if generation != observation.Generation || device != observation.Identity.DeviceID || inode != observation.Identity.Inode ||
		status != string(rollout.SourceFilePresent) || size > observation.DiscoveryObservedSize ||
		size == observation.DiscoveryObservedSize && mtime != observation.DiscoveryObservedMTimeNS ||
		observation.Compressed && (size != observation.DiscoveryObservedSize || mtime != observation.DiscoveryObservedMTimeNS) {
		return false, nil
	}
	return true, nil
}

func beginScanBuild(
	ctx context.Context,
	bound *source.Storage,
	coordinator *rebuild.Coordinator,
	trigger rebuild.Trigger,
	refs map[string][]rootSourceRef,
	byID map[int64]rootSourceRef,
	files []scanSource,
	invalidated []int64,
	retryRoots []string,
	clock func() int64,
) error {
	requirements, err := requirementsForFiles(bound, files)
	if err != nil {
		return err
	}
	if err := scanContext(ctx); err != nil {
		return err
	}
	committedAtMS := clock()
	return bound.Write(func(tx *source.WriteTx) error {
		if err := scanContext(ctx); err != nil {
			return err
		}
		state, err := tx.UsageEpochState()
		if err != nil {
			return err
		}
		if state.BuildEpoch != nil && state.BuildParserVersion != nil && *state.BuildParserVersion != usage.UsageParserVersion {
			trigger = rebuild.ParserChanged
		}
		buildEpoch, err := coordinator.BeginOrResume(tx, trigger, usage.UsageParserVersion, requirements, invalidated, committedAtMS)
		if err != nil {
			return err
		}
		_ = buildEpoch
		roots := append([]string(nil), retryRoots...)
		for _, id := range invalidated {
			if ref, ok := byID[id]; ok {
				roots = append(roots, ref.RootSessionID)
			}
		}
		roots = uniqueStrings(roots)
		if state.BuildEpoch != nil && len(roots) > 0 && trigger != rebuild.ParserChanged {
			state, err = tx.UsageEpochState()
			if err != nil {
				return err
			}
			if state.BuildEpoch == nil {
				return fmt.Errorf("build barrier disappeared")
				}
			return rebuild.ResetBuildMembersTx(tx, *state.BuildEpoch, trigger, invalidated, roots, requirements,
				ActiveSourceStateProofV3, usage.StripDeletedEventReferencesFromWindows, committedAtMS)
		}
		_ = refs
		return nil
	})
}

func requirementsForFiles(bound *source.Storage, files []scanSource) ([]rebuild.MemberRequirement, error) {
	requirements := make([]rebuild.MemberRequirement, 0, len(files))
	for _, item := range files {
		observation := item.Observation
		required := item.Metadata.LogicalCommittedOffset
		if item.Observation.Compressed {
			required = observation.DiscoveryObservedSize
		} else if !item.Metadata.FixedViewExhausted {
			checkpointOffset, _, found, err := loadMetadataCheckpoint(bound, observation.SourceFileID)
			if err != nil {
				return nil, err
			}
			if found {
				required = checkpointOffset
			}
		}
		if required > observation.DiscoveryObservedSize {
			return nil, fmt.Errorf("metadata safe offset exceeds source size for %d", observation.SourceFileID)
		}
		var owner, root *string
		if item.Owner != "" {
			value := item.Owner
			owner = &value
		}
		if item.Root != "" {
			value := item.Root
			root = &value
		}
		requirements = append(requirements, rebuild.MemberRequirement{
			SourceFileID: observation.SourceFileID, Generation: observation.Generation,
			DeviceID: observation.Identity.DeviceID, Inode: observation.Identity.Inode,
			RequiredThroughOffset: required, ObservedRawSize: observation.DiscoveryObservedSize,
			ExpectedOwningThreadID: owner, ExpectedRootSessionID: root,
		})
	}
	sort.Slice(requirements, func(i, j int) bool { return requirements[i].SourceFileID < requirements[j].SourceFileID })
	return requirements, nil
}

func currentBuildRequirementsTx(tx *source.WriteTx, files []scanSource, rootsByOwner map[string]string) ([]rebuild.MemberRequirement, error) {
	result := make([]rebuild.MemberRequirement, 0, len(files))
	err := tx.Private(func(private storage.PrivateTx) error {
		for _, item := range files {
			observation := item.Observation
			var owner sql.NullString
			var generation, device, inode, size, mtime int64
			var status string
			if err := private.QueryRow(`SELECT thread_id,file_generation,device_id,inode,observed_size,observed_mtime_ns,file_status
				FROM codex_source_files WHERE source_file_id=?`, observation.SourceFileID).
				Scan(&owner, &generation, &device, &inode, &size, &mtime, &status); err != nil {
				return err
			}
			if generation != observation.Generation || device != observation.Identity.DeviceID || inode != observation.Identity.Inode ||
				size != observation.DiscoveryObservedSize || mtime != observation.DiscoveryObservedMTimeNS || status != string(rollout.SourceFilePresent) {
				return rollout.ErrSourceChanged
			}
			var required int64
			if observation.Compressed {
				required = observation.DiscoveryObservedSize
			} else if err := private.QueryRow(`SELECT committed_offset FROM codex_source_checkpoints WHERE source_file_id=? AND consumer_kind='metadata'`, observation.SourceFileID).Scan(&required); err != nil && !errors.Is(err, sql.ErrNoRows) {
				return err
			}
			if required > observation.DiscoveryObservedSize {
				return fmt.Errorf("metadata checkpoint exceeds source size")
			}
			var expectedOwner, expectedRoot *string
			if owner.Valid {
				value := owner.String
				expectedOwner = &value
				if root := rootsByOwner[value]; root != "" {
					rootCopy := root
					expectedRoot = &rootCopy
				} else {
					var dbRoot sql.NullString
					if err := private.QueryRow(`SELECT root_session_id FROM threads WHERE thread_id=?`, value).Scan(&dbRoot); err != nil && !errors.Is(err, sql.ErrNoRows) {
						return err
					}
					if dbRoot.Valid {
						rootCopy := dbRoot.String
						expectedRoot = &rootCopy
					}
				}
			}
			result = append(result, rebuild.MemberRequirement{
				SourceFileID: observation.SourceFileID, Generation: generation, DeviceID: device, Inode: inode,
				RequiredThroughOffset: required, ObservedRawSize: size,
				ExpectedOwningThreadID: expectedOwner, ExpectedRootSessionID: expectedRoot,
			})
		}
		return nil
	})
	return result, err
}

func classifyScanError(err error, fallback string) error {
	if err == nil {
		return nil
	}
	if errors.Is(err, rollout.ErrSourceChanged) || errors.Is(err, rollout.ErrGuardMismatch) ||
		errors.Is(err, rollout.ErrIncompleteZstdInput) || errors.Is(err, rollout.ErrSourceSymlink) {
		return scanAdapterError("SOURCE_CHANGED_DURING_SCAN", err)
	}
	var adapterErr *source.AdapterError
	if errors.As(err, &adapterErr) {
		return err
	}
	return scanAdapterError(fallback, err)
}

func activeCompactionFingerprint(tx *source.WriteTx) ([]byte, error) {
	projection, err := ActiveCompactionVisibilityProjection(tx)
	if err != nil {
		return nil, err
	}
	return json.Marshal(projection)
}

func scanSideSourceError(state StateSnapshot, session SessionNameSnapshot, discovery rollout.DiscoverySnapshot) error {
	if state.Status != StateSourceComplete {
		return scanAdapterError("STATE_SOURCE_UNAVAILABLE", errors.New("state_5 snapshot unavailable"))
	}
	if session.Status == SessionSourceUnavailable {
		return scanAdapterError("SESSION_INDEX_UNAVAILABLE", errors.New("session index unavailable"))
	}
	if discovery.Sessions != rollout.RegionComplete || discovery.Archived != rollout.RegionComplete {
		return scanAdapterError("SOURCE_AREA_UNAVAILABLE", errors.New("one or more rollout areas are unavailable"))
	}
	return nil
}

func prepareScanSources(
	ctx context.Context,
	bound *source.Storage,
	observations []rollout.SourceObservation,
	discovery rollout.DiscoverySnapshot,
	state StateSnapshot,
	home string,
	probe rollout.ActiveCompactionVisibilityProbe,
	clock func() int64,
) ([]scanSource, error) {
	filesByPath := make(map[string]rollout.DiscoveredFile, len(discovery.Files))
	for _, file := range discovery.Files {
		filesByPath[file.Path] = file
	}
	verifiedWinners, err := verifyRedundantPairs(discovery.Files, observations)
	if err != nil {
		return nil, err
	}
	result := make([]scanSource, 0, len(observations))
	for _, observation := range observations {
		file, ok := filesByPath[observation.CurrentPath]
		if !ok {
			continue
		}
		candidates := owningCandidates(file, state, home)
		fact, found, err := loadMetadataFact(bound, observation.SourceFileID)
		if err != nil {
			return nil, err
		}
		prior := (*MetadataEvidence)(nil)
		if found && fact.FileGeneration == observation.Generation && fact.MetadataParserVersion == MetadataParserVersion {
			prior = &fact
		}
		plan, err := rollout.PlanFile(bound, observation, rollout.ConsumerMetadata, MetadataParserVersion, false)
		if err != nil {
			return nil, err
		}
		item := scanSource{Observation: observation, MetaPlan: plan, Fact: fact}
		item.PriorFact = prior
		if winner, redundant := verifiedWinners[observation.CurrentPath]; redundant {
			item.Redundant = true
			item.Winner = winner
		}
		if plan.Kind == rollout.PlanSkip && !item.Redundant && prior != nil {
			item.Fact = *prior
			item.Owner = prior.OwningThreadID
			item.HasMetadata = prior.OwnershipConfidence == "confirmed"
			item.Metadata = rollout.ReadResult{PhysicalCommittedOffset: plan.PhysicalStartOffset,
				LogicalCommittedOffset: prior.ResolvedThroughOffset, GuardHash: append([]byte(nil), plan.GuardHash...)}
			result = append(result, item)
			continue
		}
		if plan.Kind == rollout.PlanSkip && !item.Redundant {
			plan.Kind = rollout.PlanRebuild
			plan.PhysicalStartOffset = 0
			plan.GuardHash = nil
		}
		readPlan := rollout.ReadPlan{
			SourceFileID: observation.SourceFileID, Generation: observation.Generation,
			Path: observation.CurrentPath, Identity: observation.Identity, Compressed: observation.Compressed,
			PhysicalStartOffset: plan.PhysicalStartOffset, PhysicalObservedSize: observation.DiscoveryObservedSize,
			PhysicalObservedMTimeNS: observation.DiscoveryObservedMTimeNS, ExpectedGuard: plan.GuardHash,
		}
		if item.Redundant || plan.Kind == rollout.PlanRebuild {
			readPlan.PhysicalStartOffset = 0
			readPlan.ExpectedGuard = nil
		}
		owner := ""
		if prior != nil && !item.Redundant {
			owner = prior.OwningThreadID
		}
		if owner == "" && !item.Redundant && candidates.StateRolloutPath != nil {
			owner = candidates.StateRolloutPath.ThreadID
		}
		if owner == "" && !item.Redundant && candidates.Filename != nil {
			owner = candidates.Filename.ThreadID
		}
		acc := (*MetadataAccumulator)(nil)
		classState := rollout.OwnershipState{}
		classBoundary := rollout.OwnershipBoundary{}
		if owner != "" && !item.Redundant {
			acc = NewMetadataAccumulator(observation.SourceFileID, observation.Generation, MetadataParserVersion, 0, owner, candidates, prior)
		}
		read, err := rollout.Read(readPlan, func(record rollout.Record) error {
			if item.Redundant {
				return nil
			}
			if acc == nil {
				classification := rollout.ClassifyRecord(record, candidates, classState, classBoundary)
				classState, classBoundary = classification.NextState, classification.Boundary
				if classification.Ownership.Kind == rollout.OwnershipOwning && classification.Ownership.ThreadID != "" {
					acc = NewMetadataAccumulator(observation.SourceFileID, observation.Generation, MetadataParserVersion, 0,
						classification.Ownership.ThreadID, candidates, nil)
					acc.ObserveRecord(record)
				}
				return nil
			}
			classification := acc.ObserveRecord(record)
			classState, classBoundary = classification.NextState, classification.Boundary
			return nil
		}, func(gap rollout.Gap) error {
			if item.Redundant {
				return nil
			}
			if acc == nil {
				if rollout.ClassifyGap(gap, classState, classBoundary).Kind == rollout.OwnershipUnknown {
					classState = rollout.OwnershipState{}
					classBoundary.Confidence = rollout.OwnershipConfidenceUnresolved
				}
				return nil
			}
			acc.ObserveGap(gap)
			return nil
		})
		if err != nil {
			return nil, err
		}
		if !read.FixedViewExhausted {
			return nil, fmt.Errorf("metadata reader did not exhaust source %d", observation.SourceFileID)
		}
		if !item.Redundant && acc != nil {
			if err := scanContext(ctx); err != nil {
				return nil, err
			}
			committedAtMS := clock()
			item.Metadata = read
			item.Fact = acc.Snapshot(read.LogicalCommittedOffset, committedAtMS)
			item.Owner = item.Fact.OwningThreadID
			item.HasMetadata = item.Owner != "" && item.Fact.OwnershipConfidence == "confirmed"
			if item.HasMetadata {
				if err := commitPass1MetadataFile(ctx, bound, item, read, probe, committedAtMS); err != nil {
					return nil, err
				}
			}
		} else if item.Redundant {
			item.Metadata = read
			if err := scanContext(ctx); err != nil {
				return nil, err
			}
			if err := commitPass1RedundantFile(ctx, bound, observation, read, probe, clock()); err != nil {
				return nil, err
			}
		} else if prior != nil {
			item.Fact = *prior
			item.Owner = prior.OwningThreadID
			item.HasMetadata = true
		}
		item.Metadata = read
		result = append(result, item)
	}
	return result, nil
}

func commitPass1MetadataFile(
	ctx context.Context,
	bound *source.Storage,
	item scanSource,
	read rollout.ReadResult,
	probe rollout.ActiveCompactionVisibilityProbe,
	committedAtMS int64,
) error {
	if err := scanContext(ctx); err != nil {
		return err
	}
	return bound.Write(func(tx *source.WriteTx) error {
		if err := scanContext(ctx); err != nil {
			return err
		}
		before, err := probe(tx)
		if err != nil {
			return err
		}
		if !item.Observation.Compressed {
			if err := rollout.AcceptAppendProof(tx, item.Observation); err != nil {
				return err
			}
		}
		if err := writeMetadataFact(tx, item.Fact); err != nil {
			return err
		}
		checkpointOffset := read.PhysicalCommittedOffset
		if item.Observation.Compressed {
			checkpointOffset = item.Observation.DiscoveryObservedSize
		}
		if err := writeMetadataCheckpoint(tx, item.Observation.SourceFileID, checkpointOffset, read.GuardHash, committedAtMS); err != nil {
			return err
		}
		after, err := probe(tx)
		if err != nil {
			return err
		}
		if !bytes.Equal(before, after) {
			_, err = tx.BumpDataRevision()
		}
		return err
	})
}

func commitPass1RedundantFile(
	ctx context.Context,
	bound *source.Storage,
	observation rollout.SourceObservation,
	read rollout.ReadResult,
	probe rollout.ActiveCompactionVisibilityProbe,
	committedAtMS int64,
) error {
	if err := scanContext(ctx); err != nil {
		return err
	}
	return bound.Write(func(tx *source.WriteTx) error {
		if err := scanContext(ctx); err != nil {
			return err
		}
		before, err := probe(tx)
		if err != nil {
			return err
		}
		if err := writeMetadataCheckpoint(tx, observation.SourceFileID, observation.DiscoveryObservedSize, read.GuardHash, committedAtMS); err != nil {
			return err
		}
		after, err := probe(tx)
		if err != nil {
			return err
		}
		if !bytes.Equal(before, after) {
			_, err = tx.BumpDataRevision()
		}
		return err
	})
}

func writeMetadataCheckpoint(tx *source.WriteTx, sourceFileID, offset int64, guard []byte, committedAtMS int64) error {
	return privateExec(tx, `INSERT INTO codex_source_checkpoints(source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,processing_status,last_successful_scan_at_ms,last_error_code)
		VALUES(?, 'metadata', ?, ?, ?, 'ready', ?, NULL)
		ON CONFLICT(source_file_id,consumer_kind) DO UPDATE SET parser_version=excluded.parser_version,committed_offset=excluded.committed_offset,
		guard_hash=excluded.guard_hash,processing_status='ready',last_successful_scan_at_ms=excluded.last_successful_scan_at_ms,last_error_code=NULL`,
		sourceFileID, MetadataParserVersion, offset, guard, committedAtMS)
}

func owningCandidates(file rollout.DiscoveredFile, state StateSnapshot, home string) rollout.OwningThreadCandidates {
	var candidates rollout.OwningThreadCandidates
	for _, fact := range state.Threads {
		if fact.RolloutPath == nil {
			continue
		}
		path := *fact.RolloutPath
		if !filepath.IsAbs(path) {
			path = filepath.Join(home, path)
		}
		path, _ = filepath.Abs(path)
		if filepath.Clean(path) == file.Path {
			candidates.StateRolloutPath = &rollout.OwningThreadCandidate{ThreadID: fact.ThreadID, Confidence: rollout.CandidateConfidenceConfirmed}
			break
		}
	}
	if file.ThreadIDCandidate != "" {
		candidates.Filename = &rollout.OwningThreadCandidate{ThreadID: file.ThreadIDCandidate, Confidence: rollout.CandidateConfidenceCandidate}
	}
	return candidates
}

func verifyRedundantPairs(files []rollout.DiscoveredFile, observations []rollout.SourceObservation) (map[string]int64, error) {
	winners := make(map[string]int64)
	idsByPath := make(map[string]int64, len(observations))
	plainByPath := make(map[string]rollout.DiscoveredFile, len(files))
	for _, observation := range observations {
		idsByPath[observation.CurrentPath] = observation.SourceFileID
	}
	for _, plain := range files {
		if !plain.Compressed && strings.HasSuffix(plain.Path, ".jsonl") {
			plainByPath[filepath.Clean(plain.Path)] = plain
		}
	}
	for _, compressed := range files {
		if !compressed.Compressed || !strings.HasSuffix(compressed.Path, ".jsonl.zst") {
			continue
		}
		plainPath := filepath.Clean(strings.TrimSuffix(compressed.Path, ".zst"))
		plain, ok := plainByPath[plainPath]
		if !ok || plain.Area != compressed.Area {
			continue
		}
		plainID, plainPresent := idsByPath[filepath.Clean(plain.Path)]
		compressedID, compressedPresent := idsByPath[filepath.Clean(compressed.Path)]
		if !plainPresent || !compressedPresent || plainID <= 0 || compressedID <= 0 {
			continue
		}
		_, err := rollout.VerifyRedundantTwin(plain, compressed)
		if errors.Is(err, rollout.ErrTwinMismatch) {
			continue
		}
		if err != nil {
			return nil, err
		}
		winners[filepath.Clean(compressed.Path)] = plainID
	}
	return winners, nil
}

func loadMetadataFact(bound *source.Storage, sourceID int64) (MetadataEvidence, bool, error) {
	var fact MetadataEvidence
	var found bool
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		var err error
		fact, found, err = LoadMetadataEvidence(reader, sourceID)
		return err
	})
	return fact, found, err
}

func loadAllMetadataFacts(bound *source.Storage) ([]MetadataEvidence, error) {
	var ids []int64
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		rows, err := reader.Query("SELECT source_file_id FROM codex_rollout_metadata_facts ORDER BY source_file_id")
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var id int64
			if err := rows.Scan(&id); err != nil {
				return err
			}
			ids = append(ids, id)
		}
		return rows.Err()
	})
	if err != nil {
		return nil, err
	}
	facts := make([]MetadataEvidence, 0, len(ids))
	for _, id := range ids {
		fact, found, err := loadMetadataFact(bound, id)
		if err != nil {
			return nil, err
		}
		if found {
			facts = append(facts, fact)
		}
	}
	return facts, nil
}

func loadThreadView(bound *source.Storage) ([]domain.Thread, error) {
	var threads []domain.Thread
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		rows, err := reader.Query(`SELECT thread_id,source,native_session_id,parent_thread_id,root_session_id,agent_role,title,
			project_name,project_path,project_kind,metadata_model,created_at_ms,updated_at_ms,archived,
			metadata_quality_status,metadata_resolved_at_ms FROM threads WHERE source=? ORDER BY thread_id`, domain.SourceCodex)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var thread domain.Thread
			var parent, root, title, projectName, projectPath, model sql.NullString
			var created, updated sql.NullInt64
			var archived int64
			if err := rows.Scan(&thread.ThreadID, &thread.Source, &thread.NativeSessionID, &parent, &root, &thread.AgentRole,
				&title, &projectName, &projectPath, &thread.ProjectKind, &model, &created, &updated, &archived,
				&thread.MetadataQualityStatus, &thread.MetadataResolvedAtMS); err != nil {
				return err
			}
			thread.ParentThreadID, thread.RootSessionID = nullableText(parent), nullableText(root)
			thread.Title, thread.ProjectName, thread.ProjectPath, thread.MetadataModel = nullableText(title), nullableText(projectName), nullableText(projectPath), nullableText(model)
			thread.CreatedAtMS, thread.UpdatedAtMS = nullableNumber(created), nullableNumber(updated)
			thread.Archived = archived != 0
			threads = append(threads, thread)
		}
		return rows.Err()
	})
	return threads, err
}

func nullableText(value sql.NullString) *string {
	if !value.Valid {
		return nil
	}
	result := value.String
	return &result
}

func nullableNumber(value sql.NullInt64) *int64 {
	if !value.Valid {
		return nil
	}
	result := value.Int64
	return &result
}

func resolvedThreadRoots(threads []domain.Thread, resolved MetadataResolveResult) map[string]string {
	unresolved := make(map[string]bool)
	for _, diagnostic := range resolved.Diagnostics {
		if diagnostic.Code == "root_unresolved" {
			unresolved[diagnostic.ThreadID] = true
		}
	}
	patches := make(map[string]domain.ResolvedThreadPatch, len(resolved.Patches))
	for _, patch := range resolved.Patches {
		patches[patch.ThreadID] = patch
	}
	roots := make(map[string]string, len(threads))
	for _, thread := range threads {
		if unresolved[thread.ThreadID] {
			continue
		}
		root := thread.RootSessionID
		if patch, ok := patches[thread.ThreadID]; ok {
			switch patch.RootSessionID.Kind() {
			case domain.PatchSet:
				value, _ := patch.RootSessionID.Value()
				root = &value
			case domain.PatchClear:
				root = nil
			}
		}
		if root != nil {
			roots[thread.ThreadID] = *root
		}
	}
	return roots
}

func freezeRootSources(bound *source.Storage, observations []rollout.SourceObservation, rootsByOwner map[string]string) (map[string][]rootSourceRef, map[int64]rootSourceRef, error) {
	refs := make(map[string][]rootSourceRef)
	byID := make(map[int64]rootSourceRef)
	for _, observation := range observations {
		var owner sql.NullString
		var status string
		err := bound.PrivateRead(func(reader storage.PrivateReader) error {
			return reader.QueryRow(`SELECT thread_id,file_status FROM codex_source_files WHERE source_file_id=?`, observation.SourceFileID).
				Scan(&owner, &status)
		})
		if err != nil {
			return nil, nil, err
		}
		if !owner.Valid {
			continue
		}
		root, ok := rootsByOwner[owner.String]
		if !ok || root == "" || status != string(rollout.SourceFilePresent) {
			continue
		}
		ref := rootSourceRef{SourceFileID: observation.SourceFileID, Generation: observation.Generation,
			OwningThreadID: owner.String, RootSessionID: root, FileStatus: rollout.SourceFilePresent}
		refs[root] = append(refs[root], ref)
		byID[ref.SourceFileID] = ref
	}
	for root := range refs {
		sort.Slice(refs[root], func(i, j int) bool { return refs[root][i].SourceFileID < refs[root][j].SourceFileID })
	}
	return refs, byID, nil
}
