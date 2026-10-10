package codex

import (
	"bytes"
	"reflect"
	"testing"

	codexusage "github.com/Hogeexxl/Usagi/internal/codex/usage"
	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

func TestCompactionVisibilityProjectionReadyEventsAndUnknownMarkers(t *testing.T) {
	fixture := newQuarantineTestFixture(t, false)
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`UPDATE codex_source_files SET file_status='missing'`); err != nil {
				return err
			}
			return nil
		})
	}); err != nil {
		t.Fatal(err)
	}

	var unready codexusage.CompactionVisibilityProjection
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		var err error
		unready, err = compactionVisibilityProjection(reader, source.UsageTargetActive, 1, 11, nil)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if unready.Ready || len(unready.Events) != 0 || len(unready.UnknownScopes) != 0 {
		t.Fatalf("parser 11 compaction projection = %#v", unready)
	}

	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetActive, quarantineTestEvent("compaction-event", quarantineChildID, quarantineRootID)); err != nil {
			return err
		}
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`INSERT INTO codex_usage_event_facts(
				source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation
			) VALUES('codex',1,'compaction-event',?,'response','explicit','compaction')`, quarantineChildID); err != nil {
				return err
			}
			_, err := private.Exec(`INSERT INTO codex_compaction_markers(
				source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
				owning_thread_id,root_session_id,occurred_at_ms,model,reasoning_effort,response_id,resolved_event_id,unknown_reason
			) VALUES('codex',1,?,1,10,20,?,?,50,NULL,NULL,NULL,NULL,'usage_missing')`,
				fixture.memberFileID, quarantineChildID, quarantineRootID)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}

	var projection codexusage.CompactionVisibilityProjection
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		var err error
		projection, err = compactionVisibilityProjection(reader, source.UsageTargetActive, 1, 12, nil)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if !projection.Ready || len(projection.Events) != 1 || projection.Events[0].EventID != "compaction-event" || len(projection.UnknownScopes) != 1 {
		t.Fatalf("ready compaction projection = %#v", projection)
	}
	scope := projection.UnknownScopes[0]
	if scope.ThreadID != quarantineChildID || scope.RootSessionID != quarantineRootID || scope.Model != "model" ||
		scope.StartMS == nil || *scope.StartMS != 50 || scope.EndMS == nil || *scope.EndMS != 51 {
		t.Fatalf("unresolved marker scope = %#v", scope)
	}

	var owner, otherOwner codexusage.CompactionVisibilityProjection
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		otherEvent := quarantineTestEvent("other-owner", quarantineOtherRootID, quarantineOtherRootID)
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetActive, otherEvent); err != nil {
			return err
		}
		secondModel := quarantineTestEvent("second-model", quarantineChildID, quarantineRootID)
		secondModel.Model = "model2"
		effort := "high"
		secondModel.ReasoningEffort = &effort
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetActive, secondModel); err != nil {
			return err
		}
		if err := tx.Private(func(private storage.PrivateTx) error {
			if err := insertQuarantineLeak(private, fixture, "codex_turns", "manifest-only"); err != nil {
				return err
			}
			if _, err := private.Exec(`UPDATE codex_turns SET ledger_epoch=1,thread_id=?,started_at_ms=40,ended_at_ms=60 WHERE ledger_epoch=?`, quarantineChildID, fixture.buildEpoch); err != nil {
				return err
			}
			_, err := private.Exec(`INSERT INTO codex_compaction_markers(source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,root_session_id,model,unknown_reason)
				VALUES('codex',1,?,1,5,6,?,?,'model','usage_missing')`, fixture.memberFileID, quarantineChildID, quarantineRootID)
			return err
		}); err != nil {
			return err
		}
		var err error
		owner, err = MetadataCompactionVisibilityProjection(tx, source.UsageTargetActive, quarantineChildID)
		if err != nil {
			return err
		}
		otherOwner, err = MetadataCompactionVisibilityProjection(tx, source.UsageTargetActive, quarantineOtherRootID)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if len(owner.Events) != 1 || len(owner.UnknownScopes) != 2 || len(otherOwner.Events) != 0 || len(otherOwner.UnknownScopes) != 0 {
		t.Fatalf("owner-scoped projection owner=%#v other=%#v", owner, otherOwner)
	}
	merged, restricted := owner.UnknownScopes[0], owner.UnknownScopes[1]
	if merged.Model != "model" || merged.StartMS == nil || *merged.StartMS != 40 || merged.EndMS == nil || *merged.EndMS != 61 ||
		restricted.Model != "model2" || restricted.ReasoningEffort == nil || *restricted.ReasoningEffort != "high" || restricted.StartMS == nil || *restricted.StartMS != 50 || restricted.EndMS == nil || *restricted.EndMS != 51 {
		t.Fatalf("turn range, model restriction or stable merge = %#v", owner.UnknownScopes)
	}
}

