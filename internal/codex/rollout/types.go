package rollout

import "github.com/Hogeexxl/Usagi/internal/source"

type Area uint8

const (
	AreaSessions Area = iota + 1
	AreaArchived
)

type RegionState uint8

const (
	RegionComplete RegionState = iota + 1
	RegionUnavailable
)

type PhysicalIdentity struct {
	DeviceID int64
	Inode    int64
}

type DiscoveredFile struct {
	Path              string
	Area              Area
	Identity          PhysicalIdentity
	Size              int64
	MTimeNS           int64
	ThreadIDCandidate string
	Compressed        bool
}

type DiscoverySnapshot struct {
	StartedAtMS int64
	Sessions    RegionState
	Archived    RegionState
	Files       []DiscoveredFile
}

type SourceObservation struct {
	SourceFileID             int64
	Generation               int64
	CurrentPath              string
	Area                     Area
	Identity                 PhysicalIdentity
	Compressed               bool
	BoundThreadID            *string
	AcceptedObservedSize     int64
	AcceptedObservedMTimeNS  int64
	DiscoveryObservedSize    int64
	DiscoveryObservedMTimeNS int64
}

type SourceFileStatus string

const (
	SourceFilePresent  SourceFileStatus = "present"
	SourceFileMissing  SourceFileStatus = "missing"
	SourceFileReplaced SourceFileStatus = "replaced"
)

type AcceptedSourceProof struct {
	SourceFileID    int64
	Generation      int64
	Identity        PhysicalIdentity
	ObservedSize    int64
	ObservedMTimeNS int64
	FileStatus      SourceFileStatus
}

type ActiveCompactionVisibilityProbe func(tx *source.WriteTx) ([]byte, error)

type PlanKind uint8

const (
	PlanSkip PlanKind = iota
	PlanReadFrom
	PlanRebuild
)

type FilePlan struct {
	SourceFileID         int64
	Generation           int64
	Kind                 PlanKind
	PhysicalStartOffset  int64
	PhysicalObservedSize int64
	GuardHash            []byte
	Compressed           bool
	VerifiedRedundant    bool
}

type Record struct {
	SourceFileID       int64
	Generation         int64
	LogicalStartOffset int64
	LogicalEndOffset   int64
	JSON               []byte
}

type GapKind uint8

const (
	GapMalformed GapKind = iota + 1
	GapOversized
	GapOwnership
	GapParser
	GapRequiredInvalid
)

type Gap struct {
	SourceFileID       int64
	Generation         int64
	LogicalStartOffset int64
	LogicalEndOffset   int64
	Kind               GapKind
}

type OwnershipKind uint8

const (
	OwnershipUnknown OwnershipKind = iota
	OwnershipOwning
	OwnershipReplayedAncestor
)

type Ownership struct {
	Kind     OwnershipKind
	ThreadID string
}

type OwnershipPhase uint8

const (
	OwnershipPhaseAwaitOwningMeta OwnershipPhase = iota
	OwnershipPhaseOwningBootstrap
	OwnershipPhaseReplayedAncestor
	OwnershipPhaseOwningLive
)

type OwnershipState struct {
	OwningThreadID string
	Phase          OwnershipPhase
}
