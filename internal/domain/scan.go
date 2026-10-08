package domain

import (
	"fmt"
	"strings"
	"unicode"
)

type ScanTrigger string

const (
	ScanTriggerStartup       ScanTrigger = "Startup"
	ScanTriggerScheduled     ScanTrigger = "Scheduled"
	ScanTriggerManual        ScanTrigger = "Manual"
	ScanTriggerSourceChanged ScanTrigger = "SourceChanged"
	ScanTriggerRebuild       ScanTrigger = "Rebuild"
)

func (v ScanTrigger) Validate() error {
	switch v {
	case ScanTriggerStartup, ScanTriggerScheduled, ScanTriggerManual, ScanTriggerSourceChanged, ScanTriggerRebuild:
		return nil
	default:
		return fmt.Errorf("invalid scan_trigger: unknown value %q", v)
	}
}

func ParseScanTrigger(value string) (ScanTrigger, error) {
	parsed := ScanTrigger(value)
	if err := parsed.Validate(); err != nil {
		return "", err
	}
	return parsed, nil
}

type ScanRequestKind string

const (
	ScanRequestDirect   ScanRequestKind = "direct"
	ScanRequestFollowup ScanRequestKind = "followup"
)

func (v ScanRequestKind) Validate() error {
	switch v {
	case ScanRequestDirect, ScanRequestFollowup:
		return nil
	default:
		return fmt.Errorf("invalid scan_request_kind: unknown value %q", v)
	}
}

func ParseScanRequestKind(value string) (ScanRequestKind, error) {
	parsed := ScanRequestKind(value)
	if err := parsed.Validate(); err != nil {
		return "", err
	}
	return parsed, nil
}

type ScanLifecycleState string

const (
	ScanLifecycleIdle    ScanLifecycleState = "idle"
	ScanLifecycleRunning ScanLifecycleState = "running"
	ScanLifecycleFailed  ScanLifecycleState = "failed"
)

func (v ScanLifecycleState) Validate() error {
	switch v {
	case ScanLifecycleIdle, ScanLifecycleRunning, ScanLifecycleFailed:
		return nil
	default:
		return fmt.Errorf("invalid scan_lifecycle_state: unknown value %q", v)
	}
}

func ParseScanLifecycleState(value string) (ScanLifecycleState, error) {
	parsed := ScanLifecycleState(value)
	if err := parsed.Validate(); err != nil {
		return "", err
	}
	return parsed, nil
}

type ScanRunState string

const (
	ScanRunQueued      ScanRunState = "queued"
	ScanRunRunning     ScanRunState = "running"
	ScanRunCompleted   ScanRunState = "completed"
	ScanRunFailed      ScanRunState = "failed"
	ScanRunStartFailed ScanRunState = "start_failed"
)

func (v ScanRunState) Validate() error {
	switch v {
	case ScanRunQueued, ScanRunRunning, ScanRunCompleted, ScanRunFailed, ScanRunStartFailed:
		return nil
	default:
		return fmt.Errorf("invalid scan_run_state: unknown value %q", v)
	}
}

func ParseScanRunState(value string) (ScanRunState, error) {
	parsed := ScanRunState(value)
	if err := parsed.Validate(); err != nil {
		return "", err
	}
	return parsed, nil
}

type SourceScanState string

const (
	SourceScanQueued    SourceScanState = "queued"
	SourceScanRunning   SourceScanState = "running"
	SourceScanCompleted SourceScanState = "completed"
	SourceScanSkipped   SourceScanState = "skipped"
	SourceScanFailed    SourceScanState = "failed"
)

func (v SourceScanState) Validate() error {
	switch v {
	case SourceScanQueued, SourceScanRunning, SourceScanCompleted, SourceScanSkipped, SourceScanFailed:
		return nil
	default:
		return fmt.Errorf("invalid source_scan_state: unknown value %q", v)
	}
}

func ParseSourceScanState(value string) (SourceScanState, error) {
	parsed := SourceScanState(value)
	if err := parsed.Validate(); err != nil {
		return "", err
	}
	return parsed, nil
}

type ScanResult string

const (
	ScanResultCompleted ScanResult = "completed"
	ScanResultFailed    ScanResult = "failed"
)

func (v ScanResult) Validate() error {
	switch v {
	case ScanResultCompleted, ScanResultFailed:
		return nil
	default:
		return fmt.Errorf("invalid scan_result: unknown value %q", v)
	}
}

func ParseScanResult(value string) (ScanResult, error) {
	parsed := ScanResult(value)
	if err := parsed.Validate(); err != nil {
		return "", err
	}
	return parsed, nil
}