func TestActiveCompactionProjectionUsesFrozenProofDuringBuild(t *testing.T) {
	fixture := newQuarantineTestFixture(t, false)
	carryJSON, err := codexusage.CanonicalReconciliationCarryJSON(codexusage.NewReconciliationCarry())
	if err != nil {
		t.Fatal(err)
	}
	var incomplete, complete, after, buildBefore, buildAfter codexusage.CompactionVisibilityProjection
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetActive, quarantineTestEvent("checkpoint-candidate", quarantineChildID, quarantineRootID)); err != nil {
			return err
		}
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetBuild, quarantineTestEvent("checkpoint-candidate", quarantineChildID, quarantineRootID)); err != nil {
			return err
		}
		if err := tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`UPDATE codex_source_files SET file_status='missing'`); err != nil {
				return err
			}
			if _, err := private.Exec(`UPDATE codex_source_files SET file_status='present' WHERE source_file_id=?`, fixture.memberFileID); err != nil {
				return err
			}
			return nil
		}); err != nil {
			return err
		}
		incomplete, err = ActiveCompactionVisibilityProjection(tx)
		if err != nil {
			return err
		}
		if err := tx.Private(func(private storage.PrivateTx) error {
			for _, epoch := range []int64{1, fixture.buildEpoch} {
				_, err := private.Exec(`INSERT INTO codex_usage_source_states(
				ledger_epoch,source_file_id,file_generation,device_id,inode,usage_parser_version,canonical_algorithm_version,
				resolved_through_offset,observed_raw_size,raw_tail_status,owning_thread_id,root_session_id,
				continuation_state,chain_state,updated_at_ms,reconciliation_state_json
			) VALUES(?,?,1,1,101,12,1,128,128,'none',?,?,'owning_live','continuous',20,?)`,
					epoch, fixture.memberFileID, quarantineChildID, quarantineRootID, string(carryJSON))
				if err != nil {
					return err
				}
			}
			return nil
		}); err != nil {
			return err
		}
		proof, err := ActiveSourceStateProofV3(tx, 1, fixture.memberFileID)
		if err != nil {
			return err
		}
		if err := tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_usage_build_sources SET active_committed_offset=128,
				active_guard_hash=?,active_state_fingerprint=? WHERE build_epoch=? AND source_file_id=?`,
				bytes.Repeat([]byte{7}, 32), proof, fixture.buildEpoch, fixture.memberFileID)
			return err
		}); err != nil {
			return err
		}
		complete, err = ActiveCompactionVisibilityProjection(tx)
		if err != nil {
			return err
		}
		if err := tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`INSERT INTO codex_source_checkpoints(
				source_file_id,consumer_kind,parser_version,committed_offset,processing_status
			) VALUES(?,'usage',12,0,'rebuild_required')`, fixture.memberFileID); err != nil {
				return err
			}
			return nil
		}); err != nil {
			return err
		}
		buildBefore, err = MetadataCompactionVisibilityProjection(tx, source.UsageTargetBuild, quarantineChildID)
		if err != nil {
			return err
		}
		if err := tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`UPDATE codex_source_checkpoints SET committed_offset=128,processing_status='ready'
				WHERE source_file_id=? AND consumer_kind='usage'`, fixture.memberFileID); err != nil {
				return err
			}
			return nil
		}); err != nil {
			return err
		}
		buildAfter, err = MetadataCompactionVisibilityProjection(tx, source.UsageTargetBuild, quarantineChildID)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		var err error
		after, err = ActiveCompactionVisibilityProjection(tx)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if !incomplete.Ready || len(incomplete.UnknownScopes) != 1 || incomplete.UnknownScopes[0].StartMS != nil || incomplete.UnknownScopes[0].EndMS != nil {
		t.Fatalf("Active projection did not scope incomplete source before frozen proof: %#v", incomplete)
	}
	if !complete.Ready || len(complete.UnknownScopes) != 0 || !reflect.DeepEqual(complete, after) {
		t.Fatalf("Active projection read working checkpoint during Build: complete=%#v after=%#v", complete, after)
	}
	if len(buildBefore.UnknownScopes) != 1 || len(buildAfter.UnknownScopes) != 0 {
		t.Fatalf("Build projection did not use working checkpoint: before=%#v after=%#v", buildBefore, buildAfter)
	}
	var catalogChanged codexusage.CompactionVisibilityProjection
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		if err := tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_source_files SET observed_size=129 WHERE source_file_id=?`, fixture.memberFileID)
			return err
		}); err != nil {
			return err
		}
		var err error
		catalogChanged, err = ActiveCompactionVisibilityProjection(tx)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if len(catalogChanged.UnknownScopes) != 1 || reflect.DeepEqual(complete, catalogChanged) {
		t.Fatalf("accepted catalog change did not affect Active projection: %#v", catalogChanged)
	}
	for _, test := range []struct {
		name         string
		offset       int64
		guard        []byte
		wantComplete bool
	}{
		{"positive-null", 128, nil, false},
		{"positive-short", 128, bytes.Repeat([]byte{7}, 31), false},
		{"positive-valid", 128, bytes.Repeat([]byte{7}, 32), true},
		{"zero-null", 0, nil, true},
		{"zero-nonnull", 0, bytes.Repeat([]byte{7}, 32), false},
	} {
		t.Run(test.name, func(t *testing.T) {
			var projection codexusage.CompactionVisibilityProjection
			if err := fixture.storage.Write(func(tx *source.WriteTx) error {
				if err := tx.Private(func(private storage.PrivateTx) error {
					if _, err := private.Exec(`UPDATE codex_source_checkpoints SET committed_offset=0,guard_hash=NULL WHERE source_file_id=? AND consumer_kind='usage'`, fixture.memberFileID); err != nil {
						return err
					}
					if _, err := private.Exec(`UPDATE codex_source_files SET observed_size=? WHERE source_file_id=?`, test.offset, fixture.memberFileID); err != nil {
						return err
					}
					if _, err := private.Exec(`UPDATE codex_usage_source_states SET resolved_through_offset=?,observed_raw_size=? WHERE ledger_epoch=1 AND source_file_id=?`, test.offset, test.offset, fixture.memberFileID); err != nil {
						return err
					}
					var workingGuard []byte
					if test.offset > 0 {
						workingGuard = bytes.Repeat([]byte{9}, 32)
					}
					_, err := private.Exec(`UPDATE codex_source_checkpoints SET committed_offset=?,guard_hash=?,processing_status='ready' WHERE source_file_id=? AND consumer_kind='usage'`, test.offset, workingGuard, fixture.memberFileID)
					return err
				}); err != nil {
					return err
				}
				proof, err := ActiveSourceStateProofV3(tx, 1, fixture.memberFileID)
				if err != nil {
					return err
				}
				if err := tx.Private(func(private storage.PrivateTx) error {
					_, err := private.Exec(`UPDATE codex_usage_build_sources SET active_committed_offset=?,active_guard_hash=?,active_state_fingerprint=? WHERE build_epoch=? AND source_file_id=?`, test.offset, test.guard, proof, fixture.buildEpoch, fixture.memberFileID)
					return err
				}); err != nil {
					return err
				}
				projection, err = ActiveCompactionVisibilityProjection(tx)
				return err
			}); err != nil {
				t.Fatal(err)
			}
			wantScopes := 1
			if test.wantComplete {
				wantScopes = 0
			}
			if !projection.Ready || len(projection.UnknownScopes) != wantScopes {
				t.Fatalf("frozen guard projection with ready working checkpoint = %#v; want %d unknown scopes", projection, wantScopes)
			}
		})
	}
}

