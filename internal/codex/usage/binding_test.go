package usage

import (
	"errors"
	"reflect"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

func TestBindingConfirmedOwnerChangePreservesActiveAttribution(t *testing.T) {
	for _, withBuild := range []bool{false, true} {
		name := "without build"
		if withBuild {
			name = "with build"
		}
		t.Run(name, func(t *testing.T) {
			run := newUsageTestRun(t)
			fixture := seedBindingOwnerChange(t, run, withBuild)
			var outerTx *source.WriteTx
			projectCalls := 0
			invalidatorCalls := 0
			deps := BindingReconcileDeps{
				ProjectCompaction: func(tx *source.WriteTx, target source.UsageWriteTarget, owner string) (CompactionVisibilityProjection, error) {
					if tx != outerTx || target != source.UsageTargetActive {
						t.Fatalf("projector did not use the outer Active transaction: tx=%p target=%v", tx, target)
					}
					projectCalls++
					return bindingTestProjection(tx, target, owner)
				},
			}
			deps.InvalidateBuild = func(tx *source.WriteTx, request BuildBindingInvalidationRequest) (BuildBindingInvalidationResult, error) {
				invalidatorCalls++
				if !withBuild || tx != outerTx || request.ThreadID != "new-owner" ||
					!reflect.DeepEqual(request.PreviousRoot, stringPointer("old-root")) ||
					!reflect.DeepEqual(request.NextRoot, stringPointer("new-root")) ||
					!reflect.DeepEqual(request.BindingChangedSourceIDs, []int64{fixture.sourceFileID}) || request.CommittedAtMS != 50 {
					t.Fatalf("invalidator request/transaction mismatch: tx=%p request=%+v", tx, request)
				}
				var buildRoot string
				if err := tx.Private(func(private storage.PrivateTx) error {
					return private.QueryRow(`SELECT root_session_id FROM usage_events WHERE source='codex' AND event_id='build-new-owner'`).Scan(&buildRoot)
				}); err != nil {
					return BuildBindingInvalidationResult{}, err
				}
				if buildRoot != "new-root" {
					return BuildBindingInvalidationResult{}, errors.New("invalidator could not observe the Build root rebind in the same transaction")
				}
				return BuildBindingInvalidationResult{
					InvalidatedSourceFileIDs: []int64{fixture.sourceFileID},
					RetryRootIDs:             []string{"retry-root"},
				}, nil
			}

			var gotNeedsBuild bool
			var gotInvalidated []int64
			var gotRetryRoots []string
			if err := run.Storage().Write(func(tx *source.WriteTx) error {
				outerTx = tx
				var err error
				_, gotNeedsBuild, gotInvalidated, gotRetryRoots, err = ReconcileMetadataUsageBinding(
					tx, deps, "new-owner", stringPointer("old-root"), stringPointer("new-root"),
					[]int64{fixture.sourceFileID}, 50,
				)
				return err
			}); err != nil {
				t.Fatal(err)
			}
			wantInvalidatorCalls := 0
			if withBuild {
				wantInvalidatorCalls = 1
			}
			if projectCalls != 2 || invalidatorCalls != wantInvalidatorCalls {
				t.Fatalf("seam calls = projector %d, invalidator %d", projectCalls, invalidatorCalls)
			}
			if !gotNeedsBuild || !reflect.DeepEqual(gotInvalidated, []int64{fixture.sourceFileID}) {
				t.Fatalf("Active contributor owner change did not require a shadow build: needs=%t invalidated=%v", gotNeedsBuild, gotInvalidated)
			}
			wantRetry := []string(nil)
			if withBuild {
				wantRetry = []string{"retry-root"}
			}
			if !reflect.DeepEqual(gotRetryRoots, wantRetry) {
				t.Fatalf("retry roots = %v, want %v", gotRetryRoots, wantRetry)
			}
			assertBindingAttribution(t, run, fixture.sourceFileID, "old-owner", "old-root", "active-old-owner")
			if withBuild {
				var buildRoot string
				if err := run.Storage().PrivateRead(func(private storage.PrivateReader) error {
					return private.QueryRow(`SELECT root_session_id FROM usage_events WHERE source='codex' AND event_id='build-new-owner'`).Scan(&buildRoot)
				}); err != nil {
					t.Fatal(err)
				}
				if buildRoot != "new-root" {
					t.Fatalf("same-thread Build row root = %q", buildRoot)
				}
			}
		})
	}
}

func TestBindingSameThreadRootRebindUpdatesCanonicalAndPrivateRows(t *testing.T) {
	run := newUsageTestRun(t)
	var primarySourceID, foreignSourceID int64
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		if err := seedUsageTestThreads(tx, "owner", "foreign-owner", "old-root", "new-root"); err != nil {
			return err
		}
		if err := activateUsageTestEpoch(tx); err != nil {
			return err
		}
		var err error
		primarySourceID, err = insertUsageTestSourceFile(tx, "root-rebind-primary.jsonl", "owner")
		if err != nil {
			return err
		}
		foreignSourceID, err = insertUsageTestSourceFile(tx, "root-rebind-foreign.jsonl", "foreign-owner")
		if err != nil {
			return err
		}
		if err := writeBindingTestEvent(tx, source.UsageTargetActive, "root-rebind-primary", "owner", "old-root", primarySourceID); err != nil {
			return err
		}
		if err := writeBindingTestEvent(tx, source.UsageTargetActive, "root-rebind-foreign", "foreign-owner", "old-root", foreignSourceID); err != nil {
			return err
		}
		if err := writeBindingTestSourceState(tx, source.UsageTargetActive, primarySourceID, "owner", "old-root"); err != nil {
			return err
		}
		if err := writeBindingTestSourceState(tx, source.UsageTargetActive, foreignSourceID, "foreign-owner", "old-root"); err != nil {
			return err
		}
		if err := writeBindingTestMarker(tx, source.UsageTargetActive, primarySourceID, "owner", "old-root"); err != nil {
			return err
		}
		if err := writeBindingTestMarker(tx, source.UsageTargetActive, foreignSourceID, "foreign-owner", "old-root"); err != nil {
			return err
		}
		return writeBindingTestSkillRows(tx, source.UsageTargetActive, primarySourceID, foreignSourceID, "owner", "foreign-owner", "old-root")
	}); err != nil {
		t.Fatal(err)
	}

	var visible bool
	projectCalls := 0
	deps := BindingReconcileDeps{ProjectCompaction: func(tx *source.WriteTx, target source.UsageWriteTarget, owner string) (CompactionVisibilityProjection, error) {
		projectCalls++
		return bindingTestProjection(tx, target, owner)
	}}
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		var err error
		visible, _, _, _, err = ReconcileMetadataUsageBinding(tx, deps, "owner", stringPointer("old-root"), stringPointer("new-root"), nil, 60)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if !visible || projectCalls != 2 {
		t.Fatalf("root rebind projection result visible=%t calls=%d", visible, projectCalls)
	}
	assertBindingAttribution(t, run, primarySourceID, "owner", "new-root", "root-rebind-primary")
	assertBindingAttribution(t, run, foreignSourceID, "foreign-owner", "old-root", "root-rebind-foreign")
}

