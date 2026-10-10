package codex

import (
	"errors"
	"fmt"
	"strings"
	"testing"

	codexusage "github.com/Hogeexxl/Usagi/internal/codex/usage"
	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

const (
	quarantineRootID      = "01900000-0000-7000-8000-000000000000"
	quarantineChildID     = "01900000-0000-7000-8000-000000000001"
	quarantineOtherRootID = "01900000-0000-7000-8000-000000000002"
)

type quarantineTestFixture struct {
	db            *storage.DB
	storage       *source.Storage
	buildEpoch    int64
	memberFileID  int64
	carriedFileID int64
	missingFileID int64
	otherFileID   int64
	proof         codexusage.QuarantineSourceProof
	presentProofs []codexusage.QuarantineSourceProof
}

func newQuarantineTestFixture(t *testing.T, activeQuarantine bool) quarantineTestFixture {
	t.Helper()
	db, run := newCodexTestStorage(t)
	fixture := quarantineTestFixture{storage: run}
	err := run.Write(func(tx *source.WriteTx) error {
		for _, item := range []struct{ id, root, parent, role string }{
			{quarantineRootID, quarantineRootID, "", "main"},
			{quarantineChildID, quarantineRootID, quarantineRootID, "subagent"},
			{quarantineOtherRootID, quarantineOtherRootID, "", "main"},
		} {
			identity, err := domain.NewSessionIdentity(item.id, domain.SourceCodex, "native:"+item.id)
			if err != nil {
				return err
			}
			patch, err := domain.NewResolvedThreadPatch(identity, 1)
			if err != nil {
				return err
			}
			patch.RootSessionID = domain.Set(item.root)
			patch.AgentRole = domain.Set(domain.AgentRole(item.role))
			if item.parent != "" {
				patch.ParentThreadID = domain.Set(item.parent)
			}
			patch.UpdatedAtMS = domain.Set(int64(100))
			if _, err := tx.UpsertThreadNoRevision(identity, patch); err != nil {
				return err
			}
		}
		if err := tx.EnsureUsageEpoch(); err != nil {
			return err
		}
		firstEpoch, err := tx.BeginOrResumeUsageBuild(12)
		if err != nil {
			return err
		}
		if _, err := tx.ActivateUsageBuild(firstEpoch, 12); err != nil {
			return err
		}
		fixture.buildEpoch, err = tx.BeginOrResumeUsageBuild(12)
		if err != nil {
			return err
		}
		return tx.Private(func(private storage.PrivateTx) error {
			for _, file := range []struct {
				path   string
				inode  int64
				status string
			}{
				{"/codex/quarantine-member.jsonl", 101, "present"},
				{"/codex/quarantine-carried.jsonl", 102, "present"},
				{"/codex/quarantine-missing.jsonl", 103, "missing"},
				{"/codex/quarantine-other.jsonl", 104, "present"},
			} {
				result, err := private.Exec(`INSERT INTO codex_source_files(
					thread_id,current_path,source_area,device_id,inode,file_generation,observed_size,
					observed_mtime_ns,file_status,last_seen_at_ms
				) VALUES(? ,?,'sessions',1,?,1,128,3,?,4)`, quarantineChildID, file.path, file.inode, file.status)
				if err != nil {
					return err
				}
				id, err := result.LastInsertId()
				if err != nil {
					return err
				}
				if file.inode == 101 {
					fixture.memberFileID = id
					fixture.proof = codexusage.QuarantineSourceProof{SourceFileID: id, Generation: 1, DeviceID: 1, Inode: file.inode, ObservedSize: 128}
					fixture.presentProofs = append(fixture.presentProofs, fixture.proof)
				} else if file.inode == 102 {
					fixture.carriedFileID = id
					fixture.presentProofs = append(fixture.presentProofs, codexusage.QuarantineSourceProof{SourceFileID: id, Generation: 1, DeviceID: 1, Inode: file.inode, ObservedSize: 128})
				} else if file.inode == 103 {
					fixture.missingFileID = id
				} else {
					fixture.otherFileID = id
				}
			}
			members := []struct {
				fileID int64
				inode  int64
				status string
			}{
				{fixture.memberFileID, 101, "rebuilt"},
				{fixture.carriedFileID, 102, "carried"},
				{fixture.missingFileID, 103, "blocked"},
			}
			for _, member := range members {
				var completionError, completedGeneration, completedThrough, carryFrom any
				carryPhase := "none"
				switch member.status {
				case "rebuilt":
					completedGeneration, completedThrough = int64(1), int64(128)
				case "carried":
					completedGeneration, completedThrough, carryFrom, carryPhase = int64(1), int64(128), int64(1), "finalize"
				case "blocked":
					completionError = "RAW_MISSING"
				}
				if _, err := private.Exec(`INSERT INTO codex_usage_build_sources(
					build_epoch,source_file_id,target_parser_version,expected_file_generation,expected_device_id,expected_inode,
					expected_owning_thread_id,expected_root_session_id,active_committed_offset,active_guard_hash,active_state_fingerprint,
					required_generation,required_through_offset,observed_raw_size,raw_tail_status,raw_tail_start_offset,
					membership_reason,completion_status,completion_error_code,completed_generation,completed_through_offset,
					carry_from_epoch,carry_phase,carry_after_start_offset,carry_after_turn_key,carry_after_anomaly_id,
					carry_after_fact_event_id,carry_after_marker_start_offset,carry_after_window_start_offset,created_at_ms,updated_at_ms
				) VALUES(?,?,12,1,1,?,?,?,0,NULL,NULL,1,128,128,'none',NULL,'present_at_build_start',?,?,?,?,?,? ,NULL,NULL,NULL,NULL,NULL,NULL,5,6)`,
					fixture.buildEpoch, member.fileID, member.inode, quarantineChildID, quarantineRootID, member.status,
					completionError, completedGeneration, completedThrough, carryFrom, carryPhase); err != nil {
					return err
				}
			}
			if activeQuarantine {
				if _, err := private.Exec(`INSERT INTO codex_usage_session_quarantine(
					ledger_epoch,root_session_id,primary_error_code,last_activity_at_ms,first_seen_at_ms,updated_at_ms
				) VALUES(1,?,'RESPONSE_USAGE_CONFLICT',777,650,700)`, quarantineRootID); err != nil {
					return err
				}
				for _, proof := range fixture.presentProofs {
					if _, err := private.Exec(`INSERT INTO codex_usage_session_quarantine_sources(
						ledger_epoch,root_session_id,source_file_id,file_generation,device_id,inode,observed_size,updated_at_ms
					) VALUES(1,?,?,1,1,?,?,700)`, quarantineRootID, proof.SourceFileID, proof.Inode, proof.ObservedSize); err != nil {
						return err
					}
				}
			}
			return nil
		})
	})
	if err != nil {
		t.Fatal(err)
	}
	fixture.db = db
	return fixture
}

func TestVerifyQuarantinedRootCleanChecksManifestAndRootIndependently(t *testing.T) {
	for _, table := range []string{"codex_turns", "codex_usage_source_states", "codex_skill_usage_events"} {
		for _, scope := range []string{"root-only", "manifest-only"} {
			t.Run(table+"/"+scope, func(t *testing.T) {
				fixture := newQuarantineTestFixture(t, false)
				if err := fixture.storage.Write(func(tx *source.WriteTx) error {
					return tx.Private(func(private storage.PrivateTx) error {
						return insertQuarantineLeak(private, fixture, table, scope)
					})
				}); err != nil {
					t.Fatal(err)
				}
				if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
					return VerifyQuarantinedRootClean(reader, fixture.buildEpoch, quarantineRootID, nil)
				}); err == nil {
					t.Fatal("no-leak verifier accepted a residue in one independent scope")
				}
			})
		}
	}
}

