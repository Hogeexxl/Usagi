package source

import (
	"context"
	"errors"
	"fmt"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/storage"
	"github.com/Hogeexxl/Usagi/internal/usage"
)

type Storage struct {
	db         *storage.DB
	scanID     string
	source     domain.SourceID
	storageCtx context.Context
}

func (s *Storage) LoadUsageEpoch() (domain.SourceUsageEpochState, bool, error) {
	return s.db.GetSourceUsageEpoch(s.storageCtx, s.source)
}

func (s *Storage) PrivateRead(fn func(storage.PrivateReader) error) error {
	if fn == nil {
		return ErrNilSourceCallback
	}
	return s.db.PrivateRead(s.storageCtx, fn)
}

func (s *Storage) Write(fn func(*WriteTx) error) error {
	if fn == nil {
		return ErrNilSourceCallback
	}
	return s.db.WriteTx(s.storageCtx, func(raw *storage.Tx) error {
		wrapped := &WriteTx{
			raw:        raw,
			scanID:     s.scanID,
			source:     s.source,
			storageCtx: s.storageCtx,
		}
		defer func() {
			wrapped.closed = true
			wrapped.raw = nil
			wrapped.storageCtx = nil
		}()

		if err := fn(wrapped); err != nil {
			return err
		}
		if wrapped.poisoned {
			return ErrTransactionPoisoned
		}
		return nil
	})
}

type UsageWriteTarget uint8

const (
	UsageTargetInvalid UsageWriteTarget = iota
	UsageTargetActive
	UsageTargetBuild
)

type UsageActivationOutcome struct {
	ActiveEpoch    int64
	DataRevision   int64
	VisibleChanged bool
}

type PrivateVisibilityComparator func(
	tx storage.PrivateTx,
	source domain.SourceID,
	activeEpoch int64,
	activeParserVersion int64,
	buildEpoch int64,
	buildParserVersion int64,
) (bool, error)

type WriteTx struct {
	raw        *storage.Tx
	scanID     string
	source     domain.SourceID
	storageCtx context.Context
	closed     bool
	poisoned   bool
}

var (
	ErrTransactionClosed              = errors.New("source write transaction is closed")
	ErrTransactionPoisoned            = errors.New("source write transaction is poisoned")
	ErrSourceMismatch                 = errors.New("source-bound storage mismatch")
	ErrNilSourceCallback              = errors.New("source callback is nil")
	ErrNilPrivateVisibilityComparator = errors.New("private visibility comparator is nil")
)

func (tx *WriteTx) requireOpen() error {
	if tx.closed || tx.raw == nil {
		return ErrTransactionClosed
	}
	if tx.poisoned {
		return ErrTransactionPoisoned
	}
	return nil
}

func (tx *WriteTx) Source() domain.SourceID {
	return tx.source
}

func (tx *WriteTx) ScanID() string {
	return tx.scanID
}

func (tx *WriteTx) Private(fn func(storage.PrivateTx) error) error {
	if err := tx.requireOpen(); err != nil {
		return err
	}
	if fn == nil {
		return ErrNilSourceCallback
	}
	err := tx.raw.Private(tx.storageCtx, fn)
	if err != nil {
		tx.poisoned = true
	}
	return err
}

func (tx *WriteTx) UpsertThreadNoRevision(
	identity domain.SessionIdentity,
	patch domain.ResolvedThreadPatch,
) (storage.ThreadMutationOutcome, error) {
	if err := tx.requireOpen(); err != nil {
		return storage.ThreadMutationOutcome{}, err
	}
	if identity.Source != tx.source || patch.Source != tx.source {
		return storage.ThreadMutationOutcome{}, ErrSourceMismatch
	}
	return tx.raw.UpsertThread(tx.storageCtx, identity, patch)
}