func TestBindingQuarantineHitReturnsAllRootProofSources(t *testing.T) {
	run := newUsageTestRun(t)
	var sourceIDs []int64
	var unownedSourceID int64
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		if err := seedUsageTestThreads(tx, "owner", "root", "quarantine-root"); err != nil {
			return err
		}
		if err := activateUsageTestEpoch(tx); err != nil {
			return err
		}
		for _, path := range []string{"quarantine-proof-1.jsonl", "quarantine-proof-2.jsonl"} {
			sourceID, err := insertUsageTestSourceFile(tx, path, "owner")
			if err != nil {
				return err
			}
			sourceIDs = append(sourceIDs, sourceID)
		}
		var err error
		unownedSourceID, err = insertUsageTestSourceFile(tx, "new-unowned-source.jsonl", "owner")
		if err != nil {
			return err
		}
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`INSERT INTO codex_usage_session_quarantine(
				ledger_epoch,root_session_id,primary_error_code,last_activity_at_ms,first_seen_at_ms,updated_at_ms
			) VALUES(1,'quarantine-root','fatal',10,1,10)`); err != nil {
				return err
			}
			for _, sourceID := range sourceIDs {
				if _, err := private.Exec(`INSERT INTO codex_usage_session_quarantine_sources(
					ledger_epoch,root_session_id,source_file_id,file_generation,device_id,inode,observed_size,updated_at_ms
				) SELECT 1,'quarantine-root',source_file_id,file_generation,device_id,inode,observed_size,10
				  FROM codex_source_files WHERE source_file_id=?`, sourceID); err != nil {
					return err
				}
			}
			return nil
		})
	}); err != nil {
		t.Fatal(err)
	}

	var invalidated []int64
	var retryRoots []string
	var needsBuild, visible bool
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		if _, _, _, _, err := ReconcileMetadataUsageBinding(tx, BindingReconcileDeps{}, "owner", stringPointer("root"), stringPointer("root"), nil, 70); err != nil {
			return err
		}
		var count int
		if err := tx.Private(func(private storage.PrivateTx) error {
			return private.QueryRow(`SELECT COUNT(*) FROM codex_usage_session_quarantine WHERE root_session_id='quarantine-root'`).Scan(&count)
		}); err != nil {
			return err
		}
		if count != 1 {
			t.Fatalf("no-op binding deleted quarantine root: count=%d", count)
		}
		deps := BindingReconcileDeps{ProjectCompaction: func(tx *source.WriteTx, target source.UsageWriteTarget, owner string) (CompactionVisibilityProjection, error) {
			return bindingTestProjection(tx, target, owner)
		}}
		newSourceVisible, newSourceNeedsBuild, newSourceInvalidated, newSourceRetryRoots, err := ReconcileMetadataUsageBinding(
			tx, deps, "owner", stringPointer("root"), stringPointer("root"), []int64{unownedSourceID}, 70,
		)
		if err != nil {
			return err
		}
		if newSourceVisible || newSourceNeedsBuild || len(newSourceInvalidated) != 0 || len(newSourceRetryRoots) != 0 {
			t.Fatalf("first-time source binding without Active contribution required a build: visible=%t needsBuild=%t invalidated=%v retry=%v", newSourceVisible, newSourceNeedsBuild, newSourceInvalidated, newSourceRetryRoots)
		}
		if err := tx.Private(func(private storage.PrivateTx) error {
			return private.QueryRow(`SELECT COUNT(*) FROM codex_usage_session_quarantine WHERE root_session_id='quarantine-root'`).Scan(&count)
		}); err != nil {
			return err
		}
		if count != 1 {
			t.Fatalf("unrelated source binding deleted quarantine root: count=%d", count)
		}
		visible, needsBuild, invalidated, retryRoots, err = ReconcileMetadataUsageBinding(
			tx, deps, "owner", stringPointer("root"), stringPointer("root"), []int64{sourceIDs[0]}, 71,
		)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if !visible || !needsBuild || !reflect.DeepEqual(invalidated, sourceIDs) || !reflect.DeepEqual(retryRoots, []string{"quarantine-root"}) {
		t.Fatalf("quarantine invalidation lost root proof set: visible=%t needsBuild=%t sources=%v roots=%v", visible, needsBuild, invalidated, retryRoots)
	}
	var quarantineCount, proofCount int
	if err := run.Storage().PrivateRead(func(private storage.PrivateReader) error {
		if err := private.QueryRow(`SELECT COUNT(*) FROM codex_usage_session_quarantine WHERE root_session_id='quarantine-root'`).Scan(&quarantineCount); err != nil {
			return err
		}
		return private.QueryRow(`SELECT COUNT(*) FROM codex_usage_session_quarantine_sources WHERE root_session_id='quarantine-root'`).Scan(&proofCount)
	}); err != nil {
		t.Fatal(err)
	}
	if quarantineCount != 0 || proofCount != 0 {
		t.Fatalf("quarantine delete left rows: root=%d proofs=%d", quarantineCount, proofCount)
	}
}

