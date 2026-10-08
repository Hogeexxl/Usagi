package storage

import (
	"context"
	"database/sql"
	"reflect"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/domain"
)

func TestDirectScanStartCreatesManifestAtomically(t *testing.T) {
	ctx := context.Background()
	db := openCanonicalTestDB(t)
	manifestRejectingTrigger(t, db, true)
	event := domain.ScanStartEvent{
		ScanID:        "direct-scan",
		Trigger:       domain.ScanTriggerManual,
		RequestedAtMS: 10,
		StartedAtMS:   11,
	}
	if _, err := db.MarkScanStartedWithSources(ctx, event, []domain.SourceID{domain.SourceCodex}); err == nil {
		t.Fatal("start with a rejected child insert succeeded")
	}
	snapshot := lifecycleSnapshot(t, db, &event.ScanID)
	if snapshot.AppState.Scan.StatusRevision != 0 || snapshot.AppState.Scan.ActiveScanID != nil ||
		snapshot.TargetScan != nil || len(snapshot.Sources) != 0 {
		t.Fatalf("failed start left partial durable state: %+v", snapshot)
	}
	if db.CurrentRevision().StatusRevision != 0 {
		t.Fatalf("failed start published status revision %d", db.CurrentRevision().StatusRevision)
	}
	manifestRejectingTrigger(t, db, false)

	state, err := db.MarkScanStartedWithSources(ctx, event, []domain.SourceID{
		domain.SourceCodex,
		domain.SourceAntigravity,
	})
	if err != nil {
		t.Fatal(err)
	}
	if state.StatusRevision != 1 || state.State != domain.ScanLifecycleRunning ||
		state.ActiveScanID == nil || *state.ActiveScanID != event.ScanID {
		t.Fatalf("start state = %+v", state)
	}
	snapshot = lifecycleSnapshot(t, db, &event.ScanID)
	if snapshot.TargetScan == nil || snapshot.TargetScan.State != domain.ScanRunRunning ||
		snapshot.TargetScan.RequestKind != domain.ScanRequestDirect ||
		snapshot.TargetScan.StartedStatusRevision == nil || *snapshot.TargetScan.StartedStatusRevision != 1 {
		t.Fatalf("direct scan row = %+v", snapshot.TargetScan)
	}
	wantSources := []domain.SourceID{domain.SourceAntigravity, domain.SourceCodex}
	if got := lifecycleSourceIDs(snapshot.Sources); !reflect.DeepEqual(got, wantSources) {
		t.Fatalf("manifest order = %v, want %v", got, wantSources)
	}

	before := db.CurrentRevision().StatusRevision
	if _, err := db.MarkScanStartedWithSources(ctx, domain.ScanStartEvent{
		ScanID: "duplicate-manifest", Trigger: domain.ScanTriggerManual,
	}, []domain.SourceID{domain.SourceCodex, domain.SourceCodex}); err == nil {
		t.Fatal("duplicate source manifest succeeded")
	}
	if _, err := db.MarkScanStartedWithSources(ctx, domain.ScanStartEvent{
		ScanID: "invalid-manifest", Trigger: domain.ScanTriggerManual,
	}, []domain.SourceID{"Invalid"}); err == nil {
		t.Fatal("invalid source manifest succeeded")
	}
	if got := db.CurrentRevision().StatusRevision; got != before {
		t.Fatalf("invalid manifest changed status revision to %d", got)
	}
}

func TestFollowupCoalescesWithoutRevisionChurn(t *testing.T) {
	ctx := context.Background()
	db := openCanonicalTestDB(t)
	lifecycleStart(t, db, "active-scan")

	first, err := db.ReserveScanFollowup(ctx, domain.ReserveScanFollowupEvent{
		FollowupScanID: "followup-one", Trigger: domain.ScanTriggerManual, RequestedAtMS: 20,
	})
	if err != nil {
		t.Fatal(err)
	}
	if first.StatusRevision != 2 || first.FollowupScanID == nil || *first.FollowupScanID != "followup-one" ||
		first.FollowupEnqueuedStatusRevision == nil || *first.FollowupEnqueuedStatusRevision != 2 {
		t.Fatalf("first reservation = %+v", first)
	}

	second, err := db.ReserveScanFollowup(ctx, domain.ReserveScanFollowupEvent{
		FollowupScanID: "followup-two", Trigger: domain.ScanTriggerScheduled, RequestedAtMS: 30,
	})
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(second, first) {
		t.Fatalf("coalesced state = %+v, want original %+v", second, first)
	}
	if got := db.CurrentRevision().StatusRevision; got != 2 {
		t.Fatalf("coalescing changed status revision to %d", got)
	}
	if run := lifecycleSnapshot(t, db, ptrString("followup-one")).TargetScan; run == nil || run.State != domain.ScanRunQueued {
		t.Fatalf("original queued row = %+v", run)
	}
	if run := lifecycleSnapshot(t, db, ptrString("followup-two")).TargetScan; run != nil {
		t.Fatalf("coalescing created another row: %+v", run)
	}
}