type FollowupState string

const (
	FollowupQueued      FollowupState = "queued"
	FollowupStartFailed FollowupState = "start_failed"
)

func (v FollowupState) Validate() error {
	switch v {
	case FollowupQueued, FollowupStartFailed:
		return nil
	default:
		return fmt.Errorf("invalid followup_state: unknown value %q", v)
	}
}

func ParseFollowupState(value string) (FollowupState, error) {
	parsed := FollowupState(value)
	if err := parsed.Validate(); err != nil {
		return "", err
	}
	return parsed, nil
}

type SourceScanStatus struct {
	Source    SourceID
	State     SourceScanState
	ErrorCode *string
}

func (s SourceScanStatus) Validate() error {
	if err := validateID(string(s.Source), "source"); err != nil {
		return err
	}
	if err := s.State.Validate(); err != nil {
		return err
	}
	if (s.State == SourceScanFailed) != (s.ErrorCode != nil) {
		return fmt.Errorf("source_scan error_code must be present only for failed state")
	}
	return validateOptionalErrorCode(s.ErrorCode, "source_scan_error_code")
}

type ScanState struct {
	StatusRevision                 int64
	State                          ScanLifecycleState
	ActiveScanID                   *string
	LastFinishedScanID             *string
	LastFinishedScanResult         *ScanResult
	LastScanStartedAtMS            *int64
	LastScanCompletedAtMS          *int64
	LastScanFailedAtMS             *int64
	LastScanErrorCode              *string
	FollowupScanID                 *string
	FollowupState                  *FollowupState
	FollowupTrigger                *ScanTrigger
	FollowupRequestedAtMS          *int64
	FollowupEnqueuedStatusRevision *int64
	FollowupErrorCode              *string
}

func (s ScanState) Validate() error {
	if err := s.State.Validate(); err != nil {
		return err
	}
	if s.StatusRevision < 0 {
		return fmt.Errorf("invalid status_revision: must be non-negative")
	}
	for _, value := range []struct {
		value *int64
		field string
	}{
		{s.LastScanStartedAtMS, "last_scan_started_at_ms"},
		{s.LastScanCompletedAtMS, "last_scan_completed_at_ms"},
		{s.LastScanFailedAtMS, "last_scan_failed_at_ms"},
		{s.FollowupRequestedAtMS, "followup_requested_at_ms"},
		{s.FollowupEnqueuedStatusRevision, "followup_enqueued_status_revision"},
	} {
		if err := validateOptionalNonNegative(value.value, value.field); err != nil {
			return err
		}
	}
	for _, value := range []struct {
		value *string
		field string
	}{
		{s.ActiveScanID, "active_scan_id"},
		{s.LastFinishedScanID, "last_finished_scan_id"},
		{s.FollowupScanID, "followup_scan_id"},
	} {
		if err := validateOptionalID(value.value, value.field); err != nil {
			return err
		}
	}
	if err := validateOptionalErrorCode(s.LastScanErrorCode, "last_scan_error_code"); err != nil {
		return err
	}
	if err := validateOptionalErrorCode(s.FollowupErrorCode, "followup_error_code"); err != nil {
		return err
	}
	if (s.State == ScanLifecycleRunning) != (s.ActiveScanID != nil) {
		return fmt.Errorf("running scan state and active_scan_id must be present together")
	}
	if (s.LastFinishedScanID == nil) != (s.LastFinishedScanResult == nil) {
		return fmt.Errorf("last_finished_scan_id and last_finished_scan_result must be null together")
	}
	if s.LastFinishedScanResult != nil {
		if err := s.LastFinishedScanResult.Validate(); err != nil {
			return err
		}
	}
	if s.ActiveScanID != nil && s.FollowupScanID != nil && *s.ActiveScanID == *s.FollowupScanID {
		return fmt.Errorf("active_scan_id and followup_scan_id must differ")
	}
	if s.FollowupState == nil {
		if s.FollowupScanID != nil || s.FollowupTrigger != nil || s.FollowupRequestedAtMS != nil ||
			s.FollowupEnqueuedStatusRevision != nil || s.FollowupErrorCode != nil {
			return fmt.Errorf("empty follow-up state requires all follow-up fields to be null")
		}
		return nil
	}
	if err := s.FollowupState.Validate(); err != nil {
		return err
	}
	if s.FollowupTrigger != nil {
		if err := s.FollowupTrigger.Validate(); err != nil {
			return err
		}
	}
	switch *s.FollowupState {
	case FollowupQueued:
		if s.FollowupScanID == nil || s.FollowupTrigger == nil || s.FollowupRequestedAtMS == nil ||
			s.FollowupEnqueuedStatusRevision == nil || s.FollowupErrorCode != nil {
			return fmt.Errorf("queued follow-up requires id, trigger, requested time and revision")
		}
	case FollowupStartFailed:
		if s.FollowupScanID == nil || s.FollowupTrigger == nil || s.FollowupRequestedAtMS == nil ||
			s.FollowupEnqueuedStatusRevision == nil || s.FollowupErrorCode == nil {
			return fmt.Errorf("start-failed follow-up requires queued fields and an error code")
		}
	}
	return nil
}