func TestVerifyQuarantinedRootCleanChecksEverySemanticTable(t *testing.T) {
	for _, table := range []string{"usage_events", "codex_usage_event_occurrences", "codex_usage_event_facts",
		"codex_compaction_markers", "codex_usage_reconciliation_windows", "codex_usage_event_holds",
		"codex_turns", "codex_usage_source_states", "codex_skill_usage_events"} {
		t.Run(table, func(t *testing.T) {
			fixture := newQuarantineTestFixture(t, false)
			if err := fixture.storage.Write(func(tx *source.WriteTx) error {
				threadID, rootID := quarantineOtherRootID, quarantineOtherRootID
				if table == "usage_events" {
					rootID = quarantineRootID
				}
				if table == "codex_usage_event_facts" {
					threadID = quarantineChildID
				}
				if _, err := tx.WriteUsageNoRevision(source.UsageTargetBuild, quarantineTestEvent("table-residue", threadID, rootID)); err != nil {
					return err
				}
				return tx.Private(func(private storage.PrivateTx) error {
					var err error
					switch table {
					case "usage_events":
					case "codex_usage_event_occurrences":
						_, err = private.Exec(`INSERT INTO codex_usage_event_occurrences(source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,event_id,created_at_ms)
							VALUES('codex',?,?,1,0,20,'table-residue',8)`, fixture.buildEpoch, fixture.memberFileID)
					case "codex_usage_event_facts":
						_, err = private.Exec(`INSERT INTO codex_usage_event_facts(source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation)
							VALUES('codex',?,'table-residue',?,'response','explicit','response')`, fixture.buildEpoch, threadID)
					case "codex_compaction_markers":
						_, err = private.Exec(`INSERT INTO codex_compaction_markers(source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,root_session_id,unknown_reason)
							VALUES('codex',?,?,1,0,20,?,?,'usage_missing')`, fixture.buildEpoch, fixture.memberFileID, threadID, rootID)
					case "codex_usage_reconciliation_windows":
						var encoded []byte
						encoded, err = codexusage.CanonicalLegacyReconciliationWindowJSON(codexusage.LegacyReconciliationWindow{Version: 1, ChainState: codexusage.LegacyWindowChainState{Kind: "continuous"}})
						if err != nil {
							return err
						}
						_, err = private.Exec(`INSERT INTO codex_usage_reconciliation_windows(source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,state_json)
							VALUES('codex',?,?,1,0,20,?,?)`, fixture.buildEpoch, fixture.memberFileID, threadID, string(encoded))
					case "codex_usage_event_holds":
						_, err = private.Exec(`INSERT INTO codex_usage_event_holds(source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason)
							VALUES('codex',?,?,1,'table-residue','replay')`, fixture.buildEpoch, fixture.memberFileID)
					default:
						return insertQuarantineLeak(private, fixture, table, "manifest-only")
					}
					return err
				})
			}); err != nil {
				t.Fatal(err)
			}
			if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
				return VerifyQuarantinedRootClean(reader, fixture.buildEpoch, quarantineRootID, nil)
			}); err == nil || !strings.Contains(err.Error(), "retains "+table) {
				t.Fatalf("no-leak check for %s = %v", table, err)
			}
		})
	}
}