func TestVisiblePrivateEqualComparesSkillQuarantineAndCompaction(t *testing.T) {
	fixture := newQuarantineTestFixture(t, false)
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetActive, quarantineTestEvent("visible-compaction", quarantineChildID, quarantineRootID)); err != nil {
			return err
		}
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetBuild, quarantineTestEvent("visible-compaction", quarantineChildID, quarantineRootID)); err != nil {
			return err
		}
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`UPDATE codex_source_files SET file_status='missing'`); err != nil {
				return err
			}
			for _, epoch := range []int64{1, fixture.buildEpoch} {
				if _, err := private.Exec(`INSERT INTO codex_usage_event_facts(
					source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation
				) VALUES('codex',?,'visible-compaction',?,'response','explicit','compaction')`, epoch, quarantineChildID); err != nil {
					return err
				}
				if _, err := private.Exec(`INSERT INTO codex_skill_usage_events(
					ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,occurred_at_ms,
					thread_id,root_session_id,model,skill_name,created_at_ms
				) VALUES(?,?,1,0,20,50,?,?,NULL,'skill',?)`, epoch, fixture.memberFileID, quarantineChildID, quarantineRootID, epoch+30); err != nil {
					return err
				}
				if _, err := private.Exec(`INSERT INTO codex_usage_session_quarantine(
					ledger_epoch,root_session_id,primary_error_code,last_activity_at_ms,first_seen_at_ms,updated_at_ms
				) VALUES(?,?,'RESPONSE_USAGE_CONFLICT',77,?,?)`, epoch, quarantineRootID, epoch+40, epoch+50); err != nil {
					return err
				}
			}
			return nil
		})
	}); err != nil {
		t.Fatal(err)
	}

	compare := func(tx *source.WriteTx) (bool, error) {
		var equal bool
		err := tx.Private(func(private storage.PrivateTx) error {
			var err error
			equal, err = VisiblePrivateEqual(private, domain.SourceCodex, 1, 12, fixture.buildEpoch, 12)
			return err
		})
		return equal, err
	}
	var equal bool
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		var err error
		equal, err = compare(tx)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if !equal {
		t.Fatal("equal private projections with different write timestamps compared unequal")
	}

	for _, mutate := range []func(storage.PrivateTx) error{
		func(private storage.PrivateTx) error {
			_, err := private.Exec(`INSERT INTO codex_skill_usage_events(
				ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,occurred_at_ms,
				thread_id,root_session_id,model,skill_name,created_at_ms
			) VALUES(?,?,1,20,40,50,?,?,NULL,'skill',90)`, fixture.buildEpoch, fixture.memberFileID, quarantineChildID, quarantineRootID)
			return err
		},
		func(private storage.PrivateTx) error {
			if _, err := private.Exec(`DELETE FROM codex_skill_usage_events WHERE ledger_epoch=? AND source_start_offset=20`, fixture.buildEpoch); err != nil {
				return err
			}
			_, err := private.Exec(`UPDATE codex_usage_session_quarantine SET last_activity_at_ms=78 WHERE ledger_epoch=? AND root_session_id=?`, fixture.buildEpoch, quarantineRootID)
			return err
		},
		func(private storage.PrivateTx) error {
			if _, err := private.Exec(`UPDATE codex_usage_session_quarantine SET last_activity_at_ms=77 WHERE ledger_epoch=? AND root_session_id=?`, fixture.buildEpoch, quarantineRootID); err != nil {
				return err
			}
			_, err := private.Exec(`INSERT INTO codex_compaction_markers(
				source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
				owning_thread_id,root_session_id,occurred_at_ms,model,reasoning_effort,response_id,resolved_event_id,unknown_reason
			) VALUES('codex',?,?,1,40,50,?,?,50,NULL,NULL,NULL,NULL,'usage_missing')`,
				fixture.buildEpoch, fixture.memberFileID, quarantineChildID, quarantineRootID)
			return err
		},
		func(private storage.PrivateTx) error {
			if _, err := private.Exec(`DELETE FROM codex_compaction_markers WHERE ledger_epoch=?`, fixture.buildEpoch); err != nil {
				return err
			}
			_, err := private.Exec(`UPDATE codex_usage_event_facts SET operation='response' WHERE ledger_epoch=?`, fixture.buildEpoch)
			return err
		},
	} {
		if err := fixture.storage.Write(func(tx *source.WriteTx) error {
			if err := tx.Private(mutate); err != nil {
				return err
			}
			var err error
			equal, err = compare(tx)
			return err
		}); err != nil {
			t.Fatal(err)
		}
		if equal {
			t.Fatal("private visibility projection change compared equal")
		}
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`DELETE FROM codex_usage_event_facts`); err != nil {
				return err
			}
			var err error
			equal, err = VisiblePrivateEqual(private, domain.SourceCodex, 1, 12, fixture.buildEpoch, 11)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	if equal {
		t.Fatal("different compaction Ready state compared equal")
	}
}