func TestBindingRootChangeDeletesOldQuarantineWithoutRename(t *testing.T) {
	run := newUsageTestRun(t)
	var sourceFileID int64
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		if err := seedUsageTestThreads(tx, "owner", "old-root", "new-root"); err != nil {
			return err
		}
		if err := activateUsageTestEpoch(tx); err != nil {
			return err
		}
		var err error
		sourceFileID, err = insertUsageTestSourceFile(tx, "root-quarantine-proof.jsonl", "owner")
		if err != nil {
			return err
		}
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`INSERT INTO codex_usage_session_quarantine(
				ledger_epoch,root_session_id,primary_error_code,last_activity_at_ms,first_seen_at_ms,updated_at_ms
			) VALUES(1,'old-root','fatal',10,1,10)`); err != nil {
				return err
			}
			_, err := private.Exec(`INSERT INTO codex_usage_session_quarantine_sources(
				ledger_epoch,root_session_id,source_file_id,file_generation,device_id,inode,observed_size,updated_at_ms
			) VALUES(1,'old-root',?,1,1,1,128,10)`, sourceFileID)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	var visible, needsBuild bool
	var invalidated []int64
	var retryRoots []string
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		deps := BindingReconcileDeps{ProjectCompaction: func(tx *source.WriteTx, target source.UsageWriteTarget, owner string) (CompactionVisibilityProjection, error) {
			return bindingTestProjection(tx, target, owner)
		}}
		var err error
		visible, needsBuild, invalidated, retryRoots, err = ReconcileMetadataUsageBinding(
			tx, deps, "owner", stringPointer("old-root"), stringPointer("new-root"), nil, 72,
		)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if !visible || !needsBuild || !reflect.DeepEqual(invalidated, []int64{sourceFileID}) || !reflect.DeepEqual(retryRoots, []string{"old-root"}) {
		t.Fatalf("old quarantine root was not invalidated for retry: visible=%t needsBuild=%t sources=%v roots=%v", visible, needsBuild, invalidated, retryRoots)
	}
	var oldCount, newCount int
	if err := run.Storage().PrivateRead(func(private storage.PrivateReader) error {
		if err := private.QueryRow(`SELECT COUNT(*) FROM codex_usage_session_quarantine WHERE root_session_id='old-root'`).Scan(&oldCount); err != nil {
			return err
		}
		return private.QueryRow(`SELECT COUNT(*) FROM codex_usage_session_quarantine WHERE root_session_id='new-root'`).Scan(&newCount)
	}); err != nil {
		t.Fatal(err)
	}
	if oldCount != 0 || newCount != 0 {
		t.Fatalf("quarantine was renamed instead of deleted: old=%d new=%d", oldCount, newCount)
	}
}