func TestQuarantineRootAndReuseRejectStaleProofs(t *testing.T) {
	for _, reuse := range []bool{false, true} {
		for _, change := range []string{"missing-proof", "extra-proof", "historical-proof", "duplicate-proof", "generation", "device", "inode", "size", "accepted-db", "manifest", "build-epoch", "replaced"} {
			t.Run(fmt.Sprintf("reuse=%v/%s", reuse, change), func(t *testing.T) {
				fixture := newQuarantineTestFixture(t, reuse)
				if err := seedQuarantineLeaks(fixture); err != nil {
					t.Fatal(err)
				}
				proofs := append([]codexusage.QuarantineSourceProof(nil), fixture.presentProofs...)
				epoch := fixture.buildEpoch
				switch change {
				case "missing-proof":
					proofs = proofs[:1]
				case "extra-proof":
					proofs = append(proofs, codexusage.QuarantineSourceProof{SourceFileID: fixture.otherFileID, Generation: 1, DeviceID: 1, Inode: 104, ObservedSize: 128})
				case "historical-proof":
					proofs = append(proofs, codexusage.QuarantineSourceProof{SourceFileID: fixture.missingFileID, Generation: 1, DeviceID: 1, Inode: 103, ObservedSize: 128})
				case "duplicate-proof":
					proofs = append(proofs, proofs[0])
				case "generation":
					proofs[0].Generation++
				case "device":
					proofs[0].DeviceID++
				case "inode":
					proofs[0].Inode++
				case "size":
					proofs[0].ObservedSize++
				case "build-epoch":
					epoch++
				}
				if err := fixture.storage.Write(func(tx *source.WriteTx) error {
					return tx.Private(func(private storage.PrivateTx) error {
						var err error
						switch change {
						case "accepted-db":
							_, err = private.Exec(`UPDATE codex_source_files SET observed_size=129 WHERE source_file_id=?`, fixture.memberFileID)
						case "manifest":
							_, err = private.Exec(`UPDATE codex_usage_build_sources SET expected_inode=999 WHERE build_epoch=? AND source_file_id=?`, fixture.buildEpoch, fixture.memberFileID)
						case "replaced":
							_, err = private.Exec(`UPDATE codex_source_files SET file_status='replaced' WHERE source_file_id=?`, fixture.memberFileID)
						}
						return err
					})
				}); err != nil {
					t.Fatal(err)
				}
				err := fixture.storage.Write(func(tx *source.WriteTx) error {
					if reuse {
						return ReuseQuarantineRoot(tx, epoch, quarantineRootID, proofs, 900)
					}
					return QuarantineRoot(tx, epoch, quarantineRootID, codexusage.FatalConflict{Code: codexusage.FatalResponseUsage}, proofs, 900)
				})
				if !errors.Is(err, ErrStaleQuarantineProof) {
					t.Fatalf("proof CAS error = %v", err)
				}
				var rows, terminal int
				if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
					if err := reader.QueryRow(`SELECT COUNT(*) FROM codex_skill_usage_events WHERE ledger_epoch=?`, fixture.buildEpoch).Scan(&rows); err != nil {
						return err
					}
					return reader.QueryRow(`SELECT COUNT(*) FROM codex_usage_build_sources WHERE build_epoch=? AND completion_status='quarantined'`, fixture.buildEpoch).Scan(&terminal)
				}); err != nil {
					t.Fatal(err)
				}
				if rows != 2 || terminal != 0 {
					t.Fatalf("stale proof changed rows=%d terminal=%d", rows, terminal)
				}
			})
		}
	}
}

