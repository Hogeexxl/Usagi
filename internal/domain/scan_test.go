package domain

import (
	"strings"
	"testing"
)

func TestScanDomainRejectsUnknownEnums(t *testing.T) {
	tests := []struct {
		name     string
		valid    []string
		validate func(string) error
		parse    func(string) error
	}{
		{"trigger", []string{"Startup", "Scheduled", "Manual", "SourceChanged", "Rebuild"}, func(v string) error { return ScanTrigger(v).Validate() }, func(v string) error { _, err := ParseScanTrigger(v); return err }},
		{"request kind", []string{"direct", "followup"}, func(v string) error { return ScanRequestKind(v).Validate() }, func(v string) error { _, err := ParseScanRequestKind(v); return err }},
		{"lifecycle state", []string{"idle", "running", "failed"}, func(v string) error { return ScanLifecycleState(v).Validate() }, func(v string) error { _, err := ParseScanLifecycleState(v); return err }},
		{"run state", []string{"queued", "running", "completed", "failed", "start_failed"}, func(v string) error { return ScanRunState(v).Validate() }, func(v string) error { _, err := ParseScanRunState(v); return err }},
		{"source scan state", []string{"queued", "running", "completed", "skipped", "failed"}, func(v string) error { return SourceScanState(v).Validate() }, func(v string) error { _, err := ParseSourceScanState(v); return err }},
		{"result", []string{"completed", "failed"}, func(v string) error { return ScanResult(v).Validate() }, func(v string) error { _, err := ParseScanResult(v); return err }},
		{"follow-up state", []string{"queued", "start_failed"}, func(v string) error { return FollowupState(v).Validate() }, func(v string) error { _, err := ParseFollowupState(v); return err }},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			for _, value := range test.valid {
				if err := test.validate(value); err != nil {
					t.Errorf("Validate(%q): %v", value, err)
				}
				if err := test.parse(value); err != nil {
					t.Errorf("Parse(%q): %v", value, err)
				}
			}
			for _, value := range []string{"", "unknown"} {
				if err := test.validate(value); err == nil {
					t.Errorf("Validate(%q) succeeded", value)
				}
				if err := test.parse(value); err == nil {
					t.Errorf("Parse(%q) succeeded", value)
				}
			}
		})
	}
}

func TestValidateErrorCode(t *testing.T) {
	for _, value := range []string{"A", "A_1", "Z9", "A" + strings.Repeat("1", 63)} {
		if err := ValidateErrorCode(value); err != nil {
			t.Errorf("ValidateErrorCode(%q): %v", value, err)
		}
	}
	for _, value := range []string{"", "a", "A-1", "Aé", "A" + strings.Repeat("1", 64)} {
		if err := ValidateErrorCode(value); err == nil {
			t.Errorf("ValidateErrorCode(%q) succeeded", value)
		}
	}
}

