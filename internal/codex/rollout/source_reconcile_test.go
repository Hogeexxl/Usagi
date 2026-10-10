package rollout

import (
	"context"
	"database/sql"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/platform"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	"github.com/zeebo/blake3"
)

func TestDiscoveryReconcileIdentitySwapKeepsOwnersAndClearsStaging(t *testing.T) {
	_, bound := newRolloutTestStorage(t)
	dir := t.TempDir()
	pathP := filepath.Join(dir, "sessions", "P.jsonl")
	pathQ := filepath.Join(dir, "sessions", "Q.jsonl")
	seedRolloutSource(t, bound, rolloutSourceSeed{
		id: 10, path: pathP, area: "sessions", identity: PhysicalIdentity{DeviceID: 1, Inode: 101},
		generation: 3, size: 8, mtimeNS: 11, status: SourceFilePresent, threadID: stringPointer("thread-x"),
	}, nil, false)
	seedRolloutSource(t, bound, rolloutSourceSeed{
		id: 11, path: pathQ, area: "sessions", identity: PhysicalIdentity{DeviceID: 1, Inode: 102},
		generation: 4, size: 9, mtimeNS: 12, status: SourceFilePresent, threadID: stringPointer("thread-y"),
	}, nil, false)

	observations, err := ReconcileSources(bound, DiscoverySnapshot{
		StartedAtMS: 20, Sessions: RegionComplete, Archived: RegionComplete,
		Files: []DiscoveredFile{
			{Path: pathP, Area: AreaSessions, Identity: PhysicalIdentity{DeviceID: 1, Inode: 102}, Size: 9, MTimeNS: 12},
			{Path: pathQ, Area: AreaSessions, Identity: PhysicalIdentity{DeviceID: 1, Inode: 101}, Size: 8, MTimeNS: 11},
		},
	}, 5, 12, fixedVisibilityProbe([]byte("same")))
	if err != nil {
		t.Fatal(err)
	}
	if len(observations) != 2 || observations[0].SourceFileID != 10 || observations[1].SourceFileID != 11 {
		t.Fatalf("observations are not source-id sorted: %+v", observations)
	}
	rowX := readRolloutSource(t, bound, 10)
	rowY := readRolloutSource(t, bound, 11)
	if rowX.path != pathQ || rowX.identity.Inode != 101 || rowX.generation != 3 || rowY.path != pathP || rowY.identity.Inode != 102 || rowY.generation != 4 {
		t.Fatalf("swap changed owner continuity: X=%+v Y=%+v", rowX, rowY)
	}
	if got := countReconcileStagingPaths(t, bound); got != 0 {
		t.Fatalf("staging rows after commit = %d", got)
	}
}

func TestDiscoveryReconcileThreeOwnerCycleKeepsIdentityAndGeneration(t *testing.T) {
	_, bound := newRolloutTestStorage(t)
	dir := t.TempDir()
	paths := []string{
		filepath.Join(dir, "sessions", "P.jsonl"),
		filepath.Join(dir, "sessions", "Q.jsonl"),
		filepath.Join(dir, "sessions", "R.jsonl"),
	}
	identities := []PhysicalIdentity{
		{DeviceID: 1, Inode: 111},
		{DeviceID: 1, Inode: 112},
		{DeviceID: 1, Inode: 113},
	}
	for index := range paths {
		seedRolloutSource(t, bound, rolloutSourceSeed{
			id: int64(10 + index), path: paths[index], area: "sessions", identity: identities[index],
			generation: int64(2 + index), size: int64(8 + index), mtimeNS: int64(20 + index), status: SourceFilePresent,
		}, nil, false)
	}
	_, err := ReconcileSources(bound, DiscoverySnapshot{
		StartedAtMS: 25, Sessions: RegionComplete, Archived: RegionComplete,
		Files: []DiscoveredFile{
			{Path: paths[1], Area: AreaSessions, Identity: identities[0], Size: 8, MTimeNS: 20},
			{Path: paths[2], Area: AreaSessions, Identity: identities[1], Size: 9, MTimeNS: 21},
			{Path: paths[0], Area: AreaSessions, Identity: identities[2], Size: 10, MTimeNS: 22},
		},
	}, 5, 12, fixedVisibilityProbe([]byte("same")))
	if err != nil {
		t.Fatal(err)
	}
	for index := range paths {
		row := readRolloutSource(t, bound, int64(10+index))
		wantPath := paths[(index+1)%len(paths)]
		if row.path != wantPath || row.identity != identities[index] || row.generation != int64(2+index) {
			t.Fatalf("cycle source %d = %+v, want path=%q identity=%+v generation=%d", row.id, row, wantPath, identities[index], 2+index)
		}
	}
	if got := countReconcileStagingPaths(t, bound); got != 0 {
		t.Fatalf("staging rows after cycle = %d", got)
	}
}