func TestQuarantineRootCleansIndependentScopesAndEventDependencies(t *testing.T) {
	fixture := newQuarantineTestFixture(t, false)
	if err := seedQuarantineLeaks(fixture); err != nil {
		t.Fatal(err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		for _, event := range []sharedusage.CanonicalUsageEventWrite{
			quarantineTestEvent("hold-only", quarantineOtherRootID, quarantineOtherRootID),
			quarantineTestEvent("resolved-compaction", quarantineChildID, quarantineRootID),
			quarantineTestEvent("root-owned-fact", quarantineChildID, quarantineOtherRootID),
		} {
			if _, err := tx.WriteUsageNoRevision(source.UsageTargetBuild, event); err != nil {
				return err
			}
		}
		return tx.Private(func(private storage.PrivateTx) error {
			if _, err := private.Exec(`INSERT INTO codex_usage_event_holds(
				source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason
			) VALUES('codex',?,?,?,?, 'replay')`, fixture.buildEpoch, fixture.memberFileID, 1, "hold-only"); err != nil {
				return err
			}
			if _, err := private.Exec(`INSERT INTO codex_usage_event_facts(
				source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation
			) VALUES('codex',?,? ,?,'resolved-response','explicit','compaction')`, fixture.buildEpoch, "resolved-compaction", quarantineChildID); err != nil {
				return err
			}
			if _, err := private.Exec(`INSERT INTO codex_compaction_markers(
				source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
				owning_thread_id,root_session_id,occurred_at_ms,model,response_id,resolved_event_id,unknown_reason
			) VALUES('codex',?,?,?,?,?,?,?,10,'model','resolved-response','resolved-compaction',NULL)`,
				fixture.buildEpoch, fixture.memberFileID, 1, 0, 20, quarantineChildID, quarantineRootID); err != nil {
				return err
			}
			if _, err := private.Exec(`INSERT INTO codex_usage_event_facts(
				source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation
			) VALUES('codex',?,? ,?,'root-fact-response','explicit','response')`, fixture.buildEpoch, "root-owned-fact", quarantineChildID); err != nil {
				return err
			}
			return nil
		})
	}); err != nil {
		t.Fatal(err)
	}

	beforeRevision := fixture.db.CurrentRevision().DataRevision
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return QuarantineRoot(tx, fixture.buildEpoch, quarantineRootID,
			codexusage.FatalConflict{Code: codexusage.FatalResponseUsage}, fixture.presentProofs, 900)
	}); err != nil {
		t.Fatal(err)
	}
	if got := fixture.db.CurrentRevision().DataRevision; got != beforeRevision {
		t.Fatalf("Build quarantine changed data_revision from %d to %d", beforeRevision, got)
	}
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		return VerifyQuarantinedRootClean(reader, fixture.buildEpoch, quarantineRootID, []string{"hold-only", "resolved-compaction", "root-owned-fact"})
	}); err != nil {
		t.Fatal(err)
	}
	for _, eventID := range []string{"hold-only", "resolved-compaction", "root-owned-fact"} {
		var count int
		if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
			return reader.QueryRow(`SELECT COUNT(*) FROM usage_events WHERE source='codex' AND source_epoch=? AND event_id=?`, fixture.buildEpoch, eventID).Scan(&count)
		}); err != nil {
			t.Fatal(err)
		}
		if count != 0 {
			t.Fatalf("event %q survived quarantine", eventID)
		}
	}
	var code, completion string
	var activity int64
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		if err := reader.QueryRow(`SELECT primary_error_code,last_activity_at_ms FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?`, fixture.buildEpoch, quarantineRootID).Scan(&code, &activity); err != nil {
			return err
		}
		return reader.QueryRow(`SELECT completion_status FROM codex_usage_build_sources WHERE build_epoch=? AND source_file_id=?`, fixture.buildEpoch, fixture.memberFileID).Scan(&completion)
	}); err != nil {
		t.Fatal(err)
	}
	if code != string(codexusage.FatalResponseUsage) || activity != 100 || completion != "quarantined" {
		t.Fatalf("quarantine projection code=%q activity=%d completion=%q", code, activity, completion)
	}
	assertQuarantinedManifest(t, fixture)
}