func (tx *WriteTx) UpsertThread(
	identity domain.SessionIdentity,
	patch domain.ResolvedThreadPatch,
) error {
	if err := tx.requireOpen(); err != nil {
		return err
	}
	outcome, err := tx.UpsertThreadNoRevision(identity, patch)
	if err != nil {
		return err
	}
	if outcome.VisibleChanged {
		_, err = tx.BumpDataRevision()
	}
	return err
}

func (tx *WriteTx) CompareUsageNoRevision(
	target UsageWriteTarget,
	event usage.CanonicalUsageEventWrite,
) (storage.UsageEventMatch, error) {
	if err := tx.requireOpen(); err != nil {
		return storage.UsageEventAbsent, err
	}
	epoch, err := tx.resolveUsageWriteEpoch(target)
	if err != nil {
		return storage.UsageEventAbsent, err
	}
	return tx.raw.CompareUsageEvent(tx.storageCtx, tx.source, epoch, event)
}

func (tx *WriteTx) WriteUsageNoRevision(
	target UsageWriteTarget,
	event usage.CanonicalUsageEventWrite,
) (storage.UsageWriteOutcome, error) {
	if err := tx.requireOpen(); err != nil {
		return storage.UsageInserted, err
	}
	epoch, err := tx.resolveUsageWriteEpoch(target)
	if err != nil {
		return storage.UsageInserted, err
	}
	return tx.raw.WriteUsageEvent(tx.storageCtx, tx.source, epoch, event)
}

func (tx *WriteTx) WriteUsage(target UsageWriteTarget, event usage.CanonicalUsageEventWrite) error {
	if err := tx.requireOpen(); err != nil {
		return err
	}
	outcome, err := tx.WriteUsageNoRevision(target, event)
	if err != nil {
		return err
	}
	if target == UsageTargetActive && outcome == storage.UsageInserted {
		_, err = tx.BumpDataRevision()
	}
	return err
}

func (tx *WriteTx) CopyUsageNoRevision(
	from UsageWriteTarget,
	to UsageWriteTarget,
	eventID string,
) (storage.UsageWriteOutcome, error) {
	if err := tx.requireOpen(); err != nil {
		return storage.UsageInserted, err
	}
	if err := validateUsageWriteTarget(from); err != nil {
		return storage.UsageInserted, err
	}
	if err := validateUsageWriteTarget(to); err != nil {
		return storage.UsageInserted, err
	}
	fromEpoch, err := tx.resolveUsageWriteEpoch(from)
	if err != nil {
		return storage.UsageInserted, err
	}
	toEpoch, err := tx.resolveUsageWriteEpoch(to)
	if err != nil {
		return storage.UsageInserted, err
	}
	if fromEpoch == toEpoch {
		return storage.UsageInserted, fmt.Errorf("canonical usage copy requires distinct epochs")
	}
	return tx.raw.CopyUsageEvent(tx.storageCtx, tx.source, fromEpoch, toEpoch, eventID)
}

func (tx *WriteTx) CopyUsage(
	from UsageWriteTarget,
	to UsageWriteTarget,
	eventID string,
) (storage.UsageWriteOutcome, error) {
	if err := tx.requireOpen(); err != nil {
		return storage.UsageInserted, err
	}
	outcome, err := tx.CopyUsageNoRevision(from, to, eventID)
	if err != nil {
		return outcome, err
	}
	if to == UsageTargetActive && outcome == storage.UsageInserted {
		if _, err := tx.BumpDataRevision(); err != nil {
			return outcome, err
		}
	}
	return outcome, nil
}

func (tx *WriteTx) DeleteUsageNoRevision(
	target UsageWriteTarget,
	eventIDs []string,
) (int, error) {
	if err := tx.requireOpen(); err != nil {
		return 0, err
	}
	epoch, err := tx.resolveUsageWriteEpoch(target)
	if err != nil {
		return 0, err
	}
	return tx.raw.DeleteUsageEvents(tx.storageCtx, tx.source, epoch, eventIDs)
}