func TestActiveSourceStateProofV3CanonicalAndPrivateEvidence(t *testing.T) {
	fixture := newQuarantineTestFixture(t, false)
	carryJSON, err := codexusage.CanonicalReconciliationCarryJSON(codexusage.NewReconciliationCarry())
	if err != nil {
		t.Fatal(err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`INSERT INTO codex_usage_source_states(
				ledger_epoch,source_file_id,file_generation,device_id,inode,usage_parser_version,canonical_algorithm_version,
				resolved_through_offset,observed_raw_size,raw_tail_status,owning_thread_id,root_session_id,
				continuation_state,chain_state,updated_at_ms,reconciliation_state_json
			) VALUES(1,?,1,1,101,12,1,128,128,'none',?,?,'owning_live','continuous',20,?)`,
				fixture.memberFileID, quarantineChildID, quarantineRootID, string(carryJSON))
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	proof := func() ([]byte, error) {
		var value []byte
		err := fixture.storage.Write(func(tx *source.WriteTx) error {
			var err error
			value, err = ActiveSourceStateProofV3(tx, 1, fixture.memberFileID)
			return err
		})
		return value, err
	}
	first, err := proof()
	if err != nil || len(first) != 32 {
		t.Fatalf("initial proof length=%d err=%v", len(first), err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_usage_source_states SET updated_at_ms=99 WHERE ledger_epoch=1 AND source_file_id=?`, fixture.memberFileID)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	updatedAtOnly, err := proof()
	if err != nil || !bytes.Equal(first, updatedAtOnly) {
		t.Fatalf("updated_at_ms changed proof: err=%v", err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetActive, quarantineTestEvent("proof-private-event", quarantineChildID, quarantineRootID)); err != nil {
			return err
		}
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`INSERT INTO codex_usage_event_occurrences(
				source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,event_id,created_at_ms
			) VALUES('codex',1,?,1,0,20,'proof-private-event',10)`, fixture.memberFileID)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	withPrivateEvidence, err := proof()
	if err != nil || bytes.Equal(updatedAtOnly, withPrivateEvidence) {
		t.Fatalf("private evidence did not change proof: err=%v", err)
	}
	windowJSON, err := codexusage.CanonicalLegacyReconciliationWindowJSON(codexusage.LegacyReconciliationWindow{Version: 1, ChainState: codexusage.LegacyWindowChainState{Kind: "continuous"}})
	if err != nil {
		t.Fatal(err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`INSERT INTO codex_usage_reconciliation_windows(source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,state_json)
				VALUES('codex',1,?,1,0,20,?,?)`, fixture.memberFileID, quarantineChildID, string(windowJSON))
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	withWindow, err := proof()
	if err != nil || bytes.Equal(withWindow, withPrivateEvidence) {
		t.Fatalf("canonical window did not change proof: err=%v", err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_usage_reconciliation_windows SET state_json=? WHERE ledger_epoch=1`, string(windowJSON)+" ")
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := proof(); err == nil {
		t.Fatal("noncanonical legacy reconciliation window produced a state proof")
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`DELETE FROM codex_usage_reconciliation_windows WHERE ledger_epoch=1`); err != nil {
				return err
			}
			if err := insertQuarantineLeak(private, fixture, "codex_turns", "manifest-only"); err != nil {
				return err
			}
			_, err := private.Exec(`UPDATE codex_turns SET ledger_epoch=1,thread_id=? WHERE ledger_epoch=?`, quarantineChildID, fixture.buildEpoch)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	withTurn, err := proof()
	if err != nil || bytes.Equal(withTurn, withPrivateEvidence) {
		t.Fatalf("turn evidence did not change proof: err=%v", err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_turns SET updated_at_ms=100 WHERE ledger_epoch=1`)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	turnUpdated, err := proof()
	if err != nil || !bytes.Equal(withTurn, turnUpdated) {
		t.Fatalf("turn updated_at_ms changed proof: err=%v", err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_usage_source_states SET reconciliation_state_json=? WHERE ledger_epoch=1 AND source_file_id=?`, string(carryJSON)+" ", fixture.memberFileID)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := proof(); err == nil {
		t.Fatal("noncanonical carry JSON produced a state proof")
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		if err := tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`UPDATE codex_usage_source_states SET reconciliation_state_json=? WHERE ledger_epoch=1 AND source_file_id=?`, string(carryJSON), fixture.memberFileID); err != nil {
				return err
			}
			_, err := private.Exec(`INSERT INTO codex_usage_reconciliation_windows(
				source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,state_json
			) VALUES('codex',1,?,1,0,20,?,'{}')`, fixture.memberFileID, quarantineChildID)
			return err
		}); err != nil {
			return err
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := proof(); err == nil {
		t.Fatal("invalid legacy reconciliation window produced a state proof")
	}
}

func TestActiveSourceStateProofV3ChangesWithState(t *testing.T) {
	fixture := newQuarantineTestFixture(t, false)
	carryJSON, err := codexusage.CanonicalReconciliationCarryJSON(codexusage.NewReconciliationCarry())
	if err != nil {
		t.Fatal(err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`INSERT INTO codex_usage_source_states(
				ledger_epoch,source_file_id,file_generation,device_id,inode,usage_parser_version,canonical_algorithm_version,
				resolved_through_offset,observed_raw_size,raw_tail_status,owning_thread_id,root_session_id,
				continuation_state,chain_state,updated_at_ms,reconciliation_state_json
			) VALUES(1,?,1,1,101,12,1,128,128,'none',?,?,'owning_live','continuous',20,?)`,
				fixture.memberFileID, quarantineChildID, quarantineRootID, string(carryJSON))
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	var before, after []byte
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		var err error
		before, err = ActiveSourceStateProofV3(tx, 1, fixture.memberFileID)
		if err != nil {
			return err
		}
		if err := tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_usage_source_states SET continuation_state='replayed_ancestor' WHERE ledger_epoch=1 AND source_file_id=?`, fixture.memberFileID)
			return err
		}); err != nil {
			return err
		}
		after, err = ActiveSourceStateProofV3(tx, 1, fixture.memberFileID)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if len(before) != 32 || len(after) != 32 || bytes.Equal(before, after) {
		t.Fatalf("state change did not change 32-byte proof: before=%x after=%x", before, after)
	}
}