func TestFollowupStartAtomicManifest(t *testing.T) {
	ctx := context.Background()
	db := openCanonicalTestDB(t)
	lifecycleStart(t, db, "active-scan")
	followup := lifecycleReserve(t, db, "followup-scan")
	lifecycleComplete(t, db, "active-scan", 30)

	manifestRejectingTrigger(t, db, true)
	event := domain.FollowupStartedEvent{ScanID: followup, StartedAtMS: 40}
	if _, err := db.MarkFollowupStartedWithSources(ctx, event, []domain.SourceID{domain.SourceCodex}); err == nil {
		t.Fatal("follow-up start with a rejected child insert succeeded")
	}
	snapshot := lifecycleSnapshot(t, db, &followup)
	if snapshot.AppState.Scan.StatusRevision != 3 || snapshot.AppState.Scan.State != domain.ScanLifecycleIdle ||
		snapshot.AppState.Scan.FollowupState == nil || *snapshot.AppState.Scan.FollowupState != domain.FollowupQueued ||
		snapshot.TargetScan == nil || snapshot.TargetScan.State != domain.ScanRunQueued || len(snapshot.Sources) != 0 {
		t.Fatalf("failed follow-up start left partial state: %+v", snapshot)
	}
	manifestRejectingTrigger(t, db, false)

	state, err := db.MarkFollowupStartedWithSources(ctx, event, []domain.SourceID{
		domain.SourceCodex,
		domain.SourceAntigravity,
	})
	if err != nil {
		t.Fatal(err)
	}
	if state.StatusRevision != 4 || state.ActiveScanID == nil || *state.ActiveScanID != followup ||
		state.FollowupScanID != nil || state.FollowupState != nil {
		t.Fatalf("follow-up start state = %+v", state)
	}
	snapshot = lifecycleSnapshot(t, db, &followup)
	if snapshot.TargetScan == nil || snapshot.TargetScan.State != domain.ScanRunRunning ||
		snapshot.TargetScan.RequestKind != domain.ScanRequestFollowup ||
		snapshot.TargetScan.StartedStatusRevision == nil || *snapshot.TargetScan.StartedStatusRevision != 4 {
		t.Fatalf("started follow-up row = %+v", snapshot.TargetScan)
	}
	if got, want := lifecycleSourceIDs(snapshot.Sources), []domain.SourceID{domain.SourceAntigravity, domain.SourceCodex}; !reflect.DeepEqual(got, want) {
		t.Fatalf("follow-up manifest = %v, want %v", got, want)
	}
}

func TestFollowupStartFailedWhitelist(t *testing.T) {
	ctx := context.Background()
	db := openCanonicalTestDB(t)
	for i, code := range []string{"SCAN_START_FAILED", "SCANNER_UNAVAILABLE", "SOURCE_CHANGED"} {
		activeID := "active-" + code
		followupID := "followup-" + code
		directID := "next-" + code
		lifecycleStart(t, db, activeID)
		lifecycleReserve(t, db, followupID)
		lifecycleComplete(t, db, activeID, int64(100+i*10))

		before := db.CurrentRevision().StatusRevision
		if _, err := db.MarkFollowupStartFailed(ctx, domain.FollowupStartFailedEvent{
			ScanID: followupID, FailedAtMS: int64(101 + i*10), ErrorCode: "SOURCE_RUN_FAILED",
		}); err == nil {
			t.Fatal("non-whitelisted follow-up start failure code succeeded")
		}
		if got := db.CurrentRevision().StatusRevision; got != before {
			t.Fatalf("rejected code changed status revision to %d", got)
		}

		state, err := db.MarkFollowupStartFailed(ctx, domain.FollowupStartFailedEvent{
			ScanID: followupID, FailedAtMS: int64(102 + i*10), ErrorCode: code,
		})
		if err != nil {
			t.Fatal(err)
		}
		if state.StatusRevision != before+1 || state.FollowupState == nil ||
			*state.FollowupState != domain.FollowupStartFailed || state.FollowupErrorCode == nil ||
			*state.FollowupErrorCode != code {
			t.Fatalf("start-failed state = %+v", state)
		}
		startFailedRevision := state.StatusRevision

		if i == 0 {
			if _, err := db.writer.ExecContext(ctx,
				"UPDATE scan_runs SET error_code = 'SOURCE_CHANGED' WHERE scan_id = ?", followupID,
			); err != nil {
				t.Fatal(err)
			}
			before = db.CurrentRevision().StatusRevision
			if _, err := db.MarkScanStartedWithSources(ctx, domain.ScanStartEvent{
				ScanID: directID, Trigger: domain.ScanTriggerManual, RequestedAtMS: 110, StartedAtMS: 111,
			}, nil); err == nil {
				t.Fatal("direct start accepted a mismatched start-failed projection")
			}
			if got := db.CurrentRevision().StatusRevision; got != before {
				t.Fatalf("mismatched historical row changed status revision to %d", got)
			}
			if _, err := db.writer.ExecContext(ctx,
				"UPDATE scan_runs SET error_code = ? WHERE scan_id = ?", code, followupID,
			); err != nil {
				t.Fatal(err)
			}
		}

		state, err = db.MarkScanStartedWithSources(ctx, domain.ScanStartEvent{
			ScanID: directID, Trigger: domain.ScanTriggerManual, RequestedAtMS: 110, StartedAtMS: 111,
		}, nil)
		if err != nil {
			t.Fatal(err)
		}
		if state.FollowupScanID != nil || state.FollowupState != nil || state.StatusRevision != startFailedRevision+1 {
			t.Fatalf("direct start did not clear start-failed slot: %+v", state)
		}
		historical := lifecycleSnapshot(t, db, &followupID).TargetScan
		if historical == nil || historical.State != domain.ScanRunStartFailed ||
			historical.ErrorCode == nil || *historical.ErrorCode != code {
			t.Fatalf("start-failed history was changed: %+v", historical)
		}
		if i < 2 {
			lifecycleComplete(t, db, directID, 120)
		}
	}
}