func TestBindingInvalidatorErrorRollsBackRootRebind(t *testing.T) {
	run := newUsageTestRun(t)
	fixture := seedBindingOwnerChange(t, run, true)
	seamErr := errors.New("invalidator failed")
	err := run.Storage().Write(func(tx *source.WriteTx) error {
		deps := BindingReconcileDeps{
			ProjectCompaction: func(tx *source.WriteTx, target source.UsageWriteTarget, owner string) (CompactionVisibilityProjection, error) {
				return bindingTestProjection(tx, target, owner)
			},
			InvalidateBuild: func(tx *source.WriteTx, request BuildBindingInvalidationRequest) (BuildBindingInvalidationResult, error) {
				var root string
				if err := tx.Private(func(private storage.PrivateTx) error {
					return private.QueryRow(`SELECT root_session_id FROM usage_events WHERE source='codex' AND event_id='build-new-owner'`).Scan(&root)
				}); err != nil {
					return BuildBindingInvalidationResult{}, err
				}
				if root != "new-root" {
					return BuildBindingInvalidationResult{}, errors.New("Build root rebind was not visible inside invalidator")
				}
				return BuildBindingInvalidationResult{}, seamErr
			},
		}
		_, _, _, _, err := ReconcileMetadataUsageBinding(tx, deps, "new-owner", stringPointer("old-root"), stringPointer("new-root"), []int64{fixture.sourceFileID}, 80)
		return err
	})
	if !errors.Is(err, seamErr) {
		t.Fatalf("write error = %v, want invalidator error", err)
	}
	var activeRoot, buildRoot string
	if err := run.Storage().PrivateRead(func(private storage.PrivateReader) error {
		if err := private.QueryRow(`SELECT root_session_id FROM usage_events WHERE source='codex' AND event_id='active-old-owner'`).Scan(&activeRoot); err != nil {
			return err
		}
		return private.QueryRow(`SELECT root_session_id FROM usage_events WHERE source='codex' AND event_id='build-new-owner'`).Scan(&buildRoot)
	}); err != nil {
		t.Fatal(err)
	}
	if activeRoot != "old-root" || buildRoot != "old-root" {
		t.Fatalf("seam failure partially committed root rebind: active=%q build=%q", activeRoot, buildRoot)
	}
}