func (tx *WriteTx) DeleteUsage(target UsageWriteTarget, eventIDs []string) (int, error) {
	if err := tx.requireOpen(); err != nil {
		return 0, err
	}
	deleted, err := tx.DeleteUsageNoRevision(target, eventIDs)
	if err != nil {
		return 0, err
	}
	if target == UsageTargetActive && deleted > 0 {
		if _, err := tx.BumpDataRevision(); err != nil {
			return 0, err
		}
	}
	return deleted, nil
}

func (tx *WriteTx) DeleteInactiveUsageNoRevision(expectedEpoch int64, eventIDs []string) (int, error) {
	if err := tx.requireOpen(); err != nil {
		return 0, err
	}
	state, err := tx.raw.SourceUsageEpoch(tx.storageCtx, tx.source)
	if err != nil {
		return 0, err
	}
	if expectedEpoch <= 0 {
		return 0, fmt.Errorf("inactive usage epoch must be positive")
	}
	if expectedEpoch == state.ActiveEpoch || (state.BuildEpoch != nil && expectedEpoch == *state.BuildEpoch) {
		return 0, fmt.Errorf("cannot delete active or build usage epoch")
	}
	return tx.raw.DeleteUsageEvents(tx.storageCtx, tx.source, expectedEpoch, eventIDs)
}

func (tx *WriteTx) RebindUsageRootNoRevision(
	target UsageWriteTarget,
	threadID string,
	nextRootSessionID string,
) (int, error) {
	if err := tx.requireOpen(); err != nil {
		return 0, err
	}
	epoch, err := tx.resolveUsageWriteEpoch(target)
	if err != nil {
		return 0, err
	}
	return tx.raw.RebindUsageRoot(tx.storageCtx, tx.source, epoch, threadID, nextRootSessionID)
}

func (tx *WriteTx) EnsureUsageEpoch() error {
	if err := tx.requireOpen(); err != nil {
		return err
	}
	return tx.raw.EnsureSourceUsageEpoch(tx.storageCtx, tx.source)
}

func (tx *WriteTx) UsageEpochState() (domain.SourceUsageEpochState, error) {
	if err := tx.requireOpen(); err != nil {
		return domain.SourceUsageEpochState{}, err
	}
	return tx.raw.SourceUsageEpoch(tx.storageCtx, tx.source)
}

func (tx *WriteTx) BeginOrResumeUsageBuild(parserVersion int64) (int64, error) {
	if err := tx.requireOpen(); err != nil {
		return 0, err
	}
	return tx.raw.BeginOrResumeSourceUsageBuild(tx.storageCtx, tx.source, parserVersion)
}

func (tx *WriteTx) ResolveUsageWriteEpoch(target UsageWriteTarget) (int64, error) {
	if err := tx.requireOpen(); err != nil {
		return 0, err
	}
	return tx.resolveUsageWriteEpoch(target)
}

func (tx *WriteTx) resolveUsageWriteEpoch(target UsageWriteTarget) (int64, error) {
	if err := tx.requireOpen(); err != nil {
		return 0, err
	}
	if err := validateUsageWriteTarget(target); err != nil {
		return 0, err
	}
	state, err := tx.raw.SourceUsageEpoch(tx.storageCtx, tx.source)
	if err != nil {
		return 0, err
	}
	if target == UsageTargetActive {
		if state.ActiveEpoch <= 0 {
			return 0, fmt.Errorf("active usage epoch is not initialized")
		}
		return state.ActiveEpoch, nil
	}
	if state.BuildEpoch == nil {
		return 0, fmt.Errorf("usage build epoch is not active")
	}
	return *state.BuildEpoch, nil
}

func validateUsageWriteTarget(target UsageWriteTarget) error {
	switch target {
	case UsageTargetActive, UsageTargetBuild:
		return nil
	default:
		return fmt.Errorf("invalid usage write target")
	}
}