func TestDiscoveryReconcileDisplacedMissingRowUsesHistoricalTombstone(t *testing.T) {
	_, bound := newRolloutTestStorage(t)
	dir := t.TempDir()
	pathP := filepath.Join(dir, "sessions", "same.jsonl")
	pathQ := filepath.Join(dir, "sessions", "old.jsonl")
	seedRolloutSource(t, bound, rolloutSourceSeed{
		id: 10, path: pathP, area: "sessions", identity: PhysicalIdentity{DeviceID: 2, Inode: 201},
		generation: 1, size: 4, mtimeNS: 1, status: SourceFileMissing,
	}, nil, false)
	seedRolloutSource(t, bound, rolloutSourceSeed{
		id: 11, path: pathQ, area: "sessions", identity: PhysicalIdentity{DeviceID: 2, Inode: 202},
		generation: 5, size: 5, mtimeNS: 2, status: SourceFilePresent,
	}, nil, false)

	_, err := ReconcileSources(bound, DiscoverySnapshot{
		StartedAtMS: 30, Sessions: RegionComplete, Archived: RegionComplete,
		Files: []DiscoveredFile{{Path: pathP, Area: AreaSessions, Identity: PhysicalIdentity{DeviceID: 2, Inode: 202}, Size: 5, MTimeNS: 2}},
	}, 5, 12, fixedVisibilityProbe([]byte("same")))
	if err != nil {
		t.Fatal(err)
	}
	old := readRolloutSource(t, bound, 10)
	selected := readRolloutSource(t, bound, 11)
	if old.path != "@historical/10/1" || old.status != string(SourceFileMissing) || old.generation != 1 || old.identity.Inode != 201 {
		t.Fatalf("displaced missing row = %+v", old)
	}
	if selected.path != pathP || selected.status != string(SourceFilePresent) || selected.generation != 5 || selected.identity.Inode != 202 {
		t.Fatalf("selected identity row = %+v", selected)
	}
	if got := countReconcileStagingPaths(t, bound); got != 0 {
		t.Fatalf("staging rows after commit = %d", got)
	}
}

func TestDiscoveryReconcileCompleteAndUnavailableRegions(t *testing.T) {
	_, bound := newRolloutTestStorage(t)
	sessionsPath := filepath.Join(t.TempDir(), "sessions", "session.jsonl")
	archivedPath := filepath.Join(t.TempDir(), "archived_sessions", "archived.jsonl")
	seedRolloutSource(t, bound, rolloutSourceSeed{
		id: 10, path: sessionsPath, area: "sessions", identity: PhysicalIdentity{DeviceID: 3, Inode: 301},
		generation: 1, size: 0, mtimeNS: 0, status: SourceFilePresent,
	}, nil, false)
	seedRolloutSource(t, bound, rolloutSourceSeed{
		id: 11, path: archivedPath, area: "archived_sessions", identity: PhysicalIdentity{DeviceID: 3, Inode: 302},
		generation: 1, size: 0, mtimeNS: 0, status: SourceFilePresent,
	}, nil, false)
	_, err := ReconcileSources(bound, DiscoverySnapshot{
		StartedAtMS: 40, Sessions: RegionUnavailable, Archived: RegionComplete,
	}, 5, 12, fixedVisibilityProbe([]byte("same")))
	if err != nil {
		t.Fatal(err)
	}
	if row := readRolloutSource(t, bound, 10); row.status != string(SourceFilePresent) {
		t.Fatalf("unavailable sessions source status = %q", row.status)
	}
	if row := readRolloutSource(t, bound, 11); row.status != string(SourceFileMissing) {
		t.Fatalf("complete archived source status = %q", row.status)
	}
}