func TestScanCompleteRejectsUnfinishedChildren(t *testing.T) {
	db := openCanonicalTestDB(t)
	lifecycleStart(t, db, "scan", domain.SourceCodex)

	if _, err := db.MarkScanCompleted(context.Background(), domain.ScanCompletedEvent{ScanID: "scan", CompletedAtMS: 20}); err == nil {
		t.Fatal("completed scan with a queued child")
	}
	if got := db.CurrentRevision().StatusRevision; got != 1 {
		t.Fatalf("rejected completion changed status revision to %d", got)
	}
	if err := db.MarkSourceScanStarted(context.Background(), "scan", domain.SourceCodex, 12); err != nil {
		t.Fatal(err)
	}
	if _, err := db.MarkScanCompleted(context.Background(), domain.ScanCompletedEvent{ScanID: "scan", CompletedAtMS: 20}); err == nil {
		t.Fatal("completed scan with a running child")
	}
	if got := db.CurrentRevision().StatusRevision; got != 2 {
		t.Fatalf("rejected completion changed status revision to %d", got)
	}
	if err := db.MarkSourceScanCompleted(context.Background(), "scan", domain.SourceCodex, 18); err != nil {
		t.Fatal(err)
	}
	state := lifecycleComplete(t, db, "scan", 20)
	if state.State != domain.ScanLifecycleIdle || state.LastFinishedScanResult == nil ||
		*state.LastFinishedScanResult != domain.ScanResultCompleted {
		t.Fatalf("completed state = %+v", state)
	}
}

func TestScanAggregateFailure(t *testing.T) {
	db := openCanonicalTestDB(t)
	lifecycleStart(t, db, "aggregate", domain.SourceCodex, domain.SourceAntigravity)
	if err := db.MarkSourceScanFailed(context.Background(), "aggregate", domain.SourceCodex, 20, "SOURCE_RUN_FAILED"); err != nil {
		t.Fatal(err)
	}
	if err := db.MarkSourceScanSkipped(context.Background(), "aggregate", domain.SourceAntigravity, 21); err != nil {
		t.Fatal(err)
	}
	state := lifecycleComplete(t, db, "aggregate", 30)
	if state.State != domain.ScanLifecycleFailed || state.LastScanErrorCode == nil ||
		*state.LastScanErrorCode != "SOURCE_RUN_FAILED" || state.LastFinishedScanID == nil ||
		*state.LastFinishedScanID != "aggregate" || state.LastFinishedScanResult == nil ||
		*state.LastFinishedScanResult != domain.ScanResultFailed {
		t.Fatalf("aggregated state = %+v", state)
	}
	snapshot := lifecycleSnapshot(t, db, ptrString("aggregate"))
	if snapshot.TargetScan == nil || snapshot.TargetScan.State != domain.ScanRunFailed ||
		snapshot.TargetScan.ErrorCode == nil || *snapshot.TargetScan.ErrorCode != "SOURCE_RUN_FAILED" ||
		snapshot.TargetScan.TerminalStatusRevision == nil || *snapshot.TargetScan.TerminalStatusRevision != 4 {
		t.Fatalf("aggregated parent row = %+v", snapshot.TargetScan)
	}
}