type AppState struct {
	DataRevision int64
	Scan         ScanState
}

func (s AppState) Validate() error {
	if s.DataRevision < 0 {
		return fmt.Errorf("invalid data_revision: must be non-negative")
	}
	return s.Scan.Validate()
}

type ScanRun struct {
	ScanID                 string
	Trigger                ScanTrigger
	RequestKind            ScanRequestKind
	State                  ScanRunState
	RequestedAtMS          int64
	EnqueuedStatusRevision *int64
	StartedAtMS            *int64
	StartedStatusRevision  *int64
	FinishedAtMS           *int64
	TerminalStatusRevision *int64
	ErrorCode              *string
}

func (r ScanRun) Validate() error {
	if err := validateID(r.ScanID, "scan_id"); err != nil {
		return err
	}
	if err := r.Trigger.Validate(); err != nil {
		return err
	}
	if err := r.RequestKind.Validate(); err != nil {
		return err
	}
	if err := r.State.Validate(); err != nil {
		return err
	}
	if r.RequestedAtMS < 0 {
		return fmt.Errorf("invalid requested_at_ms: must be non-negative")
	}
	for _, value := range []struct {
		value *int64
		field string
	}{
		{r.EnqueuedStatusRevision, "enqueued_status_revision"},
		{r.StartedAtMS, "started_at_ms"},
		{r.StartedStatusRevision, "started_status_revision"},
		{r.FinishedAtMS, "finished_at_ms"},
		{r.TerminalStatusRevision, "terminal_status_revision"},
	} {
		if err := validateOptionalNonNegative(value.value, value.field); err != nil {
			return err
		}
	}
	if err := validateOptionalErrorCode(r.ErrorCode, "error_code"); err != nil {
		return err
	}
	validEnqueuedRevision := r.RequestKind == ScanRequestFollowup && r.EnqueuedStatusRevision != nil ||
		r.RequestKind == ScanRequestDirect && r.EnqueuedStatusRevision == nil
	switch r.State {
	case ScanRunQueued:
		if r.RequestKind != ScanRequestFollowup || r.EnqueuedStatusRevision == nil ||
			r.StartedAtMS != nil || r.StartedStatusRevision != nil || r.FinishedAtMS != nil ||
			r.TerminalStatusRevision != nil || r.ErrorCode != nil {
			return fmt.Errorf("queued scan row requires follow-up enqueue fields only")
		}
	case ScanRunRunning:
		if r.StartedAtMS == nil || r.StartedStatusRevision == nil || r.FinishedAtMS != nil ||
			r.TerminalStatusRevision != nil || r.ErrorCode != nil || !validEnqueuedRevision {
			return fmt.Errorf("running scan row has invalid start or terminal fields")
		}
	case ScanRunCompleted:
		if r.StartedAtMS == nil || r.StartedStatusRevision == nil || r.FinishedAtMS == nil ||
			r.TerminalStatusRevision == nil || r.ErrorCode != nil || !validEnqueuedRevision {
			return fmt.Errorf("completed scan row requires started and terminal fields")
		}
	case ScanRunFailed:
		if r.StartedAtMS == nil || r.StartedStatusRevision == nil || r.FinishedAtMS == nil ||
			r.TerminalStatusRevision == nil || r.ErrorCode == nil || !validEnqueuedRevision {
			return fmt.Errorf("failed scan row requires started, terminal and error fields")
		}
	case ScanRunStartFailed:
		if r.RequestKind != ScanRequestFollowup || r.EnqueuedStatusRevision == nil ||
			r.StartedAtMS != nil || r.StartedStatusRevision != nil || r.FinishedAtMS == nil ||
			r.TerminalStatusRevision == nil || r.ErrorCode == nil {
			return fmt.Errorf("start-failed scan row requires enqueue and terminal fields")
		}
	}
	return nil
}

type ScanStatusSnapshot struct {
	AppState   AppState
	TargetScan *ScanRun
	Sources    []SourceScanStatus
}

