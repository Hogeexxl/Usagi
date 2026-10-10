package rebuild

import (
	"errors"
	"fmt"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

type Trigger uint8

const (
	Bootstrap Trigger = iota + 1
	ParserChanged
	SourceInvalidated
	QuarantineRetry
	FatalIsolation
)

var (
	ErrInvalidTrigger      = errors.New("invalid rebuild trigger")
	ErrNilCoordinatorSeam  = errors.New("rebuild coordinator seam is nil")
	ErrInvalidRebuildInput = errors.New("invalid rebuild input")
)

type Coordinator struct {
	comparator       source.PrivateVisibilityComparator
	activeStateProof ActiveSourceStateProof
	stripWindows     WindowReferenceStripper
}

func NewCoordinator(
	comparator source.PrivateVisibilityComparator,
	activeStateProof ActiveSourceStateProof,
	stripWindows WindowReferenceStripper,
) (*Coordinator, error) {
	if comparator == nil || activeStateProof == nil || stripWindows == nil {
		return nil, ErrNilCoordinatorSeam
	}
	return &Coordinator{
		comparator:       comparator,
		activeStateProof: activeStateProof,
		stripWindows:     stripWindows,
	}, nil
}

// A successful BeginOrResume establishes the source-global Build barrier.
// The caller must discard its previous Active worklist and replan every root.
func (c *Coordinator) BeginOrResume(
	tx *source.WriteTx,
	trigger Trigger,
	parserVersion int64,
	presentSources []MemberRequirement,
	invalidatedSourceIDs []int64,
	committedAtMS int64,
) (int64, error) {
	if c == nil || c.activeStateProof == nil || c.stripWindows == nil || c.comparator == nil {
		return 0, ErrNilCoordinatorSeam
	}
	if tx == nil || tx.Source() != domain.SourceCodex || parserVersion < 0 || committedAtMS < 0 {
		return 0, ErrInvalidRebuildInput
	}
	if err := validateTrigger(trigger); err != nil {
		return 0, err
	}
	if err := validateRequirements(presentSources); err != nil {
		return 0, err
	}
	invalidatedSourceIDs, err := normalizeIDs(invalidatedSourceIDs)
	if err != nil {
		return 0, err
	}
	if err := tx.EnsureUsageEpoch(); err != nil {
		return 0, err
	}
	state, err := tx.UsageEpochState()
	if err != nil {
		return 0, err
	}

	if state.BuildEpoch == nil {
		buildEpoch, err := tx.BeginOrResumeUsageBuild(parserVersion)
		if err != nil {
			return 0, err
		}
		state, err = tx.UsageEpochState()
		if err != nil {
			return 0, err
		}
		if err := freezeInitialManifest(tx, state, buildEpoch, parserVersion, presentSources, c.activeStateProof, committedAtMS); err != nil {
			return 0, err
		}
		if trigger == QuarantineRetry || trigger == FatalIsolation || trigger == SourceInvalidated {
			roots, err := affectedRoots(tx, buildEpoch, state.ActiveEpoch, trigger, invalidatedSourceIDs)
			if err != nil {
				return 0, err
			}
			if len(invalidatedSourceIDs) > 0 || len(roots) > 0 {
				if err := ResetBuildMembersTx(tx, buildEpoch, trigger, invalidatedSourceIDs, roots, presentSources, c.activeStateProof, c.stripWindows, committedAtMS); err != nil {
					return 0, err
				}
			}
		}
		return buildEpoch, nil
	}

	buildEpoch := *state.BuildEpoch
	oldParserVersion := *state.BuildParserVersion
	if oldParserVersion != parserVersion {
		if err := tx.RetargetUsageBuild(buildEpoch, oldParserVersion, parserVersion); err != nil {
			return 0, err
		}
		if err := ReplaceBuildTarget(tx, buildEpoch, parserVersion, presentSources, c.activeStateProof, c.stripWindows, committedAtMS); err != nil {
			return 0, err
		}
		return buildEpoch, nil
	}
	if trigger == SourceInvalidated || trigger == QuarantineRetry || trigger == FatalIsolation {
		roots, err := affectedRoots(tx, buildEpoch, state.ActiveEpoch, trigger, invalidatedSourceIDs)
		if err != nil {
			return 0, err
		}
		if len(invalidatedSourceIDs) > 0 || len(roots) > 0 {
			if err := ResetBuildMembersTx(tx, buildEpoch, trigger, invalidatedSourceIDs, roots, presentSources, c.activeStateProof, c.stripWindows, committedAtMS); err != nil {
				return 0, err
			}
		}
	}
	if err := addNewPresentMembers(tx, buildEpoch, parserVersion, presentSources, c.activeStateProof, committedAtMS); err != nil {
		return 0, err
	}
	return buildEpoch, nil
}

func (c *Coordinator) PlanRoot(
	rootID string,
	members []MemberBuildPlan,
	reuseDecision RootReuseDecision,
) RootBuildPlan {
	plan := RootBuildPlan{RootSessionID: rootID, Action: RootNormalBuild}
	for _, member := range members {
		plan.Members = append(plan.Members, member.Member)
	}
	if reuseDecision == ReuseProofValid {
		plan.Action = RootReuseQuarantine
		return plan
	}
	plan.Work = append(plan.Work, members...)
	return plan
}

func (c *Coordinator) Activate(
	tx *source.WriteTx,
	buildEpoch int64,
	expectedParserVersion int64,
) (source.UsageActivationOutcome, error) {
	if c == nil || c.comparator == nil {
		return source.UsageActivationOutcome{}, ErrNilCoordinatorSeam
	}
	return Activate(tx, buildEpoch, expectedParserVersion, c.comparator)
}

// ActivateWithRedundancyProofs accepts proofs freshly re-streamed by the caller
// for this activation and verifies them in the same WriteTx as epoch activation.
// It does not persist or reuse proofs from earlier RecordVerifiedRedundant calls.
func (c *Coordinator) ActivateWithRedundancyProofs(
	tx *source.WriteTx,
	buildEpoch int64,
	expectedParserVersion int64,
	proofs []RedundancyActivationProof,
) (source.UsageActivationOutcome, error) {
	if c == nil || c.comparator == nil {
		return source.UsageActivationOutcome{}, ErrNilCoordinatorSeam
	}
	return activate(tx, buildEpoch, expectedParserVersion, c.comparator, proofs)
}

func Activate(
	tx *source.WriteTx,
	buildEpoch int64,
	expectedParserVersion int64,
	injectedComparator source.PrivateVisibilityComparator,
) (source.UsageActivationOutcome, error) {
	return activate(tx, buildEpoch, expectedParserVersion, injectedComparator, nil)
}

func activate(
	tx *source.WriteTx,
	buildEpoch int64,
	expectedParserVersion int64,
	injectedComparator source.PrivateVisibilityComparator,
	redundancyProofs []RedundancyActivationProof,
) (source.UsageActivationOutcome, error) {
	if tx == nil || tx.Source() != domain.SourceCodex || buildEpoch < 1 || expectedParserVersion < 0 || injectedComparator == nil {
		return source.UsageActivationOutcome{}, ErrInvalidRebuildInput
	}
	state, err := tx.UsageEpochState()
	if err != nil {
		return source.UsageActivationOutcome{}, err
	}
	if state.BuildEpoch == nil || *state.BuildEpoch != buildEpoch || state.BuildParserVersion == nil || *state.BuildParserVersion != expectedParserVersion {
		return source.UsageActivationOutcome{}, fmt.Errorf("%w: build epoch/parser pair changed", ErrActivationBlocked)
	}
	if err := verifyActivation(tx, buildEpoch, expectedParserVersion, redundancyProofs); err != nil {
		return source.UsageActivationOutcome{}, err
	}
	outcome, err := tx.ActivateUsageBuildWithPrivateVisibility(buildEpoch, expectedParserVersion, injectedComparator)
	if err != nil {
		return source.UsageActivationOutcome{}, err
	}
	if err := tx.Private(func(private storage.PrivateTx) error {
		_, err := private.Exec("DELETE FROM codex_usage_build_sources WHERE build_epoch=?", buildEpoch)
		return err
	}); err != nil {
		return source.UsageActivationOutcome{}, err
	}
	return outcome, nil
}

func validateTrigger(trigger Trigger) error {
	switch trigger {
	case Bootstrap, ParserChanged, SourceInvalidated, QuarantineRetry, FatalIsolation:
		return nil
	default:
		return ErrInvalidTrigger
	}
}