func TestExplicitScanFailureTerminalizesChildren(t *testing.T) {
	ctx := context.Background()
	db := openCanonicalTestDB(t)
	lifecycleStart(t, db, "explicit", domain.SourceCodex, domain.SourceAntigravity)
	if err := db.MarkSourceScanStarted(ctx, "explicit", domain.SourceCodex, 12); err != nil {
		t.Fatal(err)
	}
	state, err := db.MarkScanFailed(ctx, domain.ScanFailedEvent{
		ScanID: "explicit", FailedAtMS: 20, ErrorCode: "SCAN_CANCELLED",
	})
	if err != nil {
		t.Fatal(err)
	}
	if state.StatusRevision != 3 || state.State != domain.ScanLifecycleFailed || state.ActiveScanID != nil {
		t.Fatalf("explicit failed state = %+v", state)
	}
	snapshot := lifecycleSnapshot(t, db, ptrString("explicit"))
	if snapshot.TargetScan == nil || snapshot.TargetScan.State != domain.ScanRunFailed ||
		snapshot.TargetScan.ErrorCode == nil || *snapshot.TargetScan.ErrorCode != "SCAN_CANCELLED" {
		t.Fatalf("explicit failed parent = %+v", snapshot.TargetScan)
	}
	if len(snapshot.Sources) != 2 {
		t.Fatalf("failed children = %+v", snapshot.Sources)
	}
	for _, child := range snapshot.Sources {
		if child.State != domain.SourceScanFailed || child.ErrorCode == nil || *child.ErrorCode != "SCAN_CANCELLED" {
			t.Errorf("child was not terminalized with the parent: %+v", child)
		}
	}
	started := lifecycleSourceRow(t, db, "explicit", string(domain.SourceCodex))
	queued := lifecycleSourceRow(t, db, "explicit", string(domain.SourceAntigravity))
	if !started.StartedAtMS.Valid || started.StartedAtMS.Int64 != 12 ||
		!started.FinishedAtMS.Valid || started.FinishedAtMS.Int64 != 20 {
		t.Errorf("running child times = %+v", started)
	}
	if queued.StartedAtMS.Valid || !queued.FinishedAtMS.Valid || queued.FinishedAtMS.Int64 != 20 {
		t.Errorf("queued child times = %+v", queued)
	}
}

func TestSourceScanStateMachine(t *testing.T) {
	ctx := context.Background()
	db := openCanonicalTestDB(t)
	unknownSource, err := domain.NewSourceID("zsource")
	if err != nil {
		t.Fatal(err)
	}
	lifecycleStart(t, db, "source-states", domain.SourceCodex, domain.SourceAntigravity, unknownSource)

	if err := db.MarkSourceScanCompleted(ctx, "source-states", domain.SourceCodex, 10); err == nil {
		t.Fatal("queued child completed without starting")
	}
	if err := db.MarkSourceScanStarted(ctx, "source-states", domain.SourceCodex, 10); err != nil {
		t.Fatal(err)
	}
	if err := db.MarkSourceScanStarted(ctx, "source-states", domain.SourceCodex, 11); err == nil {
		t.Fatal("running child started twice")
	}
	if err := db.MarkSourceScanSkipped(ctx, "source-states", domain.SourceCodex, 11); err == nil {
		t.Fatal("running child skipped")
	}
	if err := db.MarkSourceScanCompleted(ctx, "source-states", domain.SourceCodex, 15); err != nil {
		t.Fatal(err)
	}
	if err := db.MarkSourceScanFailed(ctx, "source-states", domain.SourceCodex, 16, "SOURCE_RUN_FAILED"); err == nil {
		t.Fatal("completed child failed again")
	}
	if err := db.MarkSourceScanSkipped(ctx, "source-states", domain.SourceAntigravity, 16); err != nil {
		t.Fatal(err)
	}
	if err := db.MarkSourceScanStarted(ctx, "source-states", domain.SourceAntigravity, 17); err == nil {
		t.Fatal("skipped child started")
	}
	if err := db.MarkSourceScanSkipped(ctx, "source-states", domain.SourceAntigravity, 17); err == nil {
		t.Fatal("skipped child skipped twice")
	}
	if err := db.MarkSourceScanFailed(ctx, "source-states", unknownSource, 18, "SOURCE_RUN_FAILED"); err != nil {
		t.Fatal(err)
	}
	if err := db.MarkSourceScanStarted(ctx, "source-states", domain.SourceCodex, 19); err == nil {
		t.Fatal("child transition ignored its terminal parent")
	}
	state := lifecycleComplete(t, db, "source-states", 30)
	if state.State != domain.ScanLifecycleFailed {
		t.Fatalf("failed child did not aggregate: %+v", state)
	}

	lifecycleStart(t, db, "running-failure", domain.SourceCodex, domain.SourceAntigravity)
	if err := db.MarkSourceScanStarted(ctx, "running-failure", domain.SourceCodex, 40); err != nil {
		t.Fatal(err)
	}
	if err := db.MarkSourceScanFailed(ctx, "running-failure", domain.SourceCodex, 50, "SOURCE_RUN_FAILED"); err != nil {
		t.Fatal(err)
	}
	if err := db.MarkSourceScanFailed(ctx, "running-failure", domain.SourceAntigravity, 50, "bad-code"); err == nil {
		t.Fatal("invalid source scan error code succeeded")
	}
	if err := db.MarkSourceScanFailed(ctx, "running-failure", domain.SourceID("Invalid"), 50, "SOURCE_RUN_FAILED"); err == nil {
		t.Fatal("invalid source ID succeeded")
	}
	if err := db.MarkSourceScanFailed(ctx, "running-failure", domain.SourceAntigravity, -1, "SOURCE_RUN_FAILED"); err == nil {
		t.Fatal("negative child timestamp succeeded")
	}
	if got := db.CurrentRevision().StatusRevision; got != 9 {
		t.Fatalf("invalid child transitions changed status revision to %d", got)
	}
	if err := db.MarkSourceScanSkipped(ctx, "running-failure", domain.SourceAntigravity, 55); err != nil {
		t.Fatal(err)
	}
	state = lifecycleComplete(t, db, "running-failure", 60)
	if state.State != domain.ScanLifecycleFailed {
		t.Fatalf("running child failure did not aggregate: %+v", state)
	}
}

