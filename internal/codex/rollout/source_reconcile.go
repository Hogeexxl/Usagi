package rollout

import (
	"bytes"
	"database/sql"
	"errors"
	"fmt"
	"math"
	"path/filepath"
	"sort"
	"strings"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

const (
	ConsumerMetadata = "metadata"
	ConsumerUsage    = "usage"
)

var (
	ErrInvalidCatalog        = errors.New("invalid Codex source catalog")
	ErrGuardMismatch         = errors.New("rollout checkpoint guard mismatch")
	ErrVisibilityProbeNeeded = errors.New("active compaction visibility probe is required")
)

type catalogSource struct {
	id         int64
	threadID   sql.NullString
	path       string
	area       string
	identity   PhysicalIdentity
	generation int64
	size       int64
	mtimeNS    int64
	status     SourceFileStatus
	lastSeenMS int64
}

type plannedFile struct {
	file        DiscoveredFile
	existing    *catalogSource
	replacement bool
	newFile     bool
	invalidate  bool
}

type catalogPlan struct {
	files               []plannedFile
	missing             []catalogSource
	stage               []catalogSource
	historical          map[int64]bool
	visibilitySensitive bool
}

type sourceCheckpoint struct {
	consumer      string
	parserVersion int64
	offset        int64
	guard         []byte
	guardPresent  bool
	status        string
}

type sourceProofQuery interface {
	QueryRow(query string, args ...any) *sql.Row
}

func ReconcileSources(
	bound *source.Storage,
	snapshot DiscoverySnapshot,
	metadataParserVersion int64,
	usageParserVersion int64,
	probe ActiveCompactionVisibilityProbe,
) ([]SourceObservation, error) {
	if snapshot.StartedAtMS < 0 {
		return nil, fmt.Errorf("%w: negative scan time", ErrInvalidCatalog)
	}
	if metadataParserVersion < 0 || usageParserVersion < 0 {
		return nil, fmt.Errorf("%w: negative parser version", ErrInvalidCatalog)
	}
	existing, err := readCatalog(bound)
	if err != nil {
		return nil, err
	}
	plan, err := planCatalog(existing, snapshot)
	if err != nil {
		return nil, err
	}
	if plan.visibilitySensitive && probe == nil {
		return nil, ErrVisibilityProbeNeeded
	}
	observations := make([]SourceObservation, 0, len(plan.files))
	err = bound.Write(func(tx *source.WriteTx) error {
		var before []byte
		if plan.visibilitySensitive {
			var err error
			before, err = probe(tx)
			if err != nil {
				return err
			}
		}
		if err := applyCatalogPlan(tx, plan, snapshot.StartedAtMS, metadataParserVersion, usageParserVersion, &observations); err != nil {
			return err
		}
		if plan.visibilitySensitive {
			var err error
			var after []byte
			after, err = probe(tx)
			if err != nil {
				return err
			}
			if !bytes.Equal(before, after) {
				if _, err := tx.BumpDataRevision(); err != nil {
					return err
				}
			}
		}
		var staging int64
		if err := tx.Private(func(privateTx storage.PrivateTx) error {
			return privateTx.QueryRow("SELECT COUNT(*) FROM codex_source_files WHERE current_path LIKE '@reconcile/%'").Scan(&staging)
		}); err != nil {
			return err
		}
		if staging != 0 {
			return fmt.Errorf("%w: transaction staging path survived reconciliation", ErrInvalidCatalog)
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Slice(observations, func(i, j int) bool { return observations[i].SourceFileID < observations[j].SourceFileID })
	return observations, nil
}

func readCatalog(bound *source.Storage) ([]catalogSource, error) {
	var sources []catalogSource
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		rows, err := reader.Query(
			`SELECT source_file_id,thread_id,current_path,source_area,device_id,inode,file_generation,
			        observed_size,observed_mtime_ns,file_status,last_seen_at_ms
			 FROM codex_source_files ORDER BY source_file_id`,
		)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var row catalogSource
			var status string
			if err := rows.Scan(
				&row.id, &row.threadID, &row.path, &row.area, &row.identity.DeviceID, &row.identity.Inode,
				&row.generation, &row.size, &row.mtimeNS, &status, &row.lastSeenMS,
			); err != nil {
				return err
			}
			row.status = SourceFileStatus(status)
			sources = append(sources, row)
		}
		return rows.Err()
	})
	return sources, err
}

func planCatalog(existing []catalogSource, snapshot DiscoverySnapshot) (catalogPlan, error) {
	plan := catalogPlan{historical: make(map[int64]bool)}
	if (snapshot.Sessions != RegionComplete && snapshot.Sessions != RegionUnavailable) ||
		(snapshot.Archived != RegionComplete && snapshot.Archived != RegionUnavailable) {
		return catalogPlan{}, fmt.Errorf("%w: invalid discovery region state", ErrInvalidCatalog)
	}
	byIdentity := make(map[PhysicalIdentity]*catalogSource, len(existing))
	byPath := make(map[string]*catalogSource, len(existing))
	for index := range existing {
		row := &existing[index]
		if row.id <= 0 || row.generation <= 0 || row.identity.DeviceID < 0 || row.identity.Inode < 0 || row.size < 0 || row.mtimeNS < 0 || row.lastSeenMS < 0 {
			return catalogPlan{}, fmt.Errorf("%w: invalid source row %d", ErrInvalidCatalog, row.id)
		}
		if (row.area != "sessions" && row.area != "archived_sessions") ||
			(row.status != SourceFilePresent && row.status != SourceFileMissing && row.status != SourceFileReplaced) {
			return catalogPlan{}, fmt.Errorf("%w: invalid source row state %d", ErrInvalidCatalog, row.id)
		}
		if strings.HasPrefix(row.path, "@reconcile/") {
			return catalogPlan{}, fmt.Errorf("%w: stale transaction staging path", ErrInvalidCatalog)
		}
		if _, ok := byPath[row.path]; ok {
			return catalogPlan{}, fmt.Errorf("%w: duplicate catalog path %q", ErrInvalidCatalog, row.path)
		}
		byPath[row.path] = row
		if owner, ok := byIdentity[row.identity]; ok && owner.id != row.id {
			return catalogPlan{}, fmt.Errorf("%w: physical identity has multiple owners", ErrInvalidCatalog)
		}
		byIdentity[row.identity] = row
	}

	files := append([]DiscoveredFile(nil), snapshot.Files...)
	sort.Slice(files, func(i, j int) bool { return discoveredLess(files[i], files[j]) })
	seenPaths := make(map[string]struct{}, len(files))
	seenIdentities := make(map[PhysicalIdentity]struct{}, len(files))
	for index := range files {
		file := &files[index]
		if file.Size < 0 || file.MTimeNS < 0 || file.Identity.DeviceID < 0 || file.Identity.Inode < 0 ||
			(file.Area != AreaSessions && file.Area != AreaArchived) {
			return catalogPlan{}, fmt.Errorf("%w: negative discovery proof", ErrInvalidCatalog)
		}
		if strings.HasPrefix(filepath.ToSlash(file.Path), "@reconcile/") || strings.HasPrefix(filepath.ToSlash(file.Path), "@historical/") {
			return catalogPlan{}, fmt.Errorf("%w: reserved discovery path", ErrInvalidCatalog)
		}
		path, err := filepath.Abs(file.Path)
		if err != nil {
			return catalogPlan{}, fmt.Errorf("%w: invalid discovery path", ErrInvalidCatalog)
		}
		file.Path = filepath.Clean(path)
		if strings.HasPrefix(file.Path, "@reconcile/") || strings.HasPrefix(file.Path, "@historical/") {
			return catalogPlan{}, fmt.Errorf("%w: reserved discovery path", ErrInvalidCatalog)
		}
		if _, ok := seenPaths[file.Path]; ok {
			return catalogPlan{}, fmt.Errorf("%w: duplicate discovery path %q", ErrInvalidCatalog, file.Path)
		}
		if _, ok := seenIdentities[file.Identity]; ok {
			return catalogPlan{}, fmt.Errorf("%w: discovery contains duplicate physical identity", ErrInvalidCatalog)
		}
		seenPaths[file.Path] = struct{}{}
		seenIdentities[file.Identity] = struct{}{}
	}

	selected := make(map[int64]bool)
	targetOwner := make(map[string]int64)
	for _, file := range files {
		entry := plannedFile{file: file}
		if owner := byIdentity[file.Identity]; owner != nil {
			entry.existing = owner
			entry.invalidate = generationDecision(*owner, file)
			selected[owner.id] = true
			targetOwner[file.Path] = owner.id
			if owner.path != file.Path || owner.area != areaName(file.Area) || owner.status != SourceFilePresent || entry.invalidate {
				plan.visibilitySensitive = true
			}
			if owner.path != file.Path {
				plan.stage = append(plan.stage, *owner)
				plan.visibilitySensitive = true
			}
		} else if occupant := byPath[file.Path]; occupant != nil && !identityDiscovered(occupant.identity, seenIdentities) {
			entry.existing = occupant
			entry.replacement = true
			entry.invalidate = true
			selected[occupant.id] = true
			targetOwner[file.Path] = occupant.id
			plan.visibilitySensitive = true
		} else {
			entry.newFile = true
			plan.visibilitySensitive = true
		}
		plan.files = append(plan.files, entry)
	}

	for _, row := range existing {
		_, selectedThisRound := selected[row.id]
		displaced := false
		if target, ok := targetOwner[row.path]; ok && target != row.id && !selectedThisRound {
			plan.historical[row.id] = true
			displaced = true
		}
		if displaced || row.status == SourceFilePresent && !selectedThisRound && regionState(snapshot, row.area) == RegionComplete {
			plan.missing = append(plan.missing, row)
			plan.visibilitySensitive = true
		}
	}
	for i := range plan.stage {
		if plan.stage[i].path == "" {
			return catalogPlan{}, fmt.Errorf("%w: empty staging source path", ErrInvalidCatalog)
		}
	}
	sort.Slice(plan.stage, func(i, j int) bool { return plan.stage[i].id < plan.stage[j].id })
	sort.Slice(plan.missing, func(i, j int) bool { return plan.missing[i].id < plan.missing[j].id })
	return plan, nil
}

func identityDiscovered(identity PhysicalIdentity, seen map[PhysicalIdentity]struct{}) bool {
	_, ok := seen[identity]
	return ok
}

func generationDecision(old catalogSource, next DiscoveredFile) bool {
	oldCompressed := strings.HasSuffix(old.path, ".jsonl.zst")
	if oldCompressed != next.Compressed {
		return true
	}
	if next.Compressed {
		return old.size != next.Size || old.mtimeNS != next.MTimeNS
	}
	if next.Size < old.size || next.Size == old.size && next.MTimeNS != old.mtimeNS {
		return true
	}
	return false
}

func applyCatalogPlan(
	tx *source.WriteTx,
	plan catalogPlan,
	nowMS int64,
	metadataParserVersion, usageParserVersion int64,
	observations *[]SourceObservation,
) error {
	return tx.Private(func(privateTx storage.PrivateTx) error {
		for _, row := range plan.stage {
			stagingPath := fmt.Sprintf("@reconcile/%d/%d", row.id, row.generation)
			result, err := privateTx.Exec(
				`UPDATE codex_source_files SET current_path=?1
				 WHERE source_file_id=?2 AND current_path=?3 AND file_generation=?4 AND device_id=?5 AND inode=?6`,
				stagingPath, row.id, row.path, row.generation, row.identity.DeviceID, row.identity.Inode,
			)
			if err != nil {
				return err
			}
			if err := requireOneCatalogRow(result); err != nil {
				return err
			}
		}
		for _, row := range plan.missing {
			if plan.historical[row.id] {
				historicalPath := fmt.Sprintf("@historical/%d/%d", row.id, row.generation)
				result, err := privateTx.Exec(
					`UPDATE codex_source_files SET current_path=?1,file_status='missing'
					 WHERE source_file_id=?2 AND current_path=?3 AND file_generation=?4 AND file_status=?5`,
					historicalPath, row.id, row.path, row.generation, string(row.status),
				)
				if err != nil {
					return err
				}
				if err := requireOneCatalogRow(result); err != nil {
					return err
				}
			} else {
				result, err := privateTx.Exec(
					`UPDATE codex_source_files SET file_status='missing'
					 WHERE source_file_id=?1 AND current_path=?2 AND file_generation=?3 AND file_status='present'`,
					row.id, row.path, row.generation,
				)
				if err != nil {
					return err
				}
				if err := requireOneCatalogRow(result); err != nil {
					return err
				}
			}
		}

		applyFile := func(entry plannedFile) error {
			var sourceID, generation int64
			var acceptedSize, acceptedMTime int64
			var identity PhysicalIdentity
			var boundThreadID *string
			switch {
			case entry.newFile:
				result, err := privateTx.Exec(
					`INSERT INTO codex_source_files (
						thread_id,current_path,source_area,device_id,inode,file_generation,
						observed_size,observed_mtime_ns,file_status,last_seen_at_ms
					) VALUES (NULL,?1,?2,?3,?4,1,?5,?6,'present',?7)`,
					entry.file.Path, areaName(entry.file.Area), entry.file.Identity.DeviceID,
					entry.file.Identity.Inode, entry.file.Size, entry.file.MTimeNS, nowMS,
				)
				if err != nil {
					return err
				}
				sourceID, err = result.LastInsertId()
				if err != nil {
					return err
				}
				generation, acceptedSize, acceptedMTime, identity = 1, entry.file.Size, entry.file.MTimeNS, entry.file.Identity
			case entry.replacement || entry.invalidate:
				old := entry.existing
				if old == nil {
					return fmt.Errorf("%w: invalidation has no catalog owner", ErrInvalidCatalog)
				}
				expected := acceptedProof(*old)
				nextGeneration, err := InvalidateGeneration(tx, expected, entry.file, nowMS, metadataParserVersion, usageParserVersion)
				if err != nil {
					return err
				}
				sourceID, generation = old.id, nextGeneration
				acceptedSize, acceptedMTime, identity = entry.file.Size, entry.file.MTimeNS, entry.file.Identity
				if err := updateCatalogLocation(privateTx, sourceID, old.generation+1, identity, acceptedSize, acceptedMTime, entry.file.Path, entry.file.Area, nowMS); err != nil {
					return err
				}
			case entry.existing != nil:
				old := entry.existing
				sourceID, generation, identity = old.id, old.generation, old.identity
				acceptedSize, acceptedMTime = old.size, old.mtimeNS
				if err := updateCatalogLocation(privateTx, sourceID, generation, identity, acceptedSize, acceptedMTime, entry.file.Path, entry.file.Area, nowMS); err != nil {
					return err
				}
				boundThreadID = nullableString(old.threadID)
			default:
				return fmt.Errorf("%w: incomplete plan item", ErrInvalidCatalog)
			}
			*observations = append(*observations, SourceObservation{
				SourceFileID: sourceID, Generation: generation, CurrentPath: entry.file.Path,
				Area: entry.file.Area, Identity: identity, Compressed: entry.file.Compressed,
				BoundThreadID: boundThreadID, AcceptedObservedSize: acceptedSize,
				AcceptedObservedMTimeNS: acceptedMTime,
				DiscoveryObservedSize:   entry.file.Size, DiscoveryObservedMTimeNS: entry.file.MTimeNS,
			})
			return nil
		}
		identityOwners := make([]plannedFile, 0, len(plan.files))
		newIdentityFiles := make([]plannedFile, 0, len(plan.files))
		for _, entry := range plan.files {
			if entry.existing != nil && !entry.replacement {
				identityOwners = append(identityOwners, entry)
			} else {
				newIdentityFiles = append(newIdentityFiles, entry)
			}
		}
		sort.Slice(identityOwners, func(i, j int) bool {
			return identityOwners[i].existing.id < identityOwners[j].existing.id
		})
		sort.Slice(newIdentityFiles, func(i, j int) bool {
			if newIdentityFiles[i].file.Path != newIdentityFiles[j].file.Path {
				return newIdentityFiles[i].file.Path < newIdentityFiles[j].file.Path
			}
			if newIdentityFiles[i].file.Identity.DeviceID != newIdentityFiles[j].file.Identity.DeviceID {
				return newIdentityFiles[i].file.Identity.DeviceID < newIdentityFiles[j].file.Identity.DeviceID
			}
			return newIdentityFiles[i].file.Identity.Inode < newIdentityFiles[j].file.Identity.Inode
		})
		for _, entry := range identityOwners {
			if err := applyFile(entry); err != nil {
				return err
			}
		}
		for _, entry := range newIdentityFiles {
			if err := applyFile(entry); err != nil {
				return err
			}
		}
		return nil
	})
}

func updateCatalogLocation(
	privateTx storage.PrivateTx,
	sourceID, generation int64,
	identity PhysicalIdentity,
	size, mtimeNS int64,
	path string,
	area Area,
	nowMS int64,
) error {
	result, err := privateTx.Exec(
		`UPDATE codex_source_files SET current_path=?1,source_area=?2,file_status='present',last_seen_at_ms=?3
		 WHERE source_file_id=?4 AND file_generation=?5 AND device_id=?6 AND inode=?7
		   AND observed_size=?8 AND observed_mtime_ns=?9`,
		path, areaName(area), nowMS, sourceID, generation, identity.DeviceID, identity.Inode, size, mtimeNS,
	)
	if err != nil {
		return err
	}
	return requireOneCatalogRow(result)
}

func InvalidateGeneration(
	tx *source.WriteTx,
	expected AcceptedSourceProof,
	next DiscoveredFile,
	nowMS int64,
	metadataParserVersion, usageParserVersion int64,
) (int64, error) {
	if expected.SourceFileID <= 0 || expected.Generation <= 0 || expected.ObservedSize < 0 || expected.ObservedMTimeNS < 0 || nowMS < 0 ||
		next.Size < 0 || next.MTimeNS < 0 || next.Identity.DeviceID < 0 || next.Identity.Inode < 0 || expected.FileStatus == "" ||
		metadataParserVersion < 0 || usageParserVersion < 0 {
		return 0, fmt.Errorf("%w: invalid generation proof", ErrInvalidCatalog)
	}
	if expected.Generation == math.MaxInt64 {
		return 0, fmt.Errorf("%w: file generation overflow", ErrInvalidCatalog)
	}
	newGeneration := expected.Generation + 1
	err := tx.Private(func(privateTx storage.PrivateTx) error {
		durableProof, err := durableAcceptedSourceProof(privateTx, expected.SourceFileID)
		if err != nil {
			return err
		}
		if durableProof != expected {
			return fmt.Errorf("%w: durable accepted source proof changed", ErrInvalidCatalog)
		}
		result, err := privateTx.Exec(
			`UPDATE codex_source_files SET file_generation=?1,device_id=?2,inode=?3,observed_size=?4,
			 observed_mtime_ns=?5,file_status='present',thread_id=NULL,last_seen_at_ms=?6
			 WHERE source_file_id=?7 AND file_generation=?8 AND device_id=?9 AND inode=?10
			   AND observed_size=?11 AND observed_mtime_ns=?12 AND file_status=?13`,
			newGeneration, next.Identity.DeviceID, next.Identity.Inode, next.Size, next.MTimeNS, nowMS,
			expected.SourceFileID, expected.Generation, expected.Identity.DeviceID, expected.Identity.Inode,
			expected.ObservedSize, expected.ObservedMTimeNS, string(expected.FileStatus),
		)
		if err != nil {
			return err
		}
		if err := requireOneCatalogRow(result); err != nil {
			return err
		}
		if _, err := privateTx.Exec("DELETE FROM codex_rollout_metadata_facts WHERE source_file_id=?", expected.SourceFileID); err != nil {
			return err
		}
		_, err = privateTx.Exec(
			`UPDATE codex_source_checkpoints SET
			 parser_version=CASE consumer_kind WHEN 'metadata' THEN ?1 WHEN 'usage' THEN ?2 END,
			 committed_offset=0,guard_hash=NULL,processing_status='rebuild_required',
			 last_successful_scan_at_ms=NULL,last_error_code=NULL
			 WHERE source_file_id=?3`,
			metadataParserVersion, usageParserVersion, expected.SourceFileID,
		)
		return err
	})
	if err != nil {
		return 0, err
	}
	return newGeneration, nil
}

func durableAcceptedSourceProof(reader sourceProofQuery, sourceFileID int64) (AcceptedSourceProof, error) {
	var proof AcceptedSourceProof
	var status string
	if err := reader.QueryRow(
		`SELECT source_file_id,file_generation,device_id,inode,observed_size,observed_mtime_ns,file_status
		 FROM codex_source_files WHERE source_file_id=?1`,
		sourceFileID,
	).Scan(
		&proof.SourceFileID, &proof.Generation, &proof.Identity.DeviceID, &proof.Identity.Inode,
		&proof.ObservedSize, &proof.ObservedMTimeNS, &status,
	); err != nil {
		return AcceptedSourceProof{}, err
	}
	proof.FileStatus = SourceFileStatus(status)
	return proof, nil
}

func PreflightConsumerGuardQuorum(
	bound *source.Storage,
	observations []SourceObservation,
	nowMS int64,
	metadataParserVersion, usageParserVersion int64,
	probe ActiveCompactionVisibilityProbe,
) ([]SourceObservation, error) {
	if nowMS < 0 || metadataParserVersion < 0 || usageParserVersion < 0 {
		return nil, fmt.Errorf("%w: invalid preflight parameters", ErrInvalidCatalog)
	}
	if len(observations) == 0 {
		return observations, nil
	}
	byID := make(map[int64]int, len(observations))
	for index, observation := range observations {
		if observation.SourceFileID <= 0 || observation.Generation <= 0 {
			return nil, fmt.Errorf("%w: invalid source observation", ErrInvalidCatalog)
		}
		if _, duplicate := byID[observation.SourceFileID]; duplicate {
			return nil, fmt.Errorf("%w: duplicate source observation", ErrInvalidCatalog)
		}
		byID[observation.SourceFileID] = index
	}
	checkpoints := make(map[int64][]sourceCheckpoint, len(observations))
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		rows, err := reader.Query(
			`SELECT source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,guard_hash IS NOT NULL,processing_status
			 FROM codex_source_checkpoints ORDER BY source_file_id,consumer_kind`,
		)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var id int64
			var checkpoint sourceCheckpoint
			if err := rows.Scan(&id, &checkpoint.consumer, &checkpoint.parserVersion, &checkpoint.offset, &checkpoint.guard, &checkpoint.guardPresent, &checkpoint.status); err != nil {
				return err
			}
			if _, wanted := byID[id]; wanted {
				checkpoints[id] = append(checkpoints[id], checkpoint)
			}
		}
		return rows.Err()
	})
	if err != nil {
		return nil, err
	}
	type invalidation struct {
		index int
		file  DiscoveredFile
	}
	invalid := make([]invalidation, 0)
	for id, index := range byID {
		observation := observations[index]
		if observation.Compressed {
			continue
		}
		for _, checkpoint := range checkpoints[id] {
			if checkpoint.consumer != ConsumerMetadata && checkpoint.consumer != ConsumerUsage {
				return nil, fmt.Errorf("%w: unknown consumer %q", ErrInvalidCatalog, checkpoint.consumer)
			}
			if checkpoint.status != "ready" || checkpoint.offset <= 0 || !checkpoint.guardPresent {
				continue
			}
			if checkpoint.offset > observation.DiscoveryObservedSize || len(checkpoint.guard) != 32 {
				invalid = append(invalid, invalidation{index: index, file: discoveredFromObservation(observation)})
				break
			}
			err := verifyGuard(GuardPlan{
				Path: observation.CurrentPath, Identity: observation.Identity,
				PhysicalStartOffset: checkpoint.offset, PhysicalObservedSize: observation.DiscoveryObservedSize,
				PhysicalObservedMTimeNS: observation.DiscoveryObservedMTimeNS,
				ExpectedGuard:           checkpoint.guard,
			})
			if errors.Is(err, ErrGuardMismatch) {
				invalid = append(invalid, invalidation{index: index, file: discoveredFromObservation(observation)})
				break
			}
			if err != nil {
				return nil, err
			}
		}
	}
	if len(invalid) == 0 {
		return observations, nil
	}
	if probe == nil {
		return nil, ErrVisibilityProbeNeeded
	}
	updated := append([]SourceObservation(nil), observations...)
	sort.Slice(invalid, func(i, j int) bool {
		return observations[invalid[i].index].SourceFileID < observations[invalid[j].index].SourceFileID
	})
	err = bound.Write(func(tx *source.WriteTx) error {
		before, err := probe(tx)
		if err != nil {
			return err
		}
		for _, item := range invalid {
			observation := updated[item.index]
			var proof AcceptedSourceProof
			if err := tx.Private(func(privateTx storage.PrivateTx) error {
				var err error
				proof, err = durableAcceptedSourceProof(privateTx, observation.SourceFileID)
				return err
			}); err != nil {
				return err
			}
			if proof.Generation != observation.Generation || proof.Identity != observation.Identity || proof.FileStatus != SourceFilePresent ||
				proof.ObservedSize > observation.DiscoveryObservedSize ||
				proof.ObservedSize == observation.DiscoveryObservedSize && proof.ObservedMTimeNS != observation.DiscoveryObservedMTimeNS {
				return fmt.Errorf("%w: source observation no longer matches durable identity", ErrInvalidCatalog)
			}
			generation, err := InvalidateGeneration(tx, proof, item.file, nowMS, metadataParserVersion, usageParserVersion)
			if err != nil {
				return err
			}
			observation.Generation = generation
			observation.AcceptedObservedSize = item.file.Size
			observation.AcceptedObservedMTimeNS = item.file.MTimeNS
			observation.BoundThreadID = nil
			updated[item.index] = observation
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
	if err != nil {
		return nil, err
	}
	return updated, nil
}

func AcceptAppendProof(tx *source.WriteTx, observation SourceObservation) error {
	if observation.Compressed {
		return fmt.Errorf("%w: compressed source proof cannot be accepted incrementally", ErrInvalidCatalog)
	}
	if observation.SourceFileID <= 0 || observation.Generation <= 0 || observation.Identity.DeviceID < 0 ||
		observation.Identity.Inode < 0 || observation.DiscoveryObservedSize < 0 || observation.DiscoveryObservedMTimeNS < 0 {
		return fmt.Errorf("%w: invalid append observation", ErrInvalidCatalog)
	}
	return tx.Private(func(privateTx storage.PrivateTx) error {
		proof, err := durableAcceptedSourceProof(privateTx, observation.SourceFileID)
		if err != nil {
			return err
		}
		if proof.Generation != observation.Generation || proof.Identity != observation.Identity || proof.FileStatus != SourceFilePresent {
			return fmt.Errorf("%w: source observation no longer matches durable identity", ErrSourceChanged)
		}
		if proof.ObservedSize == observation.DiscoveryObservedSize && proof.ObservedMTimeNS == observation.DiscoveryObservedMTimeNS {
			return nil
		}
		if proof.ObservedSize >= observation.DiscoveryObservedSize {
			return fmt.Errorf("%w: non-append proof requires generation invalidation", ErrSourceChanged)
		}
		result, err := privateTx.Exec(
			`UPDATE codex_source_files SET observed_size=?1,observed_mtime_ns=?2
			 WHERE source_file_id=?3 AND file_generation=?4 AND device_id=?5 AND inode=?6
			   AND observed_size=?7 AND observed_mtime_ns=?8 AND file_status='present'`,
			observation.DiscoveryObservedSize, observation.DiscoveryObservedMTimeNS,
			observation.SourceFileID, observation.Generation, observation.Identity.DeviceID, observation.Identity.Inode,
			proof.ObservedSize, proof.ObservedMTimeNS,
		)
		if err != nil {
			return err
		}
		return requireOneCatalogRow(result)
	})
}

func PlanFile(
	bound *source.Storage,
	observation SourceObservation,
	consumer string,
	parserVersion int64,
	verifiedRedundant bool,
) (FilePlan, error) {
	if consumer != ConsumerMetadata && consumer != ConsumerUsage || observation.SourceFileID <= 0 || observation.Generation <= 0 ||
		parserVersion < 0 || observation.Identity.DeviceID < 0 || observation.Identity.Inode < 0 ||
		observation.CurrentPath == "" || observation.DiscoveryObservedSize < 0 || observation.DiscoveryObservedMTimeNS < 0 {
		return FilePlan{}, fmt.Errorf("%w: invalid file plan input for consumer %q", ErrInvalidCatalog, consumer)
	}
	var checkpoint sourceCheckpoint
	var checkpointErr error
	var proof AcceptedSourceProof
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		var err error
		proof, err = durableAcceptedSourceProof(reader, observation.SourceFileID)
		if err != nil {
			return err
		}
		checkpointErr = reader.QueryRow(
			`SELECT consumer_kind,parser_version,committed_offset,guard_hash,guard_hash IS NOT NULL,processing_status
			 FROM codex_source_checkpoints WHERE source_file_id=?1 AND consumer_kind=?2`,
			observation.SourceFileID, consumer,
		).Scan(&checkpoint.consumer, &checkpoint.parserVersion, &checkpoint.offset, &checkpoint.guard, &checkpoint.guardPresent, &checkpoint.status)
		if errors.Is(checkpointErr, sql.ErrNoRows) {
			return nil
		}
		return checkpointErr
	})
	if err != nil {
		return FilePlan{}, err
	}
	if proof.Generation != observation.Generation || proof.Identity != observation.Identity || proof.FileStatus != SourceFilePresent ||
		proof.ObservedSize > observation.DiscoveryObservedSize ||
		proof.ObservedSize == observation.DiscoveryObservedSize && proof.ObservedMTimeNS != observation.DiscoveryObservedMTimeNS {
		return FilePlan{}, fmt.Errorf("%w: file plan proof differs from frozen discovery", ErrSourceChanged)
	}
	plan := FilePlan{
		SourceFileID: observation.SourceFileID, Generation: observation.Generation,
		PhysicalObservedSize: observation.DiscoveryObservedSize,
		Compressed:           observation.Compressed, VerifiedRedundant: verifiedRedundant,
	}
	if errors.Is(checkpointErr, sql.ErrNoRows) {
		plan.Kind = PlanRebuild
		return plan, nil
	}
	if checkpoint.status != "ready" || checkpoint.parserVersion != parserVersion || checkpoint.offset < 0 {
		plan.Kind = PlanRebuild
		return plan, nil
	}
	if checkpoint.offset == 0 && checkpoint.guardPresent {
		plan.Kind = PlanRebuild
		return plan, nil
	}
	if observation.Compressed {
		if proof.ObservedSize != observation.DiscoveryObservedSize ||
			proof.ObservedMTimeNS != observation.DiscoveryObservedMTimeNS ||
			checkpoint.offset != observation.DiscoveryObservedSize {
			return FilePlan{}, fmt.Errorf("%w: compressed source proof changed", ErrSourceChanged)
		}
		if checkpoint.offset > 0 {
			if err := verifyGuard(GuardPlan{
				Path: observation.CurrentPath, Identity: observation.Identity,
				PhysicalStartOffset:     checkpoint.offset,
				PhysicalObservedSize:    observation.DiscoveryObservedSize,
				PhysicalObservedMTimeNS: observation.DiscoveryObservedMTimeNS,
				ExpectedGuard:           checkpoint.guard,
				Compressed:              true,
			}); err != nil {
				if errors.Is(err, ErrGuardMismatch) {
					plan.Kind = PlanRebuild
					return plan, nil
				}
				return FilePlan{}, err
			}
		}
		plan.Kind = PlanSkip
		plan.PhysicalStartOffset = 0
		plan.GuardHash = checkpoint.guard
		return plan, nil
	}
	if checkpoint.offset > observation.DiscoveryObservedSize || checkpoint.offset > proof.ObservedSize {
		plan.Kind = PlanRebuild
		return plan, nil
	}
	if checkpoint.offset > 0 && len(checkpoint.guard) == 0 {
		plan.Kind = PlanRebuild
		return plan, nil
	}
	if checkpoint.offset > 0 {
		if err := verifyGuard(GuardPlan{
			Path: observation.CurrentPath, Identity: observation.Identity,
			PhysicalStartOffset:     checkpoint.offset,
			PhysicalObservedSize:    observation.DiscoveryObservedSize,
			PhysicalObservedMTimeNS: observation.DiscoveryObservedMTimeNS,
			ExpectedGuard:           checkpoint.guard,
		}); err != nil {
			if errors.Is(err, ErrGuardMismatch) {
				plan.Kind = PlanRebuild
				return plan, nil
			}
			return FilePlan{}, err
		}
	}
	if checkpoint.offset == proof.ObservedSize &&
		observation.DiscoveryObservedSize == proof.ObservedSize &&
		observation.DiscoveryObservedMTimeNS == proof.ObservedMTimeNS {
		plan.Kind = PlanSkip
		plan.PhysicalStartOffset = checkpoint.offset
		plan.GuardHash = checkpoint.guard
		return plan, nil
	}
	plan.Kind = PlanReadFrom
	plan.PhysicalStartOffset = checkpoint.offset
	plan.GuardHash = checkpoint.guard
	return plan, nil
}