func TestScanDomainProjectionValidation(t *testing.T) {
	finishedID, failedResult := "finished", ScanResultFailed
	failedCode := "SOURCE_RUN_FAILED"
	queuedSource := SourceScanStatus{Source: SourceAntigravity, State: SourceScanSkipped}
	failedSource := SourceScanStatus{Source: SourceCodex, State: SourceScanFailed, ErrorCode: &failedCode}
	startedAt, startedRevision := int64(8), int64(3)
	finishedAt, terminalRevision := int64(7), int64(4)
	target := ScanRun{
		ScanID:                 "target",
		Trigger:                ScanTriggerManual,
		RequestKind:            ScanRequestDirect,
		State:                  ScanRunCompleted,
		RequestedAtMS:          10,
		StartedAtMS:            &startedAt,
		StartedStatusRevision:  &startedRevision,
		FinishedAtMS:           &finishedAt,
		TerminalStatusRevision: &terminalRevision,
	}
	validSnapshot := ScanStatusSnapshot{
		AppState: AppState{
			DataRevision: 5,
			Scan: ScanState{
				StatusRevision:         4,
				State:                  ScanLifecycleIdle,
				LastFinishedScanID:     &finishedID,
				LastFinishedScanResult: &failedResult,
			},
		},
		TargetScan: &target,
		Sources:    []SourceScanStatus{queuedSource, failedSource},
	}
	if err := validSnapshot.Validate(); err != nil {
		t.Fatalf("valid snapshot rejected: %v", err)
	}

	queuedState := ScanState{
		StatusRevision:                 2,
		State:                          ScanLifecycleRunning,
		ActiveScanID:                   scanStringPtr("active"),
		FollowupScanID:                 scanStringPtr("queued"),
		FollowupState:                  followupStatePtr(FollowupQueued),
		FollowupTrigger:                scanTriggerPtr(ScanTriggerScheduled),
		FollowupRequestedAtMS:          scanInt64Ptr(6),
		FollowupEnqueuedStatusRevision: scanInt64Ptr(2),
	}
	if err := queuedState.Validate(); err != nil {
		t.Fatalf("valid running state with queued follow-up rejected: %v", err)
	}
	startFailedState := queuedState
	startFailedState.State = ScanLifecycleIdle
	startFailedState.ActiveScanID = nil
	startFailedState.FollowupState = followupStatePtr(FollowupStartFailed)
	startFailedState.FollowupErrorCode = scanStringPtr("SCANNER_UNAVAILABLE")
	if err := startFailedState.Validate(); err != nil {
		t.Fatalf("valid start-failed follow-up rejected: %v", err)
	}

	invalidSnapshots := []struct {
		name  string
		value ScanStatusSnapshot
	}{
		{"unsorted sources", ScanStatusSnapshot{AppState: validSnapshot.AppState, Sources: []SourceScanStatus{failedSource, queuedSource}}},
		{"duplicate sources", ScanStatusSnapshot{AppState: validSnapshot.AppState, Sources: []SourceScanStatus{queuedSource, queuedSource}}},
		{"failed source without code", ScanStatusSnapshot{AppState: validSnapshot.AppState, Sources: []SourceScanStatus{{Source: SourceCodex, State: SourceScanFailed}}}},
		{"non-failed source with code", ScanStatusSnapshot{AppState: validSnapshot.AppState, Sources: []SourceScanStatus{{Source: SourceCodex, State: SourceScanCompleted, ErrorCode: &failedCode}}}},
		{"negative data revision", ScanStatusSnapshot{AppState: AppState{DataRevision: -1}}},
		{"invalid target scan", ScanStatusSnapshot{AppState: validSnapshot.AppState, TargetScan: &ScanRun{ScanID: "target", Trigger: ScanTriggerManual, RequestKind: ScanRequestDirect, State: ScanRunFailed}}},
	}
	for _, test := range invalidSnapshots {
		t.Run(test.name, func(t *testing.T) {
			if err := test.value.Validate(); err == nil {
				t.Fatal("Validate succeeded")
			}
		})
	}

	invalidStates := []struct {
		name  string
		value ScanState
	}{
		{"running without active ID", ScanState{State: ScanLifecycleRunning}},
		{"idle with active ID", ScanState{State: ScanLifecycleIdle, ActiveScanID: scanStringPtr("active")}},
		{"unpaired finished projection", ScanState{State: ScanLifecycleIdle, LastFinishedScanID: scanStringPtr("finished")}},
		{"same active and follow-up ID", ScanState{State: ScanLifecycleRunning, ActiveScanID: scanStringPtr("same"), FollowupScanID: scanStringPtr("same"), FollowupState: followupStatePtr(FollowupQueued), FollowupTrigger: scanTriggerPtr(ScanTriggerManual), FollowupRequestedAtMS: scanInt64Ptr(1), FollowupEnqueuedStatusRevision: scanInt64Ptr(1)}},
		{"incomplete queued follow-up", ScanState{State: ScanLifecycleIdle, FollowupState: followupStatePtr(FollowupQueued)}},
		{"queued follow-up with error", ScanState{State: ScanLifecycleIdle, FollowupScanID: scanStringPtr("followup"), FollowupState: followupStatePtr(FollowupQueued), FollowupTrigger: scanTriggerPtr(ScanTriggerManual), FollowupRequestedAtMS: scanInt64Ptr(1), FollowupEnqueuedStatusRevision: scanInt64Ptr(1), FollowupErrorCode: scanStringPtr("SOURCE_RUN_FAILED")}},
		{"start-failed follow-up without error", ScanState{State: ScanLifecycleIdle, FollowupScanID: scanStringPtr("followup"), FollowupState: followupStatePtr(FollowupStartFailed), FollowupTrigger: scanTriggerPtr(ScanTriggerManual), FollowupRequestedAtMS: scanInt64Ptr(1), FollowupEnqueuedStatusRevision: scanInt64Ptr(1)}},
		{"negative follow-up time", ScanState{State: ScanLifecycleIdle, FollowupScanID: scanStringPtr("followup"), FollowupState: followupStatePtr(FollowupQueued), FollowupTrigger: scanTriggerPtr(ScanTriggerManual), FollowupRequestedAtMS: scanInt64Ptr(-1), FollowupEnqueuedStatusRevision: scanInt64Ptr(1)}},
	}
	for _, test := range invalidStates {
		t.Run(test.name, func(t *testing.T) {
			if err := test.value.Validate(); err == nil {
				t.Fatal("Validate succeeded")
			}
		})
	}

	validRuns := []ScanRun{
		{ScanID: "queued", Trigger: ScanTriggerManual, RequestKind: ScanRequestFollowup, State: ScanRunQueued, RequestedAtMS: 1, EnqueuedStatusRevision: scanInt64Ptr(1)},
		{ScanID: "running", Trigger: ScanTriggerManual, RequestKind: ScanRequestDirect, State: ScanRunRunning, RequestedAtMS: 1, StartedAtMS: scanInt64Ptr(2), StartedStatusRevision: scanInt64Ptr(2)},
		{ScanID: "completed", Trigger: ScanTriggerManual, RequestKind: ScanRequestDirect, State: ScanRunCompleted, RequestedAtMS: 1, StartedAtMS: scanInt64Ptr(8), StartedStatusRevision: scanInt64Ptr(2), FinishedAtMS: scanInt64Ptr(1), TerminalStatusRevision: scanInt64Ptr(3)},
		{ScanID: "failed", Trigger: ScanTriggerManual, RequestKind: ScanRequestFollowup, State: ScanRunFailed, RequestedAtMS: 1, EnqueuedStatusRevision: scanInt64Ptr(1), StartedAtMS: scanInt64Ptr(2), StartedStatusRevision: scanInt64Ptr(2), FinishedAtMS: scanInt64Ptr(3), TerminalStatusRevision: scanInt64Ptr(3), ErrorCode: scanStringPtr("SOURCE_RUN_FAILED")},
		{ScanID: "start-failed", Trigger: ScanTriggerManual, RequestKind: ScanRequestFollowup, State: ScanRunStartFailed, RequestedAtMS: 1, EnqueuedStatusRevision: scanInt64Ptr(1), FinishedAtMS: scanInt64Ptr(2), TerminalStatusRevision: scanInt64Ptr(2), ErrorCode: scanStringPtr("SCAN_START_FAILED")},
	}
	for _, run := range validRuns {
		if err := run.Validate(); err != nil {
			t.Errorf("valid %s scan run rejected: %v", run.State, err)
		}
	}
	invalidRuns := []ScanRun{
		{ScanID: "queued", Trigger: ScanTriggerManual, RequestKind: ScanRequestDirect, State: ScanRunQueued, RequestedAtMS: 1},
		{ScanID: "running", Trigger: ScanTriggerManual, RequestKind: ScanRequestDirect, State: ScanRunRunning, RequestedAtMS: 1},
		{ScanID: "completed", Trigger: ScanTriggerManual, RequestKind: ScanRequestDirect, State: ScanRunCompleted, RequestedAtMS: 1, StartedAtMS: scanInt64Ptr(1), StartedStatusRevision: scanInt64Ptr(1), FinishedAtMS: scanInt64Ptr(2), TerminalStatusRevision: scanInt64Ptr(2), ErrorCode: scanStringPtr("SOURCE_RUN_FAILED")},
		{ScanID: "failed", Trigger: ScanTriggerManual, RequestKind: ScanRequestDirect, State: ScanRunFailed, RequestedAtMS: 1, StartedAtMS: scanInt64Ptr(1), StartedStatusRevision: scanInt64Ptr(1), FinishedAtMS: scanInt64Ptr(2), TerminalStatusRevision: scanInt64Ptr(2)},
		{ScanID: "start-failed", Trigger: ScanTriggerManual, RequestKind: ScanRequestDirect, State: ScanRunStartFailed, RequestedAtMS: 1, FinishedAtMS: scanInt64Ptr(2), TerminalStatusRevision: scanInt64Ptr(2), ErrorCode: scanStringPtr("SCAN_START_FAILED")},
		{ScanID: "unknown-trigger", Trigger: ScanTrigger("manual"), RequestKind: ScanRequestDirect, State: ScanRunRunning, RequestedAtMS: 1, StartedAtMS: scanInt64Ptr(1), StartedStatusRevision: scanInt64Ptr(1)},
		{ScanID: "negative", Trigger: ScanTriggerManual, RequestKind: ScanRequestDirect, State: ScanRunRunning, RequestedAtMS: -1, StartedAtMS: scanInt64Ptr(1), StartedStatusRevision: scanInt64Ptr(1)},
		{ScanID: "invalid-code", Trigger: ScanTriggerManual, RequestKind: ScanRequestDirect, State: ScanRunFailed, RequestedAtMS: 1, StartedAtMS: scanInt64Ptr(1), StartedStatusRevision: scanInt64Ptr(1), FinishedAtMS: scanInt64Ptr(2), TerminalStatusRevision: scanInt64Ptr(2), ErrorCode: scanStringPtr("bad")},
	}
	for _, run := range invalidRuns {
		if err := run.Validate(); err == nil {
			t.Errorf("invalid %s scan run %q succeeded", run.State, run.ScanID)
		}
	}
}