func TestDiscoveryReconcileReplacementInvalidatesOwnerAndBumpsRevisionOnce(t *testing.T) {
	db, bound := newRolloutTestStorage(t)
	path := filepath.Join(t.TempDir(), "rollout-replacement.jsonl")
	if err := os.WriteFile(path, []byte("{}\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	file, metadata := discoveredPhysicalFile(t, path, false)
	oldThread := "old-thread"
	checkpoints := standardCheckpoints(10, []byte("old-metadata-guard"), []byte("old-usage-guard"))
	seedRolloutSource(t, bound, rolloutSourceSeed{
		id: 10, path: path, area: "sessions", identity: PhysicalIdentity{DeviceID: file.Identity.DeviceID + 10, Inode: file.Identity.Inode + 10},
		generation: 1, size: 100, mtimeNS: 10, status: SourceFilePresent, threadID: &oldThread,
	}, checkpoints, true)
	probe, calls := changingVisibilityProbe()
	observations, err := ReconcileSources(bound, DiscoverySnapshot{
		StartedAtMS: 50, Sessions: RegionComplete, Archived: RegionComplete,
		Files: []DiscoveredFile{file},
	}, 5, 12, probe)
	if err != nil {
		t.Fatal(err)
	}
	if *calls != 2 || db.CurrentRevision().DataRevision != 1 {
		t.Fatalf("probe calls/revision = %d/%d, want 2/1", *calls, db.CurrentRevision().DataRevision)
	}
	if len(observations) != 1 || observations[0].Generation != 2 || observations[0].BoundThreadID != nil ||
		observations[0].AcceptedObservedSize != metadata.Size || observations[0].DiscoveryObservedSize != metadata.Size {
		t.Fatalf("replacement observation = %+v", observations)
	}
	row := readRolloutSource(t, bound, 10)
	if row.generation != 2 || row.identity != file.Identity || row.thread.Valid || row.size != metadata.Size || row.mtimeNS != metadata.MTimeNS {
		t.Fatalf("replacement catalog = %+v", row)
	}
	if countMetadataFacts(t, bound, 10) != 0 {
		t.Fatal("replacement retained prior-generation metadata fact")
	}
	assertRolloutCheckpointsReset(t, bound, 10, 5, 12)
}

func TestDiscoveryReconcileZstdSameSizeMTimeChangeInvalidatesGeneration(t *testing.T) {
	_, bound := newRolloutTestStorage(t)
	path := filepath.Join(t.TempDir(), "rollout-rewrite.jsonl.zst")
	identity := PhysicalIdentity{DeviceID: 4, Inode: 401}
	seedRolloutSource(t, bound, rolloutSourceSeed{
		id: 12, path: path, area: "sessions", identity: identity,
		generation: 7, size: 64, mtimeNS: 100, status: SourceFilePresent, threadID: stringPointer("old-owner"),
	}, standardCheckpoints(20, []byte("metadata-guard"), []byte("usage-guard")), true)
	_, err := ReconcileSources(bound, DiscoverySnapshot{
		StartedAtMS: 60, Sessions: RegionComplete, Archived: RegionComplete,
		Files: []DiscoveredFile{{Path: path, Area: AreaSessions, Identity: identity, Size: 64, MTimeNS: 101, Compressed: true}},
	}, 5, 12, fixedVisibilityProbe([]byte("same")))
	if err != nil {
		t.Fatal(err)
	}
	row := readRolloutSource(t, bound, 12)
	if row.generation != 8 || row.size != 64 || row.mtimeNS != 101 || row.thread.Valid {
		t.Fatalf("zstd rewrite catalog = %+v", row)
	}
	if countMetadataFacts(t, bound, 12) != 0 {
		t.Fatal("zstd rewrite retained old metadata fact")
	}
	assertRolloutCheckpointsReset(t, bound, 12, 5, 12)
}

func TestDiscoveryReconcileRollbackRestoresCatalogFactsAndCheckpoints(t *testing.T) {
	db, bound := newRolloutTestStorage(t)
	path := filepath.Join(t.TempDir(), "rollout-rollback.jsonl")
	if err := os.WriteFile(path, []byte("{}\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	file, _ := discoveredPhysicalFile(t, path, false)
	thread := "before"
	seedRolloutSource(t, bound, rolloutSourceSeed{
		id: 13, path: path, area: "sessions", identity: PhysicalIdentity{DeviceID: 99, Inode: 999},
		generation: 3, size: 40, mtimeNS: 50, status: SourceFilePresent, threadID: &thread,
	}, standardCheckpoints(20, []byte("metadata"), []byte("usage")), true)
	probeError := errors.New("after projection failed")
	var calls int
	probe := func(*source.WriteTx) ([]byte, error) {
		calls++
		if calls == 2 {
			return nil, probeError
		}
		return []byte("before"), nil
	}
	_, err := ReconcileSources(bound, DiscoverySnapshot{
		StartedAtMS: 70, Sessions: RegionComplete, Archived: RegionComplete, Files: []DiscoveredFile{file},
	}, 5, 12, probe)
	if !errors.Is(err, probeError) {
		t.Fatalf("ReconcileSources() error = %v", err)
	}
	row := readRolloutSource(t, bound, 13)
	if calls != 2 || row.generation != 3 || row.identity.Inode != 999 || row.thread.String != thread || row.size != 40 || row.mtimeNS != 50 {
		t.Fatalf("rollback catalog = %+v, probe calls=%d", row, calls)
	}
	if countMetadataFacts(t, bound, 13) != 1 {
		t.Fatal("rollback did not restore metadata fact")
	}
	assertSeedCheckpointsUntouched(t, bound, 13, 20)
	if db.CurrentRevision().DataRevision != 0 {
		t.Fatalf("rollback revision = %d, want 0", db.CurrentRevision().DataRevision)
	}
}

func TestDiscoveryReaderNullGuardSkipsQuorumButPlansRebuild(t *testing.T) {
	_, bound := newRolloutTestStorage(t)
	observation, oldBytes, newFile := seedAppendCandidate(t, bound, []byte("{\"old\":1}\n"), []byte("{\"new\":2}\n"), nil, true)
	updated, err := PreflightConsumerGuardQuorum(bound, []SourceObservation{observation}, 80, 5, 12, nil)
	if err != nil {
		t.Fatal(err)
	}
	if len(updated) != 1 || updated[0].Generation != observation.Generation || !reflect.DeepEqual(updated[0], observation) {
		t.Fatalf("NULL guard changed quorum observation: %+v", updated)
	}
	row := readRolloutSource(t, bound, observation.SourceFileID)
	if row.generation != observation.Generation || row.size != int64(len(oldBytes)) {
		t.Fatalf("NULL guard changed accepted proof: %+v", row)
	}
	plan, err := PlanFile(bound, observation, ConsumerUsage, 12, false)
	if err != nil {
		t.Fatal(err)
	}
	if plan.Kind != PlanRebuild || plan.PhysicalObservedSize != newFile.Size {
		t.Fatalf("NULL guard plan = %+v, want rebuild at discovery size %d", plan, newFile.Size)
	}
}

func TestDiscoveryReaderEmptyBlobGuardInvalidatesAndRollbackIsAtomic(t *testing.T) {
	db, bound := newRolloutTestStorage(t)
	observation, oldBytes, _ := seedAppendCandidate(t, bound, []byte("{\"old\":1}\n"), []byte("{\"new\":2}\n"), []byte{}, true)
	guardPresent := false
	if err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow("SELECT guard_hash IS NOT NULL FROM codex_source_checkpoints WHERE source_file_id=?1 AND consumer_kind='usage'", observation.SourceFileID).Scan(&guardPresent)
	}); err != nil {
		t.Fatal(err)
	}
	if !guardPresent {
		t.Fatal("empty guard was stored as SQL NULL")
	}
	frozen := observation
	rollback := errors.New("after visibility probe failed")
	var calls int
	failingProbe := func(*source.WriteTx) ([]byte, error) {
		calls++
		if calls == 2 {
			return nil, rollback
		}
		return []byte("before"), nil
	}
	if got, err := PreflightConsumerGuardQuorum(bound, []SourceObservation{observation}, 81, 5, 12, failingProbe); !errors.Is(err, rollback) || got != nil {
		t.Fatalf("failed quorum = %+v, %v", got, err)
	}
	if !reflect.DeepEqual(observation, frozen) {
		t.Fatalf("failed quorum mutated caller observation: %+v", observation)
	}
	row := readRolloutSource(t, bound, observation.SourceFileID)
	if row.generation != observation.Generation || row.size != int64(len(oldBytes)) || row.thread.String != "thread-before" || countMetadataFacts(t, bound, observation.SourceFileID) != 1 {
		t.Fatalf("failed quorum left catalog mutation: %+v", row)
	}
	assertSeedCheckpointsUntouched(t, bound, observation.SourceFileID, int64(len(oldBytes)))
	if db.CurrentRevision().DataRevision != 0 {
		t.Fatalf("failed quorum revision = %d", db.CurrentRevision().DataRevision)
	}

	probe, probeCalls := changingVisibilityProbe()
	updated, err := PreflightConsumerGuardQuorum(bound, []SourceObservation{observation}, 82, 5, 12, probe)
	if err != nil {
		t.Fatal(err)
	}
	if *probeCalls != 2 || len(updated) != 1 || updated[0].Generation != observation.Generation+1 || updated[0].BoundThreadID != nil {
		t.Fatalf("invalid guard preflight = %+v, probe calls=%d", updated, *probeCalls)
	}
	if db.CurrentRevision().DataRevision != 1 {
		t.Fatalf("invalidation revision = %d, want exactly 1", db.CurrentRevision().DataRevision)
	}
	row = readRolloutSource(t, bound, observation.SourceFileID)
	if row.generation != observation.Generation+1 || row.thread.Valid || row.size != observation.DiscoveryObservedSize || row.mtimeNS != observation.DiscoveryObservedMTimeNS {
		t.Fatalf("invalidated catalog = %+v", row)
	}
	if countMetadataFacts(t, bound, observation.SourceFileID) != 0 {
		t.Fatal("guard invalidation retained old metadata fact")
	}
	assertRolloutCheckpointsReset(t, bound, observation.SourceFileID, 5, 12)
}

func TestDiscoveryAppendObservationAndFreshAcceptedProofAcrossConsumers(t *testing.T) {
	_, bound := newRolloutTestStorage(t)
	oldBytes := []byte("first\n")
	newBytes := []byte("second\n")
	observation, _, discovered := seedAppendCandidate(t, bound, oldBytes, newBytes, blake3Sum(oldBytes), false)
	reconciled, err := ReconcileSources(bound, DiscoverySnapshot{
		StartedAtMS: 89, Sessions: RegionComplete, Archived: RegionComplete, Files: []DiscoveredFile{discovered},
	}, 5, 12, nil)
	if err != nil {
		t.Fatal(err)
	}
	if len(reconciled) != 1 {
		t.Fatalf("append reconcile observations = %+v", reconciled)
	}
	observation = reconciled[0]
	frozenObservation := observation
	acceptedBefore := readRolloutSource(t, bound, observation.SourceFileID)
	if acceptedBefore.size != int64(len(oldBytes)) || observation.AcceptedObservedSize != int64(len(oldBytes)) ||
		observation.DiscoveryObservedSize != discovered.Size || observation.DiscoveryObservedSize != int64(len(oldBytes)+len(newBytes)) ||
		observation.AcceptedObservedMTimeNS == observation.DiscoveryObservedMTimeNS {
		t.Fatalf("append observation = %+v; DB proof = %+v", observation, acceptedBefore)
	}
	quorum, err := PreflightConsumerGuardQuorum(bound, []SourceObservation{observation}, 90, 5, 12, nil)
	if err != nil {
		t.Fatal(err)
	}
	if len(quorum) != 1 || !reflect.DeepEqual(quorum[0], frozenObservation) {
		t.Fatalf("successful quorum changed observation: %+v", quorum)
	}
	plan, err := PlanFile(bound, observation, ConsumerMetadata, 5, false)
	if err != nil || plan.Kind != PlanReadFrom || plan.PhysicalStartOffset != int64(len(oldBytes)) || plan.PhysicalObservedSize != discovered.Size {
		t.Fatalf("pre-accept metadata plan = %+v, %v", plan, err)
	}

	whole := append(append([]byte(nil), oldBytes...), newBytes...)
	fullGuard := blake3.Sum256(whole)
	if err := bound.Write(func(tx *source.WriteTx) error {
		if err := AcceptAppendProof(tx, observation); err != nil {
			return err
		}
		return updateTestCheckpoint(tx, observation.SourceFileID, ConsumerMetadata, discovered.Size, fullGuard[:])
	}); err != nil {
		t.Fatalf("Pass1 append proof/checkpoint: %v", err)
	}
	if got, err := PlanFile(bound, observation, ConsumerMetadata, 5, false); err != nil || got.Kind != PlanSkip || got.PhysicalObservedSize != discovered.Size {
		t.Fatalf("Pass1 post-accept plan = %+v, %v", got, err)
	}
	if got, err := PlanFile(bound, observation, ConsumerUsage, 12, false); err != nil || got.Kind != PlanReadFrom || got.PhysicalObservedSize != discovered.Size || got.PhysicalStartOffset != int64(len(oldBytes)) {
		t.Fatalf("Pass2 post-accept plan = %+v, %v", got, err)
	}
	if err := bound.Write(func(tx *source.WriteTx) error {
		if err := AcceptAppendProof(tx, observation); err != nil {
			return err
		}
		return updateTestCheckpoint(tx, observation.SourceFileID, ConsumerUsage, discovered.Size, fullGuard[:])
	}); err != nil {
		t.Fatalf("Pass2 idempotent append proof/checkpoint: %v", err)
	}
	if got, err := PlanFile(bound, observation, ConsumerUsage, 12, false); err != nil || got.Kind != PlanSkip {
		t.Fatalf("Pass2 post-accept plan = %+v, %v", got, err)
	}
	if !reflect.DeepEqual(observation, frozenObservation) {
		t.Fatalf("append proof mutated frozen observation: %+v", observation)
	}
	acceptedAfter := readRolloutSource(t, bound, observation.SourceFileID)
	if acceptedAfter.size != discovered.Size || acceptedAfter.mtimeNS != discovered.MTimeNS || acceptedAfter.generation != observation.Generation {
		t.Fatalf("durable accepted proof = %+v", acceptedAfter)
	}
}

type rolloutSourceSeed struct {
	id         int64
	path       string
	area       string
	identity   PhysicalIdentity
	generation int64
	size       int64
	mtimeNS    int64
	status     SourceFileStatus
	threadID   *string
}

type rolloutCheckpointSeed struct {
	consumer string
	parser   int64
	offset   int64
	guard    []byte
	status   string
}

type rolloutStoredSource struct {
	id         int64
	thread     sql.NullString
	path       string
	area       string
	identity   PhysicalIdentity
	generation int64
	size       int64
	mtimeNS    int64
	status     string
}

func newRolloutTestStorage(t *testing.T) (*storage.DB, *source.Storage) {
	t.Helper()
	db, err := storage.Open(context.Background(), storage.Config{Path: filepath.Join(t.TempDir(), "rollout.sqlite3")})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := db.Close(); err != nil {
			t.Error(err)
		}
	})
	descriptor, err := source.NewDescriptor(domain.SourceCodex, "Codex")
	if err != nil {
		t.Fatal(err)
	}
	run, err := source.NewStorageFactory(db).Context(context.Background(), "rollout-test", descriptor)
	if err != nil {
		t.Fatal(err)
	}
	return db, run.Storage()
}