func TestReuseQuarantineRootCleansIndependentScopesWithoutRevisionChange(t *testing.T) {
	fixture := newQuarantineTestFixture(t, true)
	if err := seedQuarantineLeaks(fixture); err != nil {
		t.Fatal(err)
	}
	beforeRevision := fixture.db.CurrentRevision().DataRevision
	var privateEqual bool
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		if err := ReuseQuarantineRoot(tx, fixture.buildEpoch, quarantineRootID, fixture.presentProofs, 901); err != nil {
			return err
		}
		return tx.Private(func(private storage.PrivateTx) error {
			var err error
			privateEqual, err = VisiblePrivateEqual(private, domain.SourceCodex, 1, 12, fixture.buildEpoch, 12)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	if !privateEqual {
		t.Fatal("reused quarantine changed the private visibility projection")
	}
	if got := fixture.db.CurrentRevision().DataRevision; got != beforeRevision {
		t.Fatalf("quarantine reuse changed data_revision from %d to %d", beforeRevision, got)
	}
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		return VerifyQuarantinedRootClean(reader, fixture.buildEpoch, quarantineRootID, nil)
	}); err != nil {
		t.Fatal(err)
	}
	var code, completion, carryPhase string
	var activity, firstSeen, updated int64
	var completedGeneration, completedOffset, carryFrom any
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		if err := reader.QueryRow(`SELECT primary_error_code,last_activity_at_ms,first_seen_at_ms,updated_at_ms FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?`, fixture.buildEpoch, quarantineRootID).Scan(&code, &activity, &firstSeen, &updated); err != nil {
			return err
		}
		return reader.QueryRow(`SELECT completion_status,completed_generation,completed_through_offset,carry_from_epoch,carry_phase FROM codex_usage_build_sources WHERE build_epoch=? AND source_file_id=?`, fixture.buildEpoch, fixture.memberFileID).Scan(&completion, &completedGeneration, &completedOffset, &carryFrom, &carryPhase)
	}); err != nil {
		t.Fatal(err)
	}
	if code != string(codexusage.FatalResponseUsage) || activity != 777 || firstSeen != 650 || updated != 901 ||
		completion != "quarantined" || completedGeneration != nil || completedOffset != nil || carryFrom != nil || carryPhase != "none" {
		t.Fatalf("reused projection q=(%q,%d,%d,%d) manifest=(%q,%v,%v,%v,%q)", code, activity, firstSeen, updated, completion, completedGeneration, completedOffset, carryFrom, carryPhase)
	}
	assertQuarantinedManifest(t, fixture)
}