func TestSourceScanStateMachineMatrix(t *testing.T) {
	ctx := context.Background()
	operations := []struct {
		name string
		run  func(*DB) error
	}{
		{"Start", func(db *DB) error { return db.MarkSourceScanStarted(ctx, "matrix", domain.SourceCodex, 30) }},
		{"Complete", func(db *DB) error { return db.MarkSourceScanCompleted(ctx, "matrix", domain.SourceCodex, 30) }},
		{"Skip", func(db *DB) error { return db.MarkSourceScanSkipped(ctx, "matrix", domain.SourceCodex, 30) }},
		{"Fail", func(db *DB) error {
			return db.MarkSourceScanFailed(ctx, "matrix", domain.SourceCodex, 30, "NEXT_FAILURE")
		}},
	}
	states := []domain.SourceScanState{
		domain.SourceScanQueued, domain.SourceScanRunning, domain.SourceScanCompleted,
		domain.SourceScanSkipped, domain.SourceScanFailed,
	}
	prepare := func(t *testing.T, initial domain.SourceScanState) *DB {
		t.Helper()
		db := openCanonicalTestDB(t)
		lifecycleStart(t, db, "matrix", domain.SourceCodex)
		if initial == domain.SourceScanRunning || initial == domain.SourceScanCompleted {
			if err := db.MarkSourceScanStarted(ctx, "matrix", domain.SourceCodex, 10); err != nil {
				t.Fatal(err)
			}
		}
		var err error
		switch initial {
		case domain.SourceScanCompleted:
			err = db.MarkSourceScanCompleted(ctx, "matrix", domain.SourceCodex, 20)
		case domain.SourceScanSkipped:
			err = db.MarkSourceScanSkipped(ctx, "matrix", domain.SourceCodex, 20)
		case domain.SourceScanFailed:
			err = db.MarkSourceScanFailed(ctx, "matrix", domain.SourceCodex, 20, "INITIAL_FAILURE")
		}
		if err != nil {
			t.Fatal(err)
		}
		return db
	}
	assertUnchanged := func(t *testing.T, db *DB, row lifecycleSourceRowData, snapshot domain.ScanStatusSnapshot, revision RevisionTuple) {
		t.Helper()
		if got := lifecycleSourceRow(t, db, "matrix", string(domain.SourceCodex)); got != row {
			t.Fatalf("rejected operation changed child row: got %+v, want %+v", got, row)
		}
		if got := lifecycleSnapshot(t, db, ptrString("matrix")); !reflect.DeepEqual(got, snapshot) {
			t.Fatalf("rejected operation changed durable snapshot: got %+v, want %+v", got, snapshot)
		}
		if got := db.CurrentRevision(); got != revision {
			t.Fatalf("rejected operation changed published revision: got %+v, want %+v", got, revision)
		}
	}

	for _, initial := range states {
		for _, operation := range operations {
			t.Run(string(initial)+"/"+operation.name, func(t *testing.T) {
				db := prepare(t, initial)
				beforeRow := lifecycleSourceRow(t, db, "matrix", string(domain.SourceCodex))
				beforeSnapshot := lifecycleSnapshot(t, db, ptrString("matrix"))
				beforeRevision := db.CurrentRevision()
				allowed := initial == domain.SourceScanQueued && (operation.name == "Start" || operation.name == "Skip" || operation.name == "Fail") ||
					initial == domain.SourceScanRunning && (operation.name == "Complete" || operation.name == "Fail")
				err := operation.run(db)
				if !allowed {
					assertErrorKind(t, err, ErrorInvalidState)
					assertUnchanged(t, db, beforeRow, beforeSnapshot, beforeRevision)
					return
				}
				if err != nil {
					t.Fatal(err)
				}
				want := beforeRow
				switch operation.name {
				case "Start":
					want.State = "running"
					want.StartedAtMS = sql.NullInt64{Int64: 30, Valid: true}
				case "Complete":
					want.State = "completed"
					want.FinishedAtMS = sql.NullInt64{Int64: 30, Valid: true}
				case "Skip":
					want.State = "skipped"
					want.FinishedAtMS = sql.NullInt64{Int64: 30, Valid: true}
				case "Fail":
					want.State = "failed"
					want.FinishedAtMS = sql.NullInt64{Int64: 30, Valid: true}
					want.ErrorCode = sql.NullString{String: "NEXT_FAILURE", Valid: true}
				}
				if got := lifecycleSourceRow(t, db, "matrix", string(domain.SourceCodex)); got != want {
					t.Fatalf("child row = %+v, want %+v", got, want)
				}
				wantRevision := beforeRevision
				wantRevision.StatusRevision++
				if got := db.CurrentRevision(); got != wantRevision {
					t.Fatalf("revision = %+v, want %+v", got, wantRevision)
				}
				if got := lifecycleSnapshot(t, db, ptrString("matrix")).AppState.Scan.StatusRevision; got != wantRevision.StatusRevision {
					t.Fatalf("durable status revision = %d, want %d", got, wantRevision.StatusRevision)
				}
			})
		}
	}

	for _, parent := range []string{"active-mismatch", "completed", "failed"} {
		for _, initial := range []domain.SourceScanState{domain.SourceScanQueued, domain.SourceScanRunning} {
			for _, operation := range operations {
				t.Run(parent+"/"+string(initial)+"/"+operation.name, func(t *testing.T) {
					db := prepare(t, initial)
					// Keep the child runnable to isolate the parent guard from child-state rejection.
					if parent == "active-mismatch" {
						if _, err := db.writer.ExecContext(ctx,
							`INSERT INTO scan_runs (scan_id, trigger, request_kind, state, requested_at_ms, started_at_ms, started_status_revision)
							 VALUES ('other-active', 'Manual', 'direct', 'running', 1, 2, 1)`); err != nil {
							t.Fatal(err)
						}
						if _, err := db.writer.ExecContext(ctx, "UPDATE app_meta SET active_scan_id = 'other-active' WHERE id = 1"); err != nil {
							t.Fatal(err)
						}
					} else {
						var code any
						if parent == "failed" {
							code = "PARENT_FAILURE"
						}
						if _, err := db.writer.ExecContext(ctx,
							`UPDATE scan_runs SET state = ?, finished_at_ms = 20, terminal_status_revision = 1, error_code = ?
							 WHERE scan_id = 'matrix'`, parent, code); err != nil {
							t.Fatal(err)
						}
					}
					beforeRow := lifecycleSourceRow(t, db, "matrix", string(domain.SourceCodex))
					beforeSnapshot := lifecycleSnapshot(t, db, ptrString("matrix"))
					beforeRevision := db.CurrentRevision()
					assertErrorKind(t, operation.run(db), ErrorInvalidState)
					assertUnchanged(t, db, beforeRow, beforeSnapshot, beforeRevision)
				})
			}
		}
	}
}