func seedRolloutSource(t *testing.T, bound *source.Storage, row rolloutSourceSeed, checkpoints []rolloutCheckpointSeed, metadataFact bool) {
	t.Helper()
	err := bound.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(privateTx storage.PrivateTx) error {
			_, err := privateTx.Exec(
				`INSERT INTO codex_source_files(
				 source_file_id,thread_id,current_path,source_area,device_id,inode,file_generation,
				 observed_size,observed_mtime_ns,file_status,last_seen_at_ms
				) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1)`,
				row.id, row.threadID, row.path, row.area, row.identity.DeviceID, row.identity.Inode,
				row.generation, row.size, row.mtimeNS, string(row.status),
			)
			if err != nil {
				return err
			}
			for _, checkpoint := range checkpoints {
				query := `INSERT INTO codex_source_checkpoints(
				 source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,processing_status,
				 last_successful_scan_at_ms,last_error_code
				) VALUES(?1,?2,?3,?4,?5,?6,10,'old-error')`
				args := []any{row.id, checkpoint.consumer, checkpoint.parser, checkpoint.offset, checkpoint.guard, checkpoint.status}
				if checkpoint.guard != nil && len(checkpoint.guard) == 0 {
					query = `INSERT INTO codex_source_checkpoints(
					 source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,processing_status,
					 last_successful_scan_at_ms,last_error_code
					) VALUES(?1,?2,?3,?4,zeroblob(0),?5,10,'old-error')`
					args = []any{row.id, checkpoint.consumer, checkpoint.parser, checkpoint.offset, checkpoint.status}
				}
				if _, err := privateTx.Exec(query, args...); err != nil {
					return err
				}
			}
			if metadataFact {
				_, err := privateTx.Exec(
					`INSERT INTO codex_rollout_metadata_facts(
					 source_file_id,file_generation,metadata_parser_version,resolved_through_offset,owning_thread_id,
					 continuation_state,ownership_confidence,fact_quality_status,updated_at_ms
					) VALUES(?1,?2,5,0,'thread-before','owning_live','confirmed','complete',1)`,
					row.id, row.generation,
				)
				return err
			}
			return nil
		})
	})
	if err != nil {
		t.Fatal(err)
	}
}