func TestScanDomainEventValidation(t *testing.T) {
	valid := []struct {
		name     string
		validate func() error
	}{
		{"start", func() error {
			return (ScanStartEvent{ScanID: "scan", Trigger: ScanTriggerManual, RequestedAtMS: 10, StartedAtMS: 1}).Validate()
		}},
		{"reserve follow-up", func() error {
			return (ReserveScanFollowupEvent{FollowupScanID: "followup", Trigger: ScanTriggerScheduled, RequestedAtMS: 1}).Validate()
		}},
		{"follow-up started", func() error { return (FollowupStartedEvent{ScanID: "followup", StartedAtMS: 1}).Validate() }},
		{"follow-up start failed", func() error {
			return (FollowupStartFailedEvent{ScanID: "followup", FailedAtMS: 1, ErrorCode: "SCANNER_UNAVAILABLE"}).Validate()
		}},
		{"completed", func() error { return (ScanCompletedEvent{ScanID: "scan", CompletedAtMS: 1}).Validate() }},
		{"failed", func() error {
			return (ScanFailedEvent{ScanID: "scan", FailedAtMS: 1, ErrorCode: "SOURCE_RUN_FAILED"}).Validate()
		}},
	}
	for _, test := range valid {
		t.Run(test.name, func(t *testing.T) {
			if err := test.validate(); err != nil {
				t.Fatalf("Validate: %v", err)
			}
		})
	}

	invalid := []struct {
		name     string
		validate func() error
	}{
		{"start missing ID", func() error { return (ScanStartEvent{Trigger: ScanTriggerManual}).Validate() }},
		{"start control ID", func() error { return (ScanStartEvent{ScanID: "scan\x00id", Trigger: ScanTriggerManual}).Validate() }},
		{"start unknown trigger", func() error { return (ScanStartEvent{ScanID: "scan", Trigger: ScanTrigger("manual")}).Validate() }},
		{"start negative time", func() error {
			return (ScanStartEvent{ScanID: "scan", Trigger: ScanTriggerManual, StartedAtMS: -1}).Validate()
		}},
		{"reserve blank ID", func() error {
			return (ReserveScanFollowupEvent{FollowupScanID: " \t", Trigger: ScanTriggerManual}).Validate()
		}},
		{"reserve negative time", func() error {
			return (ReserveScanFollowupEvent{FollowupScanID: "followup", Trigger: ScanTriggerManual, RequestedAtMS: -1}).Validate()
		}},
		{"follow-up started negative time", func() error { return (FollowupStartedEvent{ScanID: "followup", StartedAtMS: -1}).Validate() }},
		{"follow-up failed invalid code", func() error { return (FollowupStartFailedEvent{ScanID: "followup", ErrorCode: "lowercase"}).Validate() }},
		{"completed negative time", func() error { return (ScanCompletedEvent{ScanID: "scan", CompletedAtMS: -1}).Validate() }},
		{"failed invalid code", func() error { return (ScanFailedEvent{ScanID: "scan", ErrorCode: "BAD-CODE"}).Validate() }},
	}
	for _, test := range invalid {
		t.Run(test.name, func(t *testing.T) {
			if err := test.validate(); err == nil {
				t.Fatal("Validate succeeded")
			}
		})
	}
}

func scanStringPtr(value string) *string                  { return &value }
func scanInt64Ptr(value int64) *int64                     { return &value }
func scanTriggerPtr(value ScanTrigger) *ScanTrigger       { return &value }
func followupStatePtr(value FollowupState) *FollowupState { return &value }
