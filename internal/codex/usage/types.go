package usage

import (
	"github.com/Hogeexxl/Usagi/internal/codex/rollout"
	"github.com/Hogeexxl/Usagi/internal/source"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

type UsageValueState uint8

const (
	UsageValueMissing UsageValueState = iota
	UsageValueInvalid
	UsageValueValid
)

type UsageValue struct {
	State UsageValueState
	Value sharedusage.NormalizedTokenUsage
}

type ResponseEvidence struct {
	ResponseID       string
	ThreadID         string
	SessionID        string
	TurnID           string
	Usage            UsageValue
	ThreadTokenUsage UsageValue
}

type CompactionEvidence struct {
	ResponseID string
	Latest     *ResponseEvidence
}

type EvidenceKind uint8

const (
	EvidenceExplicit EvidenceKind = iota + 1
	EvidenceLegacy
)

type Operation uint8

const (
	OperationResponse Operation = iota + 1
	OperationCompaction
)

type FatalConflictCode string

const (
	FatalResponseUsage      FatalConflictCode = "RESPONSE_USAGE_CONFLICT"
	FatalResponseOwnership  FatalConflictCode = "RESPONSE_OWNERSHIP_CONFLICT"
	FatalCompactionIdentity FatalConflictCode = "COMPACTION_IDENTITY_CONFLICT"
	FatalLegacyCoverage     FatalConflictCode = "LEGACY_COVERAGE_AMBIGUOUS"
	FatalArithmeticOverflow FatalConflictCode = "ARITHMETIC_OVERFLOW"
)

type FatalConflict struct {
	Code         FatalConflictCode
	SourceFileID int64
	Generation   int64
	Offset       int64
	ThreadID     string
}

type QuarantineSourceProof struct {
	SourceFileID int64
	Generation   int64
	DeviceID     int64
	Inode        int64
	ObservedSize int64
}

type OccurrenceWrite struct {
	SourceFileID int64
	Generation   int64
	StartOffset  int64
	EndOffset    int64
	EventID      string
}

type EventFactWrite struct {
	EventID        string
	OwningThreadID string
	ResponseID     *string
	EvidenceKind   EvidenceKind
	Operation      Operation
}

type MarkerUnknownReason string

const (
	MarkerUsageMissing    MarkerUnknownReason = "usage_missing"
	MarkerIdentityMissing MarkerUnknownReason = "identity_missing"
	MarkerUsageInvalid    MarkerUnknownReason = "usage_invalid"
	MarkerTimeMissing     MarkerUnknownReason = "time_missing"
	MarkerModelUnresolved MarkerUnknownReason = "model_unresolved"
)

type CompactionMarkerWrite struct {
	SourceFileID    int64
	Generation      int64
	StartOffset     int64
	EndOffset       int64
	OwningThreadID  string
	RootSessionID   string
	OccurredAtMS    *int64
	Model           *string
	ReasoningEffort *string
	ResponseID      *string
	ResolvedEventID *string
	UnknownReason   *MarkerUnknownReason
}

type PrivateRowKey struct {
	SourceFileID int64
	Generation   int64
	StartOffset  int64
}

type WindowWrite struct {
	SourceFileID   int64
	Generation     int64
	StartOffset    int64
	EndOffset      int64
	OwningThreadID string
	TurnKey        *string
	StateJSON      []byte
}

type HoldReason string

const (
	HoldReplay HoldReason = "replay"
	HoldCarry  HoldReason = "carry"
)

type HoldWrite struct {
	SourceFileID int64
	Generation   int64
	EventID      string
	Reason       HoldReason
}

type HoldKey struct {
	SourceFileID int64
	Generation   int64
	EventID      string
}

type TurnValueState string

const (
	TurnValueNone   TurnValueState = "none"
	TurnValueSingle TurnValueState = "single"
	TurnValueMixed  TurnValueState = "mixed"
)

type TurnStatus string

const (
	TurnOpen      TurnStatus = "open"
	TurnCompleted TurnStatus = "completed"
	TurnAborted   TurnStatus = "aborted"
	TurnFailed    TurnStatus = "failed"
)

type CompensationBlocks struct {
	StartMissing    bool
	TimeMissing     bool
	Reset           bool
	OwnershipGap    bool
	ParserGap       bool
	RequiredInvalid bool
	ModelUnresolved bool
}

type TurnWrite struct {
	SourceFileID                  int64
	Generation                    int64
	TurnKey                       string
	ThreadID                      string
	RawTurnID                     *string
	StartedAtMS                   *int64
	EndedAtMS                     *int64
	StartOffset                   int64
	EndOffset                     *int64
	Status                        TurnStatus
	StartTotal                    *sharedusage.NormalizedTokenUsage
	LastTotal                     *sharedusage.NormalizedTokenUsage
	Accounted                     sharedusage.NormalizedTokenUsage
	AccountedCandidateCount       int64
	ModelState                    TurnValueState
	SingleModel                   *string
	UnresolvedModelSeen           bool
	ReasoningEffortState          TurnValueState
	SingleReasoningEffort         *string
	UnresolvedReasoningEffortSeen bool
	Blocks                        CompensationBlocks
	QualityStatus                 string
	StateThroughOffset            int64
	UpdatedAtMS                   int64
}

type RawTailStatus string

const (
	RawTailUnverified RawTailStatus = "unverified"
	RawTailNone       RawTailStatus = "none"
	RawTailHalfLine   RawTailStatus = "half_line"
)

type ContinuationState string

const (
	ContinuationReplayedAncestor ContinuationState = "replayed_ancestor"
	ContinuationOwningLive       ContinuationState = "owning_live"
)

type ChainState string

const (
	ChainContinuous  ChainState = "continuous"
	ChainInterrupted ChainState = "interrupted"
)

type ChainBlockReason string

const (
	ChainBlockMalformed    ChainBlockReason = "malformed"
	ChainBlockOversized    ChainBlockReason = "oversized"
	ChainBlockTotalInvalid ChainBlockReason = "total_invalid"
	ChainBlockOwnershipGap ChainBlockReason = "ownership_gap"
	ChainBlockParserGap    ChainBlockReason = "parser_gap"
)

type SourceState struct {
	SourceFileID                int64
	Generation                  int64
	DeviceID                    int64
	Inode                       int64
	UsageParserVersion          int64
	CanonicalAlgorithmVersion   int64
	ResolvedThroughOffset       int64
	ObservedRawSize             int64
	RawTailStatus               RawTailStatus
	RawTailStartOffset          *int64
	OwningThreadID              string
	RootSessionID               string
	ContinuationState           ContinuationState
	PreviousTotal               *sharedusage.NormalizedTokenUsage
	PreviousTotalOffset         *int64
	ChainState                  ChainState
	ChainBlockReason            *ChainBlockReason
	ActiveTurnKey               *string
	ActiveModel                 *string
	ActiveModelOffset           *int64
	ActiveReasoningEffort       *string
	ActiveReasoningEffortOffset *int64
	UpdatedAtMS                 int64
	ReconciliationStateJSON     []byte
}

type ReconcileResult struct {
	DeleteEventIDs []string
	Events         []sharedusage.CanonicalUsageEventWrite
	Occurrences    []OccurrenceWrite
	Facts          []EventFactWrite
	MarkerUpserts  []CompactionMarkerWrite
	MarkerDeletes  []PrivateRowKey
	WindowUpserts  []WindowWrite
	WindowDeletes  []PrivateRowKey
	HoldUpserts    []HoldWrite
	HoldDeletes    []HoldKey
	TurnUpserts    []TurnWrite
	SourceState    SourceState
	Fatal          *FatalConflict
}

type CompactionVisibleEvent struct {
	EventID         string
	ThreadID        string
	RootSessionID   string
	Model           string
	ReasoningEffort *string
	OccurredAtMS    int64
	TotalTokens     int64
}

type CompactionUnknownScope struct {
	ThreadID        string
	RootSessionID   string
	Model           string
	ReasoningEffort *string
	StartMS         *int64
	EndMS           *int64
}

type CompactionVisibilityProjection struct {
	Ready         bool
	Events        []CompactionVisibleEvent
	UnknownScopes []CompactionUnknownScope
}

type BindingReconcileDeps struct {
	InvalidateBuild   BuildBindingInvalidator
	ProjectCompaction CompactionVisibilityProjector
}

type BuildBindingInvalidationRequest struct {
	ThreadID                string
	PreviousRoot            *string
	NextRoot                *string
	BindingChangedSourceIDs []int64
	CommittedAtMS           int64
}

type BuildBindingInvalidationResult struct {
	InvalidatedSourceFileIDs []int64
	RetryRootIDs             []string
}

type BuildBindingInvalidator func(
	tx *source.WriteTx,
	req BuildBindingInvalidationRequest,
) (BuildBindingInvalidationResult, error)

type CompactionVisibilityProjector func(
	tx *source.WriteTx,
	target source.UsageWriteTarget,
	ownerThreadID string,
) (CompactionVisibilityProjection, error)

type ReconciliationCarry struct {
	Version               uint8
	OpenWindowStartOffset *uint64
	PendingResponseIDs    []string
	ModernCounterDomain   *ModernCounterDomain
	ModernCounterTotal    *sharedusage.NormalizedTokenUsage
	PendingEvidence       []PendingUsageEvidence
}

type ModernCounterDomain struct {
	ThreadID  string
	SessionID *string
}

type PendingEvidenceKind string

const (
	PendingResponseUsage PendingEvidenceKind = "response_usage"
	PendingCompacted     PendingEvidenceKind = "compacted"
)

type PendingEvidenceRecord struct {
	Kind        PendingEvidenceKind
	TimestampMS *int64
	StartOffset uint64
	EndOffset   uint64
	Response    *ResponseEvidence
	Compaction  *CompactionEvidence
}

type PendingUsageEvidence struct {
	Record          PendingEvidenceRecord
	Model           *string
	ReasoningEffort *string
}

type OwnedRecord struct {
	Parsed            ParsedRecord
	Ownership         rollout.Ownership
	PhysicalEndOffset int64
}

type UsageCandidate struct {
	SourceFileID    int64
	Generation      int64
	EventID         string
	EventKind       byte
	EvidenceKind    EvidenceKind
	Operation       Operation
	OccurredAtMS    int64
	StartOffset     int64
	EndOffset       int64
	OwningThreadID  string
	RootSessionID   string
	TurnKey         *string
	Model           string
	ReasoningEffort *string
	Response        *ResponseEvidence
	PreviousTotal   *sharedusage.NormalizedTokenUsage
	CurrentTotal    *sharedusage.NormalizedTokenUsage
	Usage           sharedusage.NormalizedTokenUsage
}

type ProcessBatch struct {
	Candidates         []UsageCandidate
	Occurrences        []OccurrenceWrite
	Compactions        []PendingUsageEvidence
	TurnUpserts        []TurnWrite
	SourceState        SourceState
	LogicalSafeOffset  int64
	StopBeforeOffset   *int64
	UnresolvedBoundary bool
	Fatal              *FatalConflict
}