func standardCheckpoints(offset int64, metadataGuard, usageGuard []byte) []rolloutCheckpointSeed {
	return []rolloutCheckpointSeed{
		{consumer: ConsumerMetadata, parser: 1, offset: offset, guard: metadataGuard, status: "ready"},
		{consumer: ConsumerUsage, parser: 2, offset: offset, guard: usageGuard, status: "ready"},
	}
}

func seedAppendCandidate(t *testing.T, bound *source.Storage, oldBytes, appendBytes, usageGuard []byte, metadataFact bool) (SourceObservation, []byte, DiscoveredFile) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "rollout-append.jsonl")
	if err := os.WriteFile(path, oldBytes, 0o600); err != nil {
		t.Fatal(err)
	}
	oldTime := time.Unix(1_700_000_000, 0)
	if err := os.Chtimes(path, oldTime, oldTime); err != nil {
		t.Fatal(err)
	}
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	oldMetadata, err := platform.MetadataFromFile(file)
	if closeErr := file.Close(); closeErr != nil {
		t.Fatal(closeErr)
	}
	if err != nil {
		t.Fatal(err)
	}
	appendFile, err := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := appendFile.Write(appendBytes); err != nil {
		appendFile.Close()
		t.Fatal(err)
	}
	if err := appendFile.Close(); err != nil {
		t.Fatal(err)
	}
	newTime := oldTime.Add(time.Second)
	if err := os.Chtimes(path, newTime, newTime); err != nil {
		t.Fatal(err)
	}
	discovered, newMetadata := discoveredPhysicalFile(t, path, false)
	if discovered.Size != int64(len(oldBytes)+len(appendBytes)) || newMetadata.MTimeNS == oldMetadata.MTimeNS {
		t.Fatalf("append fixture metadata old=%+v new=%+v", oldMetadata, newMetadata)
	}
	thread := "thread-before"
	checkpoints := standardCheckpoints(int64(len(oldBytes)), blake3Sum(oldBytes), usageGuard)
	checkpoints[0].parser = 5
	checkpoints[1].parser = 12
	seedRolloutSource(t, bound, rolloutSourceSeed{
		id: 17, path: path, area: "sessions",
		identity:   PhysicalIdentity{DeviceID: oldMetadata.Identity.DeviceID, Inode: oldMetadata.Identity.Inode},
		generation: 1, size: oldMetadata.Size, mtimeNS: oldMetadata.MTimeNS, status: SourceFilePresent, threadID: &thread,
	}, checkpoints, metadataFact)
	observation := SourceObservation{
		SourceFileID: 17, Generation: 1, CurrentPath: path, Area: AreaSessions,
		Identity: discovered.Identity, BoundThreadID: &thread,
		AcceptedObservedSize: oldMetadata.Size, AcceptedObservedMTimeNS: oldMetadata.MTimeNS,
		DiscoveryObservedSize: discovered.Size, DiscoveryObservedMTimeNS: discovered.MTimeNS,
	}
	return observation, oldBytes, discovered
}