func areaName(area Area) string {
	if area == AreaSessions {
		return "sessions"
	}
	return "archived_sessions"
}

func regionState(snapshot DiscoverySnapshot, area string) RegionState {
	if area == "sessions" {
		return snapshot.Sessions
	}
	return snapshot.Archived
}

func acceptedProof(row catalogSource) AcceptedSourceProof {
	return AcceptedSourceProof{
		SourceFileID: row.id, Generation: row.generation, Identity: row.identity,
		ObservedSize: row.size, ObservedMTimeNS: row.mtimeNS, FileStatus: row.status,
	}
}

func nullableString(value sql.NullString) *string {
	if !value.Valid {
		return nil
	}
	copy := value.String
	return &copy
}

func requireOneCatalogRow(result sql.Result) error {
	rows, err := result.RowsAffected()
	if err != nil {
		return err
	}
	if rows != 1 {
		return fmt.Errorf("%w: source catalog compare-and-swap failed", ErrInvalidCatalog)
	}
	return nil
}

func discoveredFromObservation(observation SourceObservation) DiscoveredFile {
	area := observation.Area
	return DiscoveredFile{
		Path: observation.CurrentPath, Area: area, Identity: observation.Identity,
		Size: observation.DiscoveryObservedSize, MTimeNS: observation.DiscoveryObservedMTimeNS,
		Compressed: observation.Compressed,
	}
}