func TestQuarantineRootAcceptsOnlyMissingHistoricalMembersWithoutProof(t *testing.T) {
	fixture := newQuarantineTestFixture(t, false)
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_source_files SET file_status='missing' WHERE source_file_id IN (?,?)`, fixture.memberFileID, fixture.carriedFileID)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return QuarantineRoot(tx, fixture.buildEpoch, quarantineRootID,
			codexusage.FatalConflict{Code: codexusage.FatalResponseUsage}, nil, 900)
	}); err != nil {
		t.Fatal(err)
	}
	var proofCount int
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow(`SELECT COUNT(*) FROM codex_usage_session_quarantine_sources WHERE ledger_epoch=? AND root_session_id=?`, fixture.buildEpoch, quarantineRootID).Scan(&proofCount)
	}); err != nil {
		t.Fatal(err)
	}
	if proofCount != 0 {
		t.Fatalf("missing historical members received %d quarantine proofs", proofCount)
	}
	assertQuarantinedManifest(t, fixture)
}

func TestQuarantineStillValidRequiresExactCurrentProofSet(t *testing.T) {
	fixture := newQuarantineTestFixture(t, true)
	var valid, changed bool
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		var err error
		valid, err = QuarantineStillValid(1, quarantineRootID, fixture.presentProofs, reader)
		if err != nil {
			return err
		}
		stale := append([]codexusage.QuarantineSourceProof(nil), fixture.presentProofs...)
		stale[0].ObservedSize++
		changed, err = QuarantineStillValid(1, quarantineRootID, stale, reader)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if !valid || changed {
		t.Fatalf("quarantine proof equality valid=%v changed=%v", valid, changed)
	}
}

func TestQuarantineRootRejectsReplacedMemberAndRollsBack(t *testing.T) {
	fixture := newQuarantineTestFixture(t, false)
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_source_files SET file_status='replaced' WHERE source_file_id=?`, fixture.memberFileID)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return QuarantineRoot(tx, fixture.buildEpoch, quarantineRootID,
			codexusage.FatalConflict{Code: codexusage.FatalResponseUsage}, fixture.presentProofs, 900)
	})
	if !errors.Is(err, ErrStaleQuarantineProof) {
		t.Fatalf("replaced member proof error = %v", err)
	}
	var quarantineCount int
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow(`SELECT COUNT(*) FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?`, fixture.buildEpoch, quarantineRootID).Scan(&quarantineCount)
	}); err != nil {
		t.Fatal(err)
	}
	if quarantineCount != 0 {
		t.Fatal("replaced member was committed as a quarantined root")
	}
}

