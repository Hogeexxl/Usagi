package rebuild

import "github.com/Hogeexxl/Usagi/internal/source"

type RootAction uint8

const (
	RootNormalBuild RootAction = iota + 1
	RootReuseQuarantine
)

type MemberAction uint8

const (
	MemberRebuild MemberAction = iota + 1
	MemberCarry
	MemberVerifiedRedundant
)

type MemberRef struct {
	SourceFileID int64
	Generation   int64
}

type MemberRequirement struct {
	SourceFileID           int64
	Generation             int64
	DeviceID               int64
	Inode                  int64
	RequiredThroughOffset  int64
	ObservedRawSize        int64
	ExpectedOwningThreadID *string
	ExpectedRootSessionID  *string
}

type RootReuseDecision uint8

const (
	ReuseNotEligible RootReuseDecision = iota
	ReuseProofValid
)

type RootBuildPlan struct {
	RootSessionID string
	Action        RootAction
	Members       []MemberRef
	Work          []MemberBuildPlan
}

type MemberBuildPlan struct {
	Member MemberRef
	Action MemberAction
}

type ActiveSourceStateProof func(
	tx *source.WriteTx,
	activeEpoch int64,
	sourceFileID int64,
) ([]byte, error)

type WindowReferenceStripper func(
	tx *source.WriteTx,
	target source.UsageWriteTarget,
	eventIDs []string,
) error