func (tx *WriteTx) RetargetUsageBuild(
	expectedBuildEpoch int64,
	expectedOldParserVersion int64,
	newParserVersion int64,
) error {
	if err := tx.requireOpen(); err != nil {
		return err
	}
	return tx.raw.RetargetSourceUsageBuild(
		tx.storageCtx,
		tx.source,
		expectedBuildEpoch,
		expectedOldParserVersion,
		newParserVersion,
	)
}

func (tx *WriteTx) ActivateUsageBuildWithPrivateVisibility(
	expectedBuildEpoch int64,
	expectedParserVersion int64,
	comparator PrivateVisibilityComparator,
) (UsageActivationOutcome, error) {
	if err := tx.requireOpen(); err != nil {
		return UsageActivationOutcome{}, err
	}
	if comparator == nil {
		return UsageActivationOutcome{}, ErrNilPrivateVisibilityComparator
	}
	state, err := tx.raw.SourceUsageEpoch(tx.storageCtx, tx.source)
	if err != nil {
		return UsageActivationOutcome{}, err
	}
	if state.BuildEpoch == nil || state.BuildParserVersion == nil ||
		*state.BuildEpoch != expectedBuildEpoch || *state.BuildParserVersion != expectedParserVersion ||
		*state.BuildEpoch < 1 {
		return UsageActivationOutcome{}, fmt.Errorf("usage build activation expected pair mismatch")
	}
	canonicalEqual, err := tx.raw.CanonicalUsageProjectionEqual(
		tx.storageCtx,
		tx.source,
		state.ActiveEpoch,
		*state.BuildEpoch,
	)
	if err != nil {
		return UsageActivationOutcome{}, err
	}
	privateEqual := false
	if canonicalEqual {
		privateEqual, err = tx.runPrivateComparator(comparator, state)
		if err != nil {
			return UsageActivationOutcome{}, err
		}
	}
	visibleChanged := !(canonicalEqual && privateEqual)
	if err := tx.raw.ActivateSourceUsageBuild(
		tx.storageCtx,
		tx.source,
		expectedBuildEpoch,
		expectedParserVersion,
	); err != nil {
		return UsageActivationOutcome{}, err
	}
	if visibleChanged {
		if _, err := tx.BumpDataRevision(); err != nil {
			return UsageActivationOutcome{}, err
		}
	}
	revisions, err := tx.raw.Revisions(tx.storageCtx)
	if err != nil {
		return UsageActivationOutcome{}, err
	}
	return UsageActivationOutcome{
		ActiveEpoch:    *state.BuildEpoch,
		DataRevision:   revisions.DataRevision,
		VisibleChanged: visibleChanged,
	}, nil
}

func (tx *WriteTx) runPrivateComparator(
	comparator PrivateVisibilityComparator,
	state domain.SourceUsageEpochState,
) (bool, error) {
	var equal bool
	err := tx.Private(func(privateTx storage.PrivateTx) error {
		value, err := comparator(
			privateTx,
			tx.source,
			state.ActiveEpoch,
			state.ActiveParserVersion,
			*state.BuildEpoch,
			*state.BuildParserVersion,
		)
		equal = value
		return err
	})
	return equal, err
}

func (tx *WriteTx) ActivateUsageBuild(
	expectedBuildEpoch int64,
	expectedParserVersion int64,
) (UsageActivationOutcome, error) {
	if err := tx.requireOpen(); err != nil {
		return UsageActivationOutcome{}, err
	}
	return tx.ActivateUsageBuildWithPrivateVisibility(
		expectedBuildEpoch,
		expectedParserVersion,
		func(storage.PrivateTx, domain.SourceID, int64, int64, int64, int64) (bool, error) {
			return true, nil
		},
	)
}

func (tx *WriteTx) BumpDataRevision() (int64, error) {
	if err := tx.requireOpen(); err != nil {
		return 0, err
	}
	return tx.raw.BumpDataRevision(tx.storageCtx)
}

func (tx *WriteTx) BumpStatusRevision() (int64, error) {
	if err := tx.requireOpen(); err != nil {
		return 0, err
	}
	return tx.raw.BumpStatusRevision(tx.storageCtx)
}