func TestQuarantineRootRollsBackSemanticCleanupOnPrivateWriteFailure(t *testing.T) {
	fixture := newQuarantineTestFixture(t, false)
	if err := seedQuarantineLeaks(fixture); err != nil {
		t.Fatal(err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`CREATE TRIGGER reject_quarantine_insert BEFORE INSERT ON codex_usage_session_quarantine
				BEGIN SELECT RAISE(ABORT,'injected quarantine failure'); END`)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return QuarantineRoot(tx, fixture.buildEpoch, quarantineRootID,
			codexusage.FatalConflict{Code: codexusage.FatalResponseUsage}, fixture.presentProofs, 900)
	}); err == nil {
		t.Fatal("quarantine unexpectedly committed despite injected private write failure")
	}
	var skillRows, quarantineRows int
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		if err := reader.QueryRow(`SELECT COUNT(*) FROM codex_skill_usage_events WHERE ledger_epoch=?`, fixture.buildEpoch).Scan(&skillRows); err != nil {
			return err
		}
		return reader.QueryRow(`SELECT COUNT(*) FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?`, fixture.buildEpoch, quarantineRootID).Scan(&quarantineRows)
	}); err != nil {
		t.Fatal(err)
	}
	if skillRows != 2 || quarantineRows != 0 {
		t.Fatalf("failed quarantine left skill rows=%d quarantine rows=%d", skillRows, quarantineRows)
	}
	for table, expected := range map[string]int{"codex_usage_event_occurrences": 1, "codex_usage_reconciliation_windows": 1, "codex_turns": 2, "codex_usage_source_states": 2} {
		var count int
		if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
			return reader.QueryRow("SELECT COUNT(*) FROM "+table+" WHERE ledger_epoch=?", fixture.buildEpoch).Scan(&count)
		}); err != nil {
			t.Fatal(err)
		}
		if count != expected {
			t.Fatalf("rollback left %s rows=%d want=%d", table, count, expected)
		}
	}
	var canonicalCount int
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow(`SELECT COUNT(*) FROM usage_events WHERE source='codex' AND source_epoch=?`, fixture.buildEpoch).Scan(&canonicalCount)
	}); err != nil {
		t.Fatal(err)
	}
	if canonicalCount != 1 {
		t.Fatalf("rollback lost canonical evidence: rows=%d", canonicalCount)
	}
}

func TestQuarantineRootTerminalizesPendingAndRejectsNonFatal(t *testing.T) {
	fixture := newQuarantineTestFixture(t, false)
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return tx.Private(func(private storage.PrivateTx) error {
			_, err := private.Exec(`UPDATE codex_usage_build_sources SET completion_status='pending',completion_error_code=NULL,
				completed_generation=NULL,completed_through_offset=NULL,carry_from_epoch=NULL,carry_phase='none' WHERE build_epoch=?`, fixture.buildEpoch)
			return err
		})
	}); err != nil {
		t.Fatal(err)
	}
	err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return QuarantineRoot(tx, fixture.buildEpoch, quarantineRootID, codexusage.FatalConflict{Code: "RECONCILIATION_PATCH_TOO_LARGE"}, fixture.presentProofs, 900)
	})
	if !errors.Is(err, ErrInvalidFatalConflict) {
		t.Fatalf("non-Fatal quarantine error = %v", err)
	}
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		return QuarantineRoot(tx, fixture.buildEpoch, quarantineRootID, codexusage.FatalConflict{Code: codexusage.FatalResponseUsage}, fixture.presentProofs, 901)
	}); err != nil {
		t.Fatal(err)
	}
	assertQuarantinedManifest(t, fixture)
}

func seedQuarantineLeaks(fixture quarantineTestFixture) error {
	return fixture.storage.Write(func(tx *source.WriteTx) error {
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetBuild, quarantineTestEvent("occurrence-only", quarantineOtherRootID, quarantineOtherRootID)); err != nil {
			return err
		}
		return tx.Private(func(private storage.PrivateTx) error {
			for _, table := range []string{"codex_turns", "codex_usage_source_states", "codex_skill_usage_events"} {
				for _, scope := range []string{"root-only", "manifest-only"} {
					if err := insertQuarantineLeak(private, fixture, table, scope); err != nil {
						return err
					}
				}
			}
			if _, err := private.Exec(`INSERT INTO codex_usage_event_occurrences(source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,event_id,created_at_ms)
				VALUES('codex',?,?,1,30,40,'occurrence-only',8)`, fixture.buildEpoch, fixture.memberFileID); err != nil {
				return err
			}
			window, err := codexusage.CanonicalLegacyReconciliationWindowJSON(codexusage.LegacyReconciliationWindow{Version: 1, ChainState: codexusage.LegacyWindowChainState{Kind: "continuous"}})
			if err != nil {
				return err
			}
			_, err = private.Exec(`INSERT INTO codex_usage_reconciliation_windows(source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,state_json)
				VALUES('codex',?,?,1,30,40,?,?)`, fixture.buildEpoch, fixture.memberFileID, quarantineChildID, string(window))
			return err
		})
	})
}