func TestLifecycleStatusRevision(t *testing.T) {
	ctx := context.Background()
	db := openCanonicalTestDB(t)
	lifecycleStart(t, db, "revision-direct", domain.SourceCodex)
	lifecycleAssertStatusRevision(t, db, 1)
	lifecycleReserve(t, db, "revision-followup")
	lifecycleAssertStatusRevision(t, db, 2)
	if _, err := db.ReserveScanFollowup(ctx, domain.ReserveScanFollowupEvent{
		FollowupScanID: "ignored-followup", Trigger: domain.ScanTriggerManual, RequestedAtMS: 3,
	}); err != nil {
		t.Fatal(err)
	}
	lifecycleAssertStatusRevision(t, db, 2)
	if err := db.MarkSourceScanStarted(ctx, "revision-direct", domain.SourceCodex, 4); err != nil {
		t.Fatal(err)
	}
	lifecycleAssertStatusRevision(t, db, 3)
	if err := db.MarkSourceScanCompleted(ctx, "revision-direct", domain.SourceCodex, 5); err != nil {
		t.Fatal(err)
	}
	lifecycleAssertStatusRevision(t, db, 4)
	lifecycleComplete(t, db, "revision-direct", 6)
	lifecycleAssertStatusRevision(t, db, 5)
	started, err := db.MarkFollowupStartedWithSources(ctx, domain.FollowupStartedEvent{
		ScanID: "revision-followup", StartedAtMS: 7,
	}, []domain.SourceID{domain.SourceAntigravity})
	if err != nil {
		t.Fatal(err)
	}
	if started.StatusRevision != 6 {
		t.Fatalf("follow-up start revision = %d, want 6", started.StatusRevision)
	}
	if err := db.MarkSourceScanSkipped(ctx, "revision-followup", domain.SourceAntigravity, 8); err != nil {
		t.Fatal(err)
	}
	lifecycleAssertStatusRevision(t, db, 7)
	lifecycleComplete(t, db, "revision-followup", 9)
	lifecycleAssertStatusRevision(t, db, 8)
	if got := db.CurrentRevision().DataRevision; got != 0 {
		t.Fatalf("lifecycle transitions changed data revision to %d", got)
	}
}