func TestBindingBuildRetryRootRequiresShadowBuild(t *testing.T) {
	run := newUsageTestRun(t)
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		if err := seedUsageTestThreads(tx, "owner", "old-root", "new-root"); err != nil {
			return err
		}
		if err := activateUsageTestEpoch(tx); err != nil {
			return err
		}
		if _, err := tx.BeginOrResumeUsageBuild(UsageParserVersion); err != nil {
			return err
		}
		var visible, needsBuild bool
		var retryRoots []string
		deps := BindingReconcileDeps{
			ProjectCompaction: func(tx *source.WriteTx, target source.UsageWriteTarget, owner string) (CompactionVisibilityProjection, error) {
				return bindingTestProjection(tx, target, owner)
			},
			InvalidateBuild: func(tx *source.WriteTx, request BuildBindingInvalidationRequest) (BuildBindingInvalidationResult, error) {
				return BuildBindingInvalidationResult{RetryRootIDs: []string{"old-root"}}, nil
			},
		}
		visible, needsBuild, _, retryRoots, err := ReconcileMetadataUsageBinding(
			tx, deps, "owner", stringPointer("old-root"), stringPointer("new-root"), nil, 90,
		)
		if err != nil {
			return err
		}
		if visible || !needsBuild || !reflect.DeepEqual(retryRoots, []string{"old-root"}) {
			t.Fatalf("Build retry root did not require a shadow build: visible=%t needsBuild=%t roots=%v", visible, needsBuild, retryRoots)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

type bindingOwnerChangeFixture struct{ sourceFileID int64 }

func seedBindingOwnerChange(t *testing.T, run source.RunContext, withBuild bool) bindingOwnerChangeFixture {
	t.Helper()
	var fixture bindingOwnerChangeFixture
	if err := run.Storage().Write(func(tx *source.WriteTx) error {
		if err := seedUsageTestThreads(tx, "old-owner", "new-owner", "old-root", "new-root"); err != nil {
			return err
		}
		if err := activateUsageTestEpoch(tx); err != nil {
			return err
		}
		var err error
		fixture.sourceFileID, err = insertUsageTestSourceFile(tx, "owner-change.jsonl", "old-owner")
		if err != nil {
			return err
		}
		if err := writeBindingTestEvent(tx, source.UsageTargetActive, "active-old-owner", "old-owner", "old-root", fixture.sourceFileID); err != nil {
			return err
		}
		if err := writeBindingTestSourceState(tx, source.UsageTargetActive, fixture.sourceFileID, "old-owner", "old-root"); err != nil {
			return err
		}
		if err := writeBindingTestMarker(tx, source.UsageTargetActive, fixture.sourceFileID, "old-owner", "old-root"); err != nil {
			return err
		}
		if err := writeBindingTestSkillRows(tx, source.UsageTargetActive, fixture.sourceFileID, 0, "old-owner", "", "old-root"); err != nil {
			return err
		}
		if withBuild {
			if _, err := tx.BeginOrResumeUsageBuild(UsageParserVersion); err != nil {
				return err
			}
			if err := writeBindingTestEvent(tx, source.UsageTargetBuild, "build-new-owner", "new-owner", "old-root", 0); err != nil {
				return err
			}
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	return fixture
}

func writeBindingTestEvent(tx *source.WriteTx, target source.UsageWriteTarget, eventID, threadID, rootID string, sourceFileID int64) error {
	cacheWrite := int64(1)
	event := sharedusage.CanonicalUsageEventWrite{
		EventID: eventID, Kind: sharedusage.EventKindNormal, OccurredAtMS: 10,
		ThreadID: threadID, RootSessionID: rootID, Model: "model", CreatedAtMS: 11,
		Usage: sharedusage.NormalizedTokenUsage{
			InputTokens: 2, CachedTokens: 1, CacheWriteTokens: &cacheWrite,
			OutputTokens: 1, ReasoningTokens: 0, TotalTokens: 3,
		},
	}
	if _, err := tx.WriteUsageNoRevision(target, event); err != nil {
		return err
	}
	if sourceFileID <= 0 {
		return nil
	}
	epoch, err := tx.ResolveUsageWriteEpoch(target)
	if err != nil {
		return err
	}
	return tx.Private(func(private storage.PrivateTx) error {
		_, err := private.Exec(`INSERT INTO codex_usage_event_occurrences(
			source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,event_id,created_at_ms
		) VALUES('codex',?,?,1,10,20,?,11)`, epoch, sourceFileID, eventID)
		return err
	})
}

func writeBindingTestSourceState(tx *source.WriteTx, target source.UsageWriteTarget, sourceFileID int64, owner, root string) error {
	carry, err := CanonicalReconciliationCarryJSON(NewReconciliationCarry())
	if err != nil {
		return err
	}
	return WriteSourceState(tx, target, SourceState{
		SourceFileID: sourceFileID, Generation: 1, DeviceID: 1, Inode: 2,
		UsageParserVersion: UsageParserVersion, CanonicalAlgorithmVersion: UsageCanonicalAlgorithmVersion,
		ResolvedThroughOffset: 64, ObservedRawSize: 128, RawTailStatus: RawTailUnverified,
		OwningThreadID: owner, RootSessionID: root, ContinuationState: ContinuationOwningLive,
		ChainState: ChainContinuous, UpdatedAtMS: 10, ReconciliationStateJSON: carry,
	})
}

func writeBindingTestMarker(tx *source.WriteTx, target source.UsageWriteTarget, sourceFileID int64, owner, root string) error {
	epoch, err := tx.ResolveUsageWriteEpoch(target)
	if err != nil {
		return err
	}
	return tx.Private(func(private storage.PrivateTx) error {
		_, err := private.Exec(`INSERT INTO codex_compaction_markers(
			ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
			owning_thread_id,root_session_id,unknown_reason
		) VALUES(?,?,1,30,40,?,?,'usage_missing')`, epoch, sourceFileID, owner, root)
		return err
	})
}

func writeBindingTestSkillRows(tx *source.WriteTx, target source.UsageWriteTarget, primaryID, foreignID int64, primaryOwner, foreignOwner, root string) error {
	epoch, err := tx.ResolveUsageWriteEpoch(target)
	if err != nil {
		return err
	}
	return tx.Private(func(private storage.PrivateTx) error {
		if primaryID > 0 {
			if _, err := private.Exec(`INSERT INTO codex_skill_usage_events(
				ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
				occurred_at_ms,thread_id,root_session_id,skill_name,created_at_ms
			) VALUES(?,?,1,50,60,10,?,?,'skill-primary',11)`, epoch, primaryID, primaryOwner, root); err != nil {
				return err
			}
		}
		if foreignID > 0 {
			_, err := private.Exec(`INSERT INTO codex_skill_usage_events(
				ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
				occurred_at_ms,thread_id,root_session_id,skill_name,created_at_ms
			) VALUES(?,?,1,50,60,10,?,?,'skill-foreign',11)`, epoch, foreignID, foreignOwner, root)
			return err
		}
		return nil
	})
}

func bindingTestProjection(tx *source.WriteTx, target source.UsageWriteTarget, owner string) (CompactionVisibilityProjection, error) {
	if target != source.UsageTargetActive {
		return CompactionVisibilityProjection{}, errors.New("binding projector received non-Active target")
	}
	epoch, err := tx.ResolveUsageWriteEpoch(target)
	if err != nil {
		return CompactionVisibilityProjection{}, err
	}
	projection := CompactionVisibilityProjection{Ready: true}
	err = tx.Private(func(private storage.PrivateTx) error {
		rows, err := private.Query(`SELECT root_session_id FROM codex_compaction_markers WHERE ledger_epoch=? AND owning_thread_id=? ORDER BY root_session_id`, epoch, owner)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var root string
			if err := rows.Scan(&root); err != nil {
				return err
			}
			projection.UnknownScopes = append(projection.UnknownScopes, CompactionUnknownScope{ThreadID: owner, RootSessionID: root})
		}
		return rows.Err()
	})
	return projection, err
}

func assertBindingAttribution(t *testing.T, run source.RunContext, sourceFileID int64, owner, root, eventID string) {
	t.Helper()
	var eventThread, eventRoot, stateOwner, stateRoot, markerOwner, markerRoot, skillOwner, skillRoot string
	err := run.Storage().PrivateRead(func(private storage.PrivateReader) error {
		if err := private.QueryRow(`SELECT thread_id,root_session_id FROM usage_events WHERE source='codex' AND event_id=?`, eventID).Scan(&eventThread, &eventRoot); err != nil {
			return err
		}
		if err := private.QueryRow(`SELECT owning_thread_id,root_session_id FROM codex_usage_source_states WHERE ledger_epoch=1 AND source_file_id=?`, sourceFileID).Scan(&stateOwner, &stateRoot); err != nil {
			return err
		}
		if err := private.QueryRow(`SELECT owning_thread_id,root_session_id FROM codex_compaction_markers WHERE ledger_epoch=1 AND source_file_id=?`, sourceFileID).Scan(&markerOwner, &markerRoot); err != nil {
			return err
		}
		return private.QueryRow(`SELECT thread_id,root_session_id FROM codex_skill_usage_events WHERE ledger_epoch=1 AND source_file_id=? ORDER BY skill_name LIMIT 1`, sourceFileID).Scan(&skillOwner, &skillRoot)
	})
	if err != nil {
		t.Fatal(err)
	}
	if eventThread != owner || eventRoot != root || stateOwner != owner || stateRoot != root ||
		markerOwner != owner || markerRoot != root || skillOwner != owner || skillRoot != root {
		t.Fatalf("canonical/private attribution diverged: event=(%s,%s) state=(%s,%s) marker=(%s,%s) skill=(%s,%s)",
			eventThread, eventRoot, stateOwner, stateRoot, markerOwner, markerRoot, skillOwner, skillRoot)
	}
}