func assertQuarantinedManifest(t *testing.T, fixture quarantineTestFixture) {
	t.Helper()
	var total, quarantined, cleared int
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow(`SELECT COUNT(*),
			SUM(completion_status='quarantined'),
			SUM(completed_generation IS NULL AND completed_through_offset IS NULL AND carry_from_epoch IS NULL AND carry_phase='none'
				AND carry_after_start_offset IS NULL AND carry_after_turn_key IS NULL AND carry_after_anomaly_id IS NULL
				AND carry_after_fact_event_id IS NULL AND carry_after_marker_start_offset IS NULL AND carry_after_window_start_offset IS NULL)
			FROM codex_usage_build_sources WHERE build_epoch=? AND expected_root_session_id=?`, fixture.buildEpoch, quarantineRootID).Scan(&total, &quarantined, &cleared)
	}); err != nil {
		t.Fatal(err)
	}
	var codes int
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow(`SELECT SUM(completion_error_code='RESPONSE_USAGE_CONFLICT') FROM codex_usage_build_sources WHERE build_epoch=? AND expected_root_session_id=?`, fixture.buildEpoch, quarantineRootID).Scan(&codes)
	}); err != nil {
		t.Fatal(err)
	}
	if total != 3 || quarantined != 3 || cleared != 3 || codes != 3 {
		t.Fatalf("quarantine terminalized %d/%d members, cleared progress on %d, code on %d", quarantined, total, cleared, codes)
	}
}

func insertQuarantineLeak(private storage.PrivateTx, fixture quarantineTestFixture, table, scope string) error {
	fileID, threadID, rootID := fixture.otherFileID, quarantineChildID, quarantineRootID
	if scope == "manifest-only" {
		fileID, threadID, rootID = fixture.memberFileID, quarantineOtherRootID, quarantineOtherRootID
	}
	switch table {
	case "codex_turns":
		_, err := private.Exec(`INSERT INTO codex_turns(
			ledger_epoch,source_file_id,file_generation,turn_key,thread_id,start_offset,end_offset,status,
			accounted_input_tokens,accounted_cached_tokens,accounted_output_tokens,accounted_reasoning_tokens,
			accounted_total_tokens,accounted_fingerprint,accounted_candidate_count,model_state,unresolved_model_seen,
			reasoning_effort_state,unresolved_reasoning_effort_seen,compensation_allowed,block_start_missing,
			block_time_missing,block_reset,block_ownership_gap,block_parser_gap,block_required_invalid,
			block_model_unresolved,quality_status,state_through_offset,updated_at_ms
		) VALUES(?,?,?,?,?,0,20,'completed',0,0,0,0,0,x'00',0,'none',0,'none',0,1,0,0,0,0,0,0,0,'complete',20,8)`,
			fixture.buildEpoch, fileID, 1, fmt.Sprintf("turn-%s-%s", table, scope), threadID)
		return err
	case "codex_usage_source_states":
		_, err := private.Exec(`INSERT INTO codex_usage_source_states(
			ledger_epoch,source_file_id,file_generation,device_id,inode,usage_parser_version,canonical_algorithm_version,
			resolved_through_offset,observed_raw_size,raw_tail_status,owning_thread_id,root_session_id,
			continuation_state,chain_state,updated_at_ms
		) VALUES(?,?,1,1,101,12,1,128,128,'none',?,?,'owning_live','continuous',8)`,
			fixture.buildEpoch, fileID, threadID, rootID)
		return err
	case "codex_skill_usage_events":
		_, err := private.Exec(`INSERT INTO codex_skill_usage_events(
			ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,occurred_at_ms,
			thread_id,root_session_id,model,skill_name,created_at_ms
		) VALUES(?,?,1,0,20,10,?,?,NULL,?,8)`, fixture.buildEpoch, fileID, threadID, rootID, "skill-"+scope)
		return err
	default:
		return fmt.Errorf("unknown quarantine leak table %q", table)
	}
}

func quarantineTestEvent(eventID, threadID, rootID string) sharedusage.CanonicalUsageEventWrite {
	return sharedusage.CanonicalUsageEventWrite{
		EventID: eventID, Kind: sharedusage.EventKindNormal, OccurredAtMS: 10,
		ThreadID: threadID, RootSessionID: rootID, Model: "model",
		Usage: sharedusage.NormalizedTokenUsage{InputTokens: 1, OutputTokens: 0, TotalTokens: 1}, CreatedAtMS: 11,
	}
}