func TestScanSnapshotSingleTransactionAndSort(t *testing.T) {
	ctx := context.Background()
	db := openCanonicalTestDB(t)
	lifecycleStart(t, db, "snapshot-scan", domain.SourceCodex, domain.SourceAntigravity)
	if _, err := db.writer.ExecContext(ctx, "DELETE FROM source_scan_runs WHERE scan_id = ?", "snapshot-scan"); err != nil {
		t.Fatal(err)
	}
	for _, source := range []string{"zeta", "alpha"} {
		if _, err := db.writer.ExecContext(ctx,
			`INSERT INTO source_scan_runs (scan_id, source, state) VALUES (?, ?, 'queued')`, "snapshot-scan", source,
		); err != nil {
			t.Fatal(err)
		}
	}

	snapshot, err := db.ScanStatusSnapshot(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	if snapshot.TargetScan != nil || snapshot.AppState.Scan.ActiveScanID == nil ||
		*snapshot.AppState.Scan.ActiveScanID != "snapshot-scan" || snapshot.AppState.Scan.StatusRevision != 1 {
		t.Fatalf("active snapshot = %+v", snapshot)
	}
	if got, want := lifecycleSourceIDs(snapshot.Sources), []domain.SourceID{"alpha", "zeta"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("snapshot source order = %v, want %v", got, want)
	}
	target := "snapshot-scan"
	snapshot, err = db.ScanStatusSnapshot(ctx, &target)
	if err != nil {
		t.Fatal(err)
	}
	if snapshot.TargetScan == nil || snapshot.TargetScan.StartedStatusRevision == nil ||
		*snapshot.TargetScan.StartedStatusRevision != snapshot.AppState.Scan.StatusRevision {
		t.Fatalf("target and app projection do not share a status revision: %+v", snapshot)
	}
	invalid := "bad\x00scan"
	if _, err := db.ScanStatusSnapshot(ctx, &invalid); err == nil {
		t.Fatal("snapshot accepted an invalid target Scan ID")
	}
}

func TestTerminalScanCannotReopen(t *testing.T) {
	ctx := context.Background()
	db := openCanonicalTestDB(t)
	lifecycleStart(t, db, "terminal-complete", domain.SourceCodex)
	if err := db.MarkSourceScanSkipped(ctx, "terminal-complete", domain.SourceCodex, 12); err != nil {
		t.Fatal(err)
	}
	lifecycleComplete(t, db, "terminal-complete", 15)
	before := db.CurrentRevision().StatusRevision
	if _, err := db.MarkScanStartedWithSources(ctx, domain.ScanStartEvent{
		ScanID: "terminal-complete", Trigger: domain.ScanTriggerManual, RequestedAtMS: 20, StartedAtMS: 21,
	}, nil); err == nil {
		t.Fatal("completed scan row restarted")
	}
	if _, err := db.MarkScanCompleted(ctx, domain.ScanCompletedEvent{ScanID: "terminal-complete", CompletedAtMS: 22}); err == nil {
		t.Fatal("completed scan completed again")
	}
	if _, err := db.MarkScanFailed(ctx, domain.ScanFailedEvent{
		ScanID: "terminal-complete", FailedAtMS: 22, ErrorCode: "SCAN_CANCELLED",
	}); err == nil {
		t.Fatal("completed scan changed terminal result")
	}
	if got := db.CurrentRevision().StatusRevision; got != before {
		t.Fatalf("rejected terminal transitions changed status revision to %d", got)
	}

	lifecycleStart(t, db, "terminal-failed")
	if _, err := db.MarkScanFailed(ctx, domain.ScanFailedEvent{
		ScanID: "terminal-failed", FailedAtMS: 30, ErrorCode: "SCAN_INTERRUPTED",
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := db.MarkScanStartedWithSources(ctx, domain.ScanStartEvent{
		ScanID: "terminal-failed", Trigger: domain.ScanTriggerStartup, RequestedAtMS: 31, StartedAtMS: 32,
	}, nil); err == nil {
		t.Fatal("failed scan row restarted")
	}
	run := lifecycleSnapshot(t, db, ptrString("terminal-failed")).TargetScan
	if run == nil || run.State != domain.ScanRunFailed || run.ErrorCode == nil || *run.ErrorCode != "SCAN_INTERRUPTED" {
		t.Fatalf("failed terminal row changed: %+v", run)
	}
}

type lifecycleSourceRowData struct {
	State        string
	StartedAtMS  sql.NullInt64
	FinishedAtMS sql.NullInt64
	ErrorCode    sql.NullString
}

func lifecycleSourceRow(t *testing.T, db *DB, scanID, source string) lifecycleSourceRowData {
	t.Helper()
	var value lifecycleSourceRowData
	err := db.ReadTx(context.Background(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(context.Background(),
			`SELECT state, started_at_ms, finished_at_ms, error_code
			 FROM source_scan_runs WHERE scan_id = ? AND source = ?`, scanID, source,
		).Scan(&value.State, &value.StartedAtMS, &value.FinishedAtMS, &value.ErrorCode)
	})
	if err != nil {
		t.Fatal(err)
	}
	return value
}

func lifecycleSnapshot(t *testing.T, db *DB, target *string) domain.ScanStatusSnapshot {
	t.Helper()
	snapshot, err := db.ScanStatusSnapshot(context.Background(), target)
	if err != nil {
		t.Fatal(err)
	}
	return snapshot
}

func lifecycleStart(t *testing.T, db *DB, scanID string, sources ...domain.SourceID) domain.ScanState {
	t.Helper()
	state, err := db.MarkScanStartedWithSources(context.Background(), domain.ScanStartEvent{
		ScanID: scanID, Trigger: domain.ScanTriggerManual, RequestedAtMS: 1, StartedAtMS: 2,
	}, sources)
	if err != nil {
		t.Fatal(err)
	}
	return state
}

func lifecycleReserve(t *testing.T, db *DB, scanID string) string {
	t.Helper()
	state, err := db.ReserveScanFollowup(context.Background(), domain.ReserveScanFollowupEvent{
		FollowupScanID: scanID, Trigger: domain.ScanTriggerManual, RequestedAtMS: 3,
	})
	if err != nil {
		t.Fatal(err)
	}
	if state.FollowupScanID == nil || *state.FollowupScanID != scanID {
		t.Fatalf("reserved follow-up = %+v", state)
	}
	return scanID
}

func lifecycleComplete(t *testing.T, db *DB, scanID string, completedAtMS int64) domain.ScanState {
	t.Helper()
	state, err := db.MarkScanCompleted(context.Background(), domain.ScanCompletedEvent{
		ScanID: scanID, CompletedAtMS: completedAtMS,
	})
	if err != nil {
		t.Fatal(err)
	}
	return state
}

func lifecycleAssertStatusRevision(t *testing.T, db *DB, want int64) {
	t.Helper()
	if got := db.CurrentRevision().StatusRevision; got != want {
		t.Fatalf("status revision = %d, want %d", got, want)
	}
}

func lifecycleSourceIDs(sources []domain.SourceScanStatus) []domain.SourceID {
	ids := make([]domain.SourceID, 0, len(sources))
	for _, source := range sources {
		ids = append(ids, source.Source)
	}
	return ids
}

func manifestRejectingTrigger(t *testing.T, db *DB, install bool) {
	t.Helper()
	query := `DROP TRIGGER IF EXISTS lifecycle_reject_manifest`
	if install {
		query = `CREATE TRIGGER lifecycle_reject_manifest BEFORE INSERT ON source_scan_runs
			BEGIN SELECT RAISE(ABORT, 'manifest rejected'); END`
	}
	if _, err := db.writer.ExecContext(context.Background(), query); err != nil {
		t.Fatal(err)
	}
}

func ptrString(value string) *string {
	return &value
}