func (s ScanStatusSnapshot) Validate() error {
	if err := s.AppState.Validate(); err != nil {
		return err
	}
	if s.TargetScan != nil {
		if err := s.TargetScan.Validate(); err != nil {
			return err
		}
	}
	var previous SourceID
	for i, source := range s.Sources {
		if err := source.Validate(); err != nil {
			return err
		}
		if i > 0 && source.Source <= previous {
			return fmt.Errorf("source scan statuses must be sorted and unique")
		}
		previous = source.Source
	}
	return nil
}

type ScanStartEvent struct {
	ScanID        string
	Trigger       ScanTrigger
	RequestedAtMS int64
	StartedAtMS   int64
}

func (e ScanStartEvent) Validate() error {
	if err := validateID(e.ScanID, "scan_id"); err != nil {
		return err
	}
	if err := e.Trigger.Validate(); err != nil {
		return err
	}
	if e.RequestedAtMS < 0 || e.StartedAtMS < 0 {
		return fmt.Errorf("scan start event timestamps must be non-negative")
	}
	return nil
}

type ReserveScanFollowupEvent struct {
	FollowupScanID string
	Trigger        ScanTrigger
	RequestedAtMS  int64
}

func (e ReserveScanFollowupEvent) Validate() error {
	if err := validateID(e.FollowupScanID, "followup_scan_id"); err != nil {
		return err
	}
	if err := e.Trigger.Validate(); err != nil {
		return err
	}
	if e.RequestedAtMS < 0 {
		return fmt.Errorf("invalid requested_at_ms: must be non-negative")
	}
	return nil
}

type FollowupStartedEvent struct {
	ScanID      string
	StartedAtMS int64
}

func (e FollowupStartedEvent) Validate() error {
	if err := validateID(e.ScanID, "scan_id"); err != nil {
		return err
	}
	if e.StartedAtMS < 0 {
		return fmt.Errorf("invalid started_at_ms: must be non-negative")
	}
	return nil
}

type FollowupStartFailedEvent struct {
	ScanID     string
	FailedAtMS int64
	ErrorCode  string
}

func (e FollowupStartFailedEvent) Validate() error {
	if err := validateID(e.ScanID, "scan_id"); err != nil {
		return err
	}
	if e.FailedAtMS < 0 {
		return fmt.Errorf("invalid failed_at_ms: must be non-negative")
	}
	return ValidateErrorCode(e.ErrorCode)
}

type ScanCompletedEvent struct {
	ScanID        string
	CompletedAtMS int64
}

func (e ScanCompletedEvent) Validate() error {
	if err := validateID(e.ScanID, "scan_id"); err != nil {
		return err
	}
	if e.CompletedAtMS < 0 {
		return fmt.Errorf("invalid completed_at_ms: must be non-negative")
	}
	return nil
}

type ScanFailedEvent struct {
	ScanID     string
	FailedAtMS int64
	ErrorCode  string
}

func (e ScanFailedEvent) Validate() error {
	if err := validateID(e.ScanID, "scan_id"); err != nil {
		return err
	}
	if e.FailedAtMS < 0 {
		return fmt.Errorf("invalid failed_at_ms: must be non-negative")
	}
	return ValidateErrorCode(e.ErrorCode)
}

func ValidateErrorCode(value string) error {
	if len(value) < 1 || len(value) > 64 {
		return fmt.Errorf("error code must be 1-64 bytes")
	}
	if value[0] < 'A' || value[0] > 'Z' {
		return fmt.Errorf("error code must start with an ASCII uppercase letter")
	}
	for _, ch := range []byte(value[1:]) {
		if (ch < 'A' || ch > 'Z') && (ch < '0' || ch > '9') && ch != '_' {
			return fmt.Errorf("error code contains an invalid byte")
		}
	}
	return nil
}

func validateID(value, field string) error {
	if value == "" || strings.TrimSpace(value) == "" {
		return fmt.Errorf("invalid %s: must not be empty", field)
	}
	for _, r := range value {
		if unicode.IsControl(r) {
			return fmt.Errorf("invalid %s: must not contain control characters", field)
		}
	}
	return nil
}

func validateOptionalID(value *string, field string) error {
	if value != nil {
		return validateID(*value, field)
	}
	return nil
}

func validateOptionalNonNegative(value *int64, field string) error {
	if value != nil && *value < 0 {
		return fmt.Errorf("invalid %s: must be non-negative", field)
	}
	return nil
}

func validateOptionalErrorCode(value *string, field string) error {
	if value == nil {
		return nil
	}
	if err := ValidateErrorCode(*value); err != nil {
		return fmt.Errorf("%s: %w", field, err)
	}
	return nil
}