func updateTestCheckpoint(tx *source.WriteTx, sourceID int64, consumer string, offset int64, guard []byte) error {
	return tx.Private(func(privateTx storage.PrivateTx) error {
		_, err := privateTx.Exec(
			`UPDATE codex_source_checkpoints SET committed_offset=?1,guard_hash=?2,processing_status='ready',
			 last_successful_scan_at_ms=20,last_error_code=NULL
			 WHERE source_file_id=?3 AND consumer_kind=?4`,
			offset, guard, sourceID, consumer,
		)
		return err
	})
}

func readRolloutSource(t *testing.T, bound *source.Storage, sourceID int64) rolloutStoredSource {
	t.Helper()
	var row rolloutStoredSource
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow(
			`SELECT source_file_id,thread_id,current_path,source_area,device_id,inode,file_generation,
			 observed_size,observed_mtime_ns,file_status FROM codex_source_files WHERE source_file_id=?1`, sourceID,
		).Scan(&row.id, &row.thread, &row.path, &row.area, &row.identity.DeviceID, &row.identity.Inode,
			&row.generation, &row.size, &row.mtimeNS, &row.status)
	})
	if err != nil {
		t.Fatal(err)
	}
	return row
}

func assertRolloutCheckpointsReset(t *testing.T, bound *source.Storage, sourceID, metadataParser, usageParser int64) {
	t.Helper()
	seen := map[string]bool{}
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		rows, err := reader.Query(
			`SELECT consumer_kind,parser_version,committed_offset,guard_hash IS NOT NULL,processing_status,
			 last_successful_scan_at_ms,last_error_code FROM codex_source_checkpoints WHERE source_file_id=?1`, sourceID,
		)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var consumer, status string
			var parser, offset int64
			var guardPresent bool
			var successful sql.NullInt64
			var errorCode sql.NullString
			if err := rows.Scan(&consumer, &parser, &offset, &guardPresent, &status, &successful, &errorCode); err != nil {
				return err
			}
			wantParser := metadataParser
			if consumer == ConsumerUsage {
				wantParser = usageParser
			}
			if parser != wantParser || offset != 0 || guardPresent || status != "rebuild_required" || successful.Valid || errorCode.Valid {
				return errors.New("source checkpoint was not fully reset")
			}
			seen[consumer] = true
		}
		return rows.Err()
	})
	if err != nil {
		t.Fatal(err)
	}
	if !seen[ConsumerMetadata] || !seen[ConsumerUsage] {
		t.Fatalf("reset checkpoints missing: %+v", seen)
	}
}

func assertSeedCheckpointsUntouched(t *testing.T, bound *source.Storage, sourceID, offset int64) {
	t.Helper()
	err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		var count int
		if err := reader.QueryRow(
			`SELECT COUNT(*) FROM codex_source_checkpoints
			 WHERE source_file_id=?1 AND committed_offset=?2 AND processing_status='ready'
			 AND last_successful_scan_at_ms=10 AND last_error_code='old-error'`, sourceID, offset,
		).Scan(&count); err != nil {
			return err
		}
		if count != 2 {
			return errors.New("source checkpoints changed")
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}

func countMetadataFacts(t *testing.T, bound *source.Storage, sourceID int64) int {
	t.Helper()
	var count int
	if err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow("SELECT COUNT(*) FROM codex_rollout_metadata_facts WHERE source_file_id=?1", sourceID).Scan(&count)
	}); err != nil {
		t.Fatal(err)
	}
	return count
}

func countReconcileStagingPaths(t *testing.T, bound *source.Storage) int {
	t.Helper()
	var count int
	if err := bound.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow("SELECT COUNT(*) FROM codex_source_files WHERE current_path LIKE '@reconcile/%'").Scan(&count)
	}); err != nil {
		t.Fatal(err)
	}
	return count
}

func fixedVisibilityProbe(value []byte) ActiveCompactionVisibilityProbe {
	return func(*source.WriteTx) ([]byte, error) {
		return append([]byte(nil), value...), nil
	}
}

func changingVisibilityProbe() (ActiveCompactionVisibilityProbe, *int) {
	calls := 0
	return func(*source.WriteTx) ([]byte, error) {
		calls++
		if calls%2 == 1 {
			return []byte("before"), nil
		}
		return []byte("after"), nil
	}, &calls
}

func stringPointer(value string) *string { return &value }

func blake3Sum(content []byte) []byte {
	sum := blake3.Sum256(content)
	return sum[:]
}
