package codex

import (
	"bytes"
	"database/sql"
	"errors"
	"fmt"
	"path/filepath"
	"sort"
	"strings"

	"github.com/Hogeexxl/Usagi/internal/codex/rollout"
	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

const (
	maxMetadataParentDepth = 256
)

type MetadataEvidence struct {
	SourceFileID              int64
	FileGeneration            int64
	MetadataParserVersion     int64
	ResolvedThroughOffset     int64
	OwningThreadID            string
	ContinuationState         string
	UpdatedAtMS               int64
	CWD                       *rollout.MetadataStringCandidate
	CreatedAtMS               *int64
	LatestContextModel        *string
	LatestContextAtMS         *int64
	LatestContextTurnID       *string
	LatestContextRecordOffset *int64
	ParentThreadIDHint        *rollout.MetadataStringCandidate
	AgentRoleHint             *rollout.MetadataStringCandidate
	AgentPath                 *rollout.MetadataStringCandidate
	ReplayStartOffset         *int64
	OwningRecordsStartOffset  *int64
	OwnershipConfidence       string
	QualityStatus             string
	RelationshipConflict      bool
	HasConflict               bool
}

type MetadataAccumulator struct {
	fact       MetadataEvidence
	candidates rollout.OwningThreadCandidates
	state      rollout.OwnershipState
	boundary   rollout.OwnershipBoundary
	gaps       int
}

type MetadataDiagnostic struct {
	Code     string
	ThreadID string
	Field    string
}

type MetadataResolveInput struct {
	State        StateSnapshot
	Session      SessionNameSnapshot
	Global       GlobalStateSnapshot
	RolloutFacts []MetadataEvidence
	Sources      []rollout.SourceObservation
	Existing     []domain.Thread
	ResolvedAtMS int64
}

type MetadataResolveResult struct {
	Patches []domain.ResolvedThreadPatch
	// AffectedThreadIDs includes patch targets, diagnostic targets, and both ends of proven source binding changes.
	AffectedThreadIDs []string
	Diagnostics       []MetadataDiagnostic
}

type ParentChoice struct {
	ThreadID   string
	Confirmed  bool
	Unresolved bool
}

type MetadataSourceCommit struct {
	Observation           rollout.SourceObservation
	SafeFact              MetadataEvidence
	AcceptedProof         *rollout.AcceptedSourceProof
	PlainCheckpointOffset int64
	GuardHash             []byte
	CheckpointStatus      string
	CheckpointErrorCode   *string
}

type MetadataThreadCommit struct {
	Patch   *domain.ResolvedThreadPatch
	Sources []MetadataSourceCommit
}

type MetadataCommitBatch struct {
	Threads []MetadataThreadCommit
}

type MetadataUsageBindingReconciler func(
	tx *source.WriteTx,
	threadID string,
	previousRoot *string,
	nextRoot *string,
	bindingChangedSourceIDs []int64,
	committedAtMS int64,
) (visibleChanged bool, needsShadowBuild bool, invalidatedSourceFileIDs []int64, retryRootIDs []string, err error)

type MetadataCommitDeps struct {
	ReconcileUsageBinding   MetadataUsageBindingReconciler
	ProjectActiveCompaction rollout.ActiveCompactionVisibilityProbe
}

type MetadataCommitOutcome struct {
	CommittedThreads         int
	VisibleChanged           bool
	NeedsShadowBuild         bool
	InvalidatedSourceFileIDs []int64
	RetryRootIDs             []string
}

func NewMetadataAccumulator(
	sourceID, generation, parserVersion, committedAtMS int64,
	owner string,
	candidates rollout.OwningThreadCandidates,
	prior *MetadataEvidence,
) *MetadataAccumulator {
	fact := MetadataEvidence{SourceFileID: sourceID, FileGeneration: generation,
		MetadataParserVersion: parserVersion, OwningThreadID: owner,
		ContinuationState: "unstable", OwnershipConfidence: "unresolved",
		QualityStatus: "complete", UpdatedAtMS: committedAtMS}
	state := rollout.OwnershipState{}
	boundary := rollout.OwnershipBoundary{}
	if prior != nil && prior.SourceFileID == sourceID && prior.FileGeneration == generation &&
		prior.MetadataParserVersion == parserVersion && prior.OwningThreadID == owner {
		fact = cloneMetadataEvidence(*prior)
		fact.UpdatedAtMS = committedAtMS
		state.OwningThreadID = owner
		switch fact.ContinuationState {
		case "replayed_ancestor":
			state.Phase = rollout.OwnershipPhaseReplayedAncestor
		case "owning_live":
			state.Phase = rollout.OwnershipPhaseOwningLive
		default:
			state = rollout.OwnershipState{}
		}
		if fact.OwnershipConfidence == "confirmed" {
			boundary.Confidence = rollout.OwnershipConfidenceConfirmed
		}
		boundary.ReplayStartOffset = copyInt64(fact.ReplayStartOffset)
		boundary.OwningRecordsStartOffset = copyInt64(fact.OwningRecordsStartOffset)
	} else {
		fact.OwningThreadID = owner
	}
	return &MetadataAccumulator{fact: fact, candidates: candidates, state: state, boundary: boundary}
}

// ObserveRecord consumes a single complete record and retains only allowlisted facts.
func (a *MetadataAccumulator) ObserveRecord(record rollout.Record) rollout.RecordClassification {
	classified := rollout.ClassifyRecord(record, a.candidates, a.state, a.boundary)
	a.state, a.boundary = classified.NextState, classified.Boundary
	if classified.NeedsRebuild {
		a.fact.HasConflict = true
	}
	if classified.Ownership.Kind == rollout.OwnershipOwning && classified.Ownership.ThreadID == a.fact.OwningThreadID {
		evidence := classified.Metadata
		if evidence.CreatedAtMS != nil && (a.fact.CreatedAtMS == nil || *evidence.CreatedAtMS < *a.fact.CreatedAtMS) {
			a.fact.CreatedAtMS = copyInt64(evidence.CreatedAtMS)
		}
		mergeMetadataCandidate(&a.fact.CWD, evidence.CWD, &a.fact.HasConflict, cwdCandidatePriority)
		mergeMetadataCandidate(&a.fact.ParentThreadIDHint, evidence.ParentThreadIDHint, &a.fact.HasConflict, parentCandidatePriority)
		mergeMetadataCandidate(&a.fact.AgentRoleHint, evidence.AgentRoleHint, &a.fact.HasConflict, roleCandidatePriority)
		mergeMetadataCandidate(&a.fact.AgentPath, evidence.AgentPath, &a.fact.HasConflict, agentPathCandidatePriority)
		a.fact.RelationshipConflict = a.fact.RelationshipConflict || evidence.RelationshipConflict
		if evidence.LatestContextModel != nil && isLaterMetadataContext(evidence, a.fact) {
			a.fact.LatestContextModel = copyString(evidence.LatestContextModel)
			a.fact.LatestContextTurnID = copyString(evidence.LatestContextTurnID)
			a.fact.LatestContextAtMS = copyInt64(evidence.LatestContextAtMS)
			a.fact.LatestContextRecordOffset = copyInt64(evidence.LatestContextRecordOffset)
		}
	}
	a.updateBoundaryFact()
	return classified
}

func (a *MetadataAccumulator) ObserveGap(gap rollout.Gap) rollout.Ownership {
	a.gaps++
	ownership := rollout.ClassifyGap(gap, a.state, a.boundary)
	if ownership.Kind == rollout.OwnershipUnknown {
		a.state = rollout.OwnershipState{}
		a.boundary.Confidence = rollout.OwnershipConfidenceUnresolved
	}
	a.updateBoundaryFact()
	return ownership
}

func (a *MetadataAccumulator) Snapshot(resolvedThroughOffset, committedAtMS int64) MetadataEvidence {
	a.fact.ResolvedThroughOffset = resolvedThroughOffset
	a.fact.UpdatedAtMS = committedAtMS
	a.updateBoundaryFact()
	if a.fact.HasConflict || a.fact.RelationshipConflict {
		a.fact.QualityStatus = "conflict"
	} else if a.gaps > 0 || a.fact.OwnershipConfidence != "confirmed" {
		a.fact.QualityStatus = "partial"
	}
	return cloneMetadataEvidence(a.fact)
}

func (a *MetadataAccumulator) updateBoundaryFact() {
	a.fact.ReplayStartOffset = copyInt64(a.boundary.ReplayStartOffset)
	a.fact.OwningRecordsStartOffset = copyInt64(a.boundary.OwningRecordsStartOffset)
	if a.boundary.Confidence == rollout.OwnershipConfidenceConfirmed {
		a.fact.OwnershipConfidence = "confirmed"
	} else {
		a.fact.OwnershipConfidence = "unresolved"
		a.fact.ContinuationState = "unstable"
	}
	switch a.state.Phase {
	case rollout.OwnershipPhaseReplayedAncestor:
		a.fact.ContinuationState = "replayed_ancestor"
	case rollout.OwnershipPhaseOwningBootstrap, rollout.OwnershipPhaseOwningLive:
		a.fact.ContinuationState = "owning_live"
	}
}

func cwdCandidatePriority(value rollout.MetadataStringCandidate) int {
	if value.Provenance == "session_meta" {
		return 2
	}
	return 1
}

func cloneMetadataEvidence(value MetadataEvidence) MetadataEvidence {
	value.CWD = copyCandidate(value.CWD)
	value.CreatedAtMS = copyInt64(value.CreatedAtMS)
	value.LatestContextModel = copyString(value.LatestContextModel)
	value.LatestContextAtMS = copyInt64(value.LatestContextAtMS)
	value.LatestContextTurnID = copyString(value.LatestContextTurnID)
	value.LatestContextRecordOffset = copyInt64(value.LatestContextRecordOffset)
	value.ParentThreadIDHint = copyCandidate(value.ParentThreadIDHint)
	value.AgentRoleHint = copyCandidate(value.AgentRoleHint)
	value.AgentPath = copyCandidate(value.AgentPath)
	value.ReplayStartOffset = copyInt64(value.ReplayStartOffset)
	value.OwningRecordsStartOffset = copyInt64(value.OwningRecordsStartOffset)
	return value
}

func ResolveThreadView(input MetadataResolveInput) MetadataResolveResult {
	result := MetadataResolveResult{}
	ids := make(map[string]struct{})
	states := make(map[string]StateThreadFact)
	existing := make(map[string]domain.Thread)
	for _, fact := range input.State.Threads {
		states[fact.ThreadID] = fact
		ids[fact.ThreadID] = struct{}{}
	}
	for _, fact := range input.Session.Facts {
		ids[fact.ThreadID] = struct{}{}
	}
	factsByThread := make(map[string][]MetadataEvidence)
	for _, fact := range input.RolloutFacts {
		factsByThread[fact.OwningThreadID] = append(factsByThread[fact.OwningThreadID], fact)
		ids[fact.OwningThreadID] = struct{}{}
	}
	for _, observation := range input.Sources {
		if observation.BoundThreadID != nil {
			ids[*observation.BoundThreadID] = struct{}{}
		}
	}
	for _, thread := range input.Existing {
		existing[thread.ThreadID] = thread
	}

	parents, parentConflicts := resolveParentChoices(input.State, factsByThread)
	roles := resolveMetadataRoles(input, ids, parents, parentConflicts, factsByThread)
	cycles := metadataCycles(parents)
	trusted := make(map[string]bool)
	for id := range states {
		trusted[id] = true
	}
	for id := range factsByThread {
		trusted[id] = true
	}
	for _, name := range input.Session.Facts {
		trusted[name.ThreadID] = true
	}
	for id := range existing {
		trusted[id] = true
	}

	ordered := make([]string, 0, len(ids))
	for id := range ids {
		if id != "" {
			ordered = append(ordered, id)
		}
	}
	sort.Strings(ordered)
	roots := make(map[string]string)
	rootKnown := make(map[string]bool)
	rootConflict := make(map[string]bool)
	for _, id := range ordered {
		roots[id], rootKnown[id], rootConflict[id] = resolveMetadataRoot(id, parents, roles, cycles, trusted)
	}
	affected := make(map[string]struct{})
	for _, id := range ordered {
		if !rootKnown[id] {
			result.Diagnostics = append(result.Diagnostics, MetadataDiagnostic{Code: "root_unresolved", ThreadID: id, Field: "root_session_id"})
			affected[id] = struct{}{}
		}
		patch, diagnostic := resolveMetadataThread(input, id, states[id], statesHas(states, id), factsByThread[id], roles[id], parents[id], parentConflicts[id], roots[id], rootKnown[id], rootConflict[id], cycles[id], existing[id], hasExisting(existing, id))
		if diagnostic != nil {
			result.Diagnostics = append(result.Diagnostics, *diagnostic)
			affected[id] = struct{}{}
		}
		if patch != nil {
			result.Patches = append(result.Patches, *patch)
			affected[id] = struct{}{}
		}
	}
	for _, fact := range input.RolloutFacts {
		observation, ok := sourceObservationByID(input.Sources, fact.SourceFileID, fact.FileGeneration)
		if !ok || sameOptionalString(observation.BoundThreadID, &fact.OwningThreadID) {
			continue
		}
		affected[fact.OwningThreadID] = struct{}{}
		if observation.BoundThreadID != nil {
			affected[*observation.BoundThreadID] = struct{}{}
		}
	}
	result.AffectedThreadIDs = make([]string, 0, len(affected))
	for id := range affected {
		if id != "" {
			result.AffectedThreadIDs = append(result.AffectedThreadIDs, id)
		}
	}
	sort.Strings(result.AffectedThreadIDs)
	result.AffectedThreadIDs = uniqueStrings(result.AffectedThreadIDs)
	return result
}

func resolveParentChoices(state StateSnapshot, facts map[string][]MetadataEvidence) (map[string]ParentChoice, map[string]bool) {
	byChild := make(map[string]map[string]struct{})
	if state.SpawnEdgesStatus == StateSourceComplete {
		for _, edge := range state.SpawnEdges {
			if byChild[edge.ChildThreadID] == nil {
				byChild[edge.ChildThreadID] = make(map[string]struct{})
			}
			byChild[edge.ChildThreadID][edge.ParentThreadID] = struct{}{}
		}
	}
	ids := make(map[string]struct{})
	for id := range byChild {
		ids[id] = struct{}{}
	}
	for id := range facts {
		ids[id] = struct{}{}
	}
	choices, conflicts := make(map[string]ParentChoice), make(map[string]bool)
	for id := range ids {
		spawnParents := byChild[id]
		if state.SpawnEdgesStatus != StateSourceComplete {
			spawnParents = nil
		}
		levels := []map[string]struct{}{spawnParents, make(map[string]struct{}), make(map[string]struct{}), make(map[string]struct{})}
		for _, fact := range facts[id] {
			if fact.OwnershipConfidence != "confirmed" {
				continue
			}
			if candidate := fact.ParentThreadIDHint; candidate != nil {
				switch candidate.Provenance {
				case "session_meta_parent":
					levels[1][candidate.Value] = struct{}{}
				case "subagent_source":
					levels[2][candidate.Value] = struct{}{}
				case "forked_from_id":
					levels[3][candidate.Value] = struct{}{}
				}
			}
		}
		for level, values := range levels {
			if len(values) > 1 {
				choices[id] = ParentChoice{Unresolved: true}
				conflicts[id] = true
				break
			}
			if len(values) == 1 {
				for parent := range values {
					choices[id] = ParentChoice{ThreadID: parent, Confirmed: true}
				}
				for _, lower := range levels[level+1:] {
					for parent := range lower {
						if parent != choices[id].ThreadID {
							conflicts[id] = true
						}
					}
				}
				break
			}
		}
	}
	return choices, conflicts
}

func resolveMetadataRoles(input MetadataResolveInput, ids map[string]struct{}, parents map[string]ParentChoice, conflicts map[string]bool, facts map[string][]MetadataEvidence) map[string]domain.AgentRole {
	roles := make(map[string]domain.AgentRole, len(ids))
	for id := range ids {
		var explicitMain, explicitSub bool
		for _, fact := range facts[id] {
			if fact.OwnershipConfidence == "confirmed" && fact.AgentRoleHint != nil {
				value := strings.ToLower(fact.AgentRoleHint.Value)
				explicitMain = explicitMain || value == "main"
				explicitSub = explicitSub || value == "subagent"
			}
		}
		if state, ok := findStateFact(input.State.Threads, id); input.State.Status == StateSourceComplete && ok && state.AgentRoleHint != nil {
			value := strings.ToLower(*state.AgentRoleHint)
			explicitMain = explicitMain || value == "main"
			explicitSub = explicitSub || value == "subagent"
		}
		switch {
		case conflicts[id] || explicitMain && (explicitSub || parents[id].Confirmed) || explicitSub && explicitMain:
			roles[id] = "unknown"
		case parents[id].Confirmed && !hasMetadataIdentity(input, parents[id].ThreadID, facts[parents[id].ThreadID]):
			roles[id] = "unknown"
		case parents[id].Confirmed:
			roles[id] = "subagent"
		case explicitSub:
			roles[id] = "unknown"
		case explicitMain || input.State.Status == StateSourceComplete && input.State.SpawnEdgesStatus == StateSourceComplete && (hasMetadataIdentity(input, id, facts[id])):
			roles[id] = "main"
		}
	}
	return roles
}

func metadataCycles(parents map[string]ParentChoice) map[string]bool {
	cycles := make(map[string]bool)
	for start := range parents {
		path := make([]string, 0)
		position := make(map[string]int)
		current := start
		for steps := 0; steps <= maxMetadataParentDepth; steps++ {
			if at, ok := position[current]; ok {
				for _, id := range path[at:] {
					cycles[id] = true
				}
				break
			}
			position[current] = len(path)
			path = append(path, current)
			choice := parents[current]
			if !choice.Confirmed {
				break
			}
			current = choice.ThreadID
		}
	}
	return cycles
}

func resolveMetadataRoot(start string, parents map[string]ParentChoice, roles map[string]domain.AgentRole, cycles, trusted map[string]bool) (string, bool, bool) {
	current := start
	seen := make(map[string]struct{})
	for depth := 0; depth <= maxMetadataParentDepth; depth++ {
		if cycles[current] {
			return "", false, true
		}
		if _, ok := seen[current]; ok {
			return "", false, true
		}
		seen[current] = struct{}{}
		switch roles[current] {
		case "main":
			return current, true, false
		case "unknown":
			return "", false, false
		}
		choice := parents[current]
		if choice.Unresolved {
			return "", false, true
		}
		if !choice.Confirmed {
			return "", false, false
		}
		if !trusted[choice.ThreadID] {
			return "", false, false
		}
		current = choice.ThreadID
	}
	return "", false, true
}

func resolveMetadataThread(input MetadataResolveInput, id string, state StateThreadFact, hasState bool, facts []MetadataEvidence, role domain.AgentRole, parent ParentChoice, parentConflict bool, root string, rootKnown, rootConflict, cycle bool, old domain.Thread, hasOld bool) (*domain.ResolvedThreadPatch, *MetadataDiagnostic) {
	identity, err := domain.NewSessionIdentity(id, domain.SourceCodex, id)
	if err != nil {
		return nil, nil
	}
	patch, err := domain.NewResolvedThreadPatch(identity, input.ResolvedAtMS)
	if err != nil {
		return nil, nil
	}
	stateComplete := input.State.Status == StateSourceComplete
	spawnComplete := input.State.SpawnEdgesStatus == StateSourceComplete
	sessionComplete := input.Session.Status == SessionSourceComplete
	rolloutsComplete := threadRolloutsComplete(id, facts, input.Sources)
	relationshipComplete := stateComplete && spawnComplete && rolloutsComplete && !parentConflict && !cycle && !rootConflict
	quality := domain.MetadataQualityStatus("complete")
	if !stateComplete || !spawnComplete || !sessionComplete || !rolloutsComplete || role == "" || role == "unknown" || !rootKnown ||
		stateHasFieldDiagnostic(input.State, id) || sessionHasDiagnostic(input.Session, id, "") {
		quality = "partial"
	}
	if parentConflict || cycle || rootConflict || anyMetadataFactConflict(facts) {
		quality = "conflict"
	}
	if hasOld && old.Source != domain.SourceCodex {
		return nil, nil
	}
	patch.MetadataQualityStatus = quality

	relationHasConflict := parentConflict || cycle || rootConflict
	if !relationHasConflict {
		switch {
		case role == "main" && hasExplicitMainEvidence(input, id, facts) && (!hasOld || old.ParentThreadID == nil || relationshipComplete):
			if relationshipComplete && hasOld && old.ParentThreadID != nil {
				patch.ParentThreadID = domain.Clear[string]()
			}
			if !hasOld || old.AgentRole != role {
				patch.AgentRole = domain.Set(role)
			}
			setOrClearString(&patch.RootSessionID, id)
		case relationshipComplete && role == "main":
			if hasOld && old.ParentThreadID != nil {
				patch.ParentThreadID = domain.Clear[string]()
			}
			if !hasOld || old.AgentRole != role {
				patch.AgentRole = domain.Set(role)
			}
			setOrClearString(&patch.RootSessionID, id)
		case parent.Confirmed && role == "subagent":
			setOrClearString(&patch.ParentThreadID, parent.ThreadID)
			if !hasOld || old.AgentRole != role {
				patch.AgentRole = domain.Set(role)
			}
			if rootKnown {
				setOrClearString(&patch.RootSessionID, root)
			}
		case relationshipComplete && role == "unknown":
			patch.AgentRole = domain.Set(domain.AgentRole("unknown"))
		}
	}

	title := resolveMetadataTitle(input, id, state, hasState, facts, role, old, hasOld)
	titleClearTrusted := stateFieldClearTrusted(input.State, id, [][]string{{"name", "title"}}, "name", "title") &&
		sessionFieldClearTrusted(input.Session, id, "thread_name") && rolloutsComplete && (role != "unknown" || relationshipComplete)
	if role == "subagent" || role == "unknown" {
		titleClearTrusted = titleClearTrusted && stateFieldClearTrusted(input.State, id, [][]string{{"agent_path"}}, "agent_path")
	}
	setOptionalMetadata(&patch.Title, oldTitle(old, hasOld), title, titleClearTrusted)
	projectPath := metadataProjectPath(state, hasState && stateComplete, facts)
	projectPathClearTrusted := stateFieldClearTrusted(input.State, id, [][]string{{"cwd"}}, "cwd") && rolloutsComplete
	setOptionalMetadata(&patch.ProjectPath, oldProjectPath(old, hasOld), projectPath, projectPathClearTrusted)
	var projectName *string
	if projectPath != nil {
		name := filepath.Base(*projectPath)
		if name != "." && name != string(filepath.Separator) {
			projectName = &name
		}
	}
	setOptionalMetadata(&patch.ProjectName, oldProjectName(old, hasOld), projectName, projectPathClearTrusted)
	projectKind, projectKnown := metadataProjectKind(input.Global, id, projectPath)
	if projectKnown && (!hasOld || old.ProjectKind != projectKind) {
		patch.ProjectKind = domain.Set(projectKind)
	}
	model := metadataModel(state, hasState && stateComplete, facts)
	modelClearTrusted := stateFieldClearTrusted(input.State, id, [][]string{{"model"}}, "model") && rolloutsComplete
	setOptionalMetadata(&patch.MetadataModel, oldModel(old, hasOld), model, modelClearTrusted)
	created := metadataCreated(state, hasState && stateComplete, facts)
	createdClearTrusted := stateFieldClearTrusted(input.State, id, [][]string{{"created_at", "created_at_ms"}}, "created_at") && rolloutsComplete
	setOptionalInt(&patch.CreatedAtMS, oldCreated(old, hasOld), created, createdClearTrusted)
	sessionFact, sessionFactFound := input.Session.Get(id)
	if !sessionFactFound || !sessionComplete {
		sessionFact = SessionNameFact{}
	}
	updated := metadataUpdated(state, hasState && stateComplete, facts, sessionFact)
	updatedClearTrusted := stateFieldClearTrusted(input.State, id, [][]string{{"updated_at", "updated_at_ms"}}, "updated_at") &&
		sessionFieldClearTrusted(input.Session, id, "updated_at") && rolloutsComplete
	setOptionalInt(&patch.UpdatedAtMS, oldUpdated(old, hasOld), updated, updatedClearTrusted)
	archived, archivedKnown := metadataArchived(state, hasState && stateComplete, input.Sources, id)
	if archivedKnown && (!hasOld || old.Archived != archived) {
		patch.Archived = domain.Set(archived)
	}
	patch.FullResolution = patchHasClear(patch)
	if err := patch.Validate(); err != nil {
		return nil, &MetadataDiagnostic{Code: "invalid_resolved_patch", ThreadID: id}
	}
	if !hasPatchMutation(patch) && hasOld && old.MetadataQualityStatus == patch.MetadataQualityStatus {
		return nil, nil
	}
	return &patch, nil
}

func resolveMetadataTitle(input MetadataResolveInput, id string, state StateThreadFact, hasState bool, facts []MetadataEvidence, role domain.AgentRole, old domain.Thread, hasOld bool) *string {
	if input.State.Status == StateSourceComplete && hasState {
		if state.Name != nil {
			return copyString(state.Name)
		}
		if state.Title != nil {
			return copyString(state.Title)
		}
	}
	if input.State.Status == StateSourceComplete && input.Session.Status == SessionSourceComplete {
		if fact, ok := input.Session.Get(id); ok {
			return &fact.ThreadName
		}
	}
	if role != "subagent" || hasOld && old.Title != nil {
		return nil
	}
	var path *string
	if input.State.Status == StateSourceComplete && hasState {
		path = state.AgentPath
	}
	if path == nil {
		for _, fact := range facts {
			if fact.OwnershipConfidence == "confirmed" && fact.AgentPath != nil {
				value := fact.AgentPath.Value
				path = &value
				break
			}
		}
	}
	if path == nil {
		return nil
	}
	base := filepath.Base(*path)
	title := strings.Title(strings.ReplaceAll(strings.ReplaceAll(base, "_", " "), "  ", " "))
	if title == "" || title == "." {
		return nil
	}
	return &title
}

func metadataProjectPath(state StateThreadFact, hasState bool, facts []MetadataEvidence) *string {
	var selected *rollout.MetadataStringCandidate
	for _, fact := range facts {
		candidate := fact.CWD
		if candidate == nil || fact.OwnershipConfidence != "confirmed" {
			continue
		}
		if selected == nil || candidatePriority(candidate.Provenance) > candidatePriority(selected.Provenance) || candidatePriority(candidate.Provenance) == candidatePriority(selected.Provenance) && candidate.Offset < selected.Offset {
			selected = candidate
		}
	}
	if selected != nil {
		value := selected.Value
		return &value
	}
	if hasState && state.CWD != nil {
		return copyString(state.CWD)
	}
	return nil
}

func metadataProjectKind(snapshot GlobalStateSnapshot, id string, path *string) (domain.ProjectKind, bool) {
	switch snapshot.Status {
	case GlobalStateComplete:
		projectless, assigned := snapshot.IsProjectless(id), snapshot.HasAssignment(id)
		if projectless && assigned {
			return "unknown", true
		}
		if projectless {
			return "projectless", true
		}
		if assigned {
			return "project", true
		}
		if path != nil {
			return "project", true
		}
		return "unknown", true
	case GlobalStateNotPresent:
		if path != nil {
			return "project", true
		}
		return "unknown", true
	default:
		return "", false
	}
}

func metadataModel(state StateThreadFact, hasState bool, facts []MetadataEvidence) *string {
	if hasState && state.MetadataModel != nil {
		return copyString(state.MetadataModel)
	}
	var selected *MetadataEvidence
	for i := range facts {
		if facts[i].OwnershipConfidence == "confirmed" && facts[i].LatestContextModel != nil && (selected == nil || isLaterMetadataContext(facts[i].recordEvidence(), *selected)) {
			selected = &facts[i]
		}
	}
	if selected == nil {
		return nil
	}
	return copyString(selected.LatestContextModel)
}

func metadataCreated(state StateThreadFact, hasState bool, facts []MetadataEvidence) *int64 {
	if hasState && state.CreatedAtMS != nil {
		return copyInt64(state.CreatedAtMS)
	}
	var earliest *int64
	for _, fact := range facts {
		if fact.OwnershipConfidence == "confirmed" && fact.CreatedAtMS != nil && (earliest == nil || *fact.CreatedAtMS < *earliest) {
			earliest = fact.CreatedAtMS
		}
	}
	return copyInt64(earliest)
}

func metadataUpdated(state StateThreadFact, hasState bool, facts []MetadataEvidence, session SessionNameFact) *int64 {
	var latest *int64
	if hasState && state.UpdatedAtMS != nil {
		latest = copyInt64(state.UpdatedAtMS)
	}
	for _, fact := range facts {
		if fact.OwnershipConfidence == "confirmed" && fact.LatestContextAtMS != nil && (latest == nil || *fact.LatestContextAtMS > *latest) {
			latest = fact.LatestContextAtMS
		}
	}
	if session.UpdatedAtMS != nil && (latest == nil || *session.UpdatedAtMS > *latest) {
		latest = session.UpdatedAtMS
	}
	return copyInt64(latest)
}

func metadataArchived(state StateThreadFact, hasState bool, sources []rollout.SourceObservation, id string) (bool, bool) {
	if hasState && state.Archived != nil {
		return *state.Archived, true
	}
	for _, source := range sources {
		if source.BoundThreadID != nil && *source.BoundThreadID == id {
			return source.Area == rollout.AreaArchived, true
		}
	}
	return false, false
}

func threadRolloutsComplete(id string, facts []MetadataEvidence, sources []rollout.SourceObservation) bool {
	relevant := make(map[int64]int64)
	for _, observation := range sources {
		if observation.BoundThreadID != nil && *observation.BoundThreadID == id {
			relevant[observation.SourceFileID] = observation.Generation
		}
	}
	for _, fact := range facts {
		relevant[fact.SourceFileID] = fact.FileGeneration
	}
	for sourceID, generation := range relevant {
		found := false
		for _, fact := range facts {
			if fact.SourceFileID == sourceID && fact.FileGeneration == generation && fact.OwningThreadID == id &&
				fact.OwnershipConfidence == "confirmed" && fact.QualityStatus == "complete" && !fact.HasConflict && !fact.RelationshipConflict {
				found = true
				break
			}
		}
		if !found {
			return false
		}
		if _, observed := sourceObservationByID(sources, sourceID, generation); !observed {
			return false
		}
	}
	return true
}

func stateFieldClearTrusted(snapshot StateSnapshot, threadID string, columnGroups [][]string, diagnosticFields ...string) bool {
	if snapshot.Status != StateSourceComplete {
		return false
	}
	for _, group := range columnGroups {
		available := false
		for _, column := range group {
			if snapshot.ThreadColumns[column] {
				available = true
				break
			}
		}
		if !available {
			return false
		}
	}
	for _, diagnostic := range snapshot.Diagnostics {
		if diagnostic.ThreadID == threadID && containsString(diagnosticFields, diagnostic.Field) ||
			diagnostic.ThreadID == "" && diagnostic.Code == "invalid_thread_id" {
			return false
		}
	}
	return true
}

func sessionFieldClearTrusted(snapshot SessionNameSnapshot, threadID, field string) bool {
	if snapshot.Status != SessionSourceComplete {
		return false
	}
	for _, diagnostic := range snapshot.Diagnostics {
		if diagnostic.ThreadID != "" && diagnostic.ThreadID != threadID {
			continue
		}
		switch diagnostic.Code {
		case "invalid_json", "invalid_id", "line_too_large", "file_unreadable":
			return false
		}
		if diagnostic.Field == field {
			return false
		}
	}
	return true
}

func sessionHasDiagnostic(snapshot SessionNameSnapshot, threadID, field string) bool {
	for _, diagnostic := range snapshot.Diagnostics {
		if diagnostic.ThreadID != "" && diagnostic.ThreadID != threadID {
			continue
		}
		if field == "" || diagnostic.Code == "invalid_json" || diagnostic.Code == "invalid_id" ||
			diagnostic.Code == "line_too_large" || diagnostic.Field == field {
			return true
		}
	}
	return false
}

func stateHasFieldDiagnostic(snapshot StateSnapshot, threadID string) bool {
	for _, diagnostic := range snapshot.Diagnostics {
		if diagnostic.ThreadID == threadID && diagnostic.Field != "" {
			return true
		}
	}
	return false
}

func containsString(values []string, value string) bool {
	for _, candidate := range values {
		if candidate == value {
			return true
		}
	}
	return false
}

func sourceObservationByID(sources []rollout.SourceObservation, sourceID, generation int64) (rollout.SourceObservation, bool) {
	for _, observation := range sources {
		if observation.SourceFileID == sourceID && observation.Generation == generation {
			return observation, true
		}
	}
	return rollout.SourceObservation{}, false
}

func anyMetadataFactConflict(facts []MetadataEvidence) bool {
	for _, fact := range facts {
		if fact.HasConflict || fact.RelationshipConflict || fact.QualityStatus == "conflict" {
			return true
		}
	}
	return false
}

func hasExplicitMainEvidence(input MetadataResolveInput, id string, facts []MetadataEvidence) bool {
	for _, fact := range facts {
		if fact.OwnershipConfidence == "confirmed" && fact.AgentRoleHint != nil && strings.EqualFold(fact.AgentRoleHint.Value, "main") {
			return true
		}
	}
	if input.State.Status == StateSourceComplete {
		if fact, ok := findStateFact(input.State.Threads, id); ok && fact.AgentRoleHint != nil && strings.EqualFold(*fact.AgentRoleHint, "main") {
			return true
		}
	}
	return false
}

func CommitMetadata(storageContext *source.Storage, batch MetadataCommitBatch, deps MetadataCommitDeps, committedAtMS int64) (MetadataCommitOutcome, error) {
	if storageContext == nil || committedAtMS < 0 {
		return MetadataCommitOutcome{}, fmt.Errorf("invalid metadata commit input")
	}
	groups := append([]MetadataThreadCommit(nil), batch.Threads...)
	sort.Slice(groups, func(i, j int) bool { return metadataCommitThreadID(groups[i]) < metadataCommitThreadID(groups[j]) })
	outcome := MetadataCommitOutcome{}
	for _, group := range groups {
		threadID := metadataCommitThreadID(group)
		if threadID == "" {
			return outcome, fmt.Errorf("metadata commit has no thread id")
		}
		if group.Patch != nil {
			if err := group.Patch.Validate(); err != nil {
				return outcome, err
			}
			if group.Patch.ThreadID != threadID {
				return outcome, fmt.Errorf("metadata patch thread mismatch")
			}
		}
		var threadChanged, callbackVisible, activeVisible bool
		var needsShadow bool
		var invalidated []int64
		var retryIDs []string
		err := storageContext.Write(func(tx *source.WriteTx) error {
			var currentRoot *string
			if err := tx.Private(func(private storage.PrivateTx) error {
				var value sql.NullString
				err := private.QueryRow("SELECT root_session_id FROM threads WHERE thread_id=?", threadID).Scan(&value)
				if err == nil && value.Valid {
					currentRoot = &value.String
				}
				if errors.Is(err, sql.ErrNoRows) {
					return nil
				}
				return err
			}); err != nil {
				return err
			}
			bindingChanged := make([]int64, 0)
			acceptedPhysicalMutation := false
			for _, commit := range group.Sources {
				observation := commit.Observation
				if commit.SafeFact.SourceFileID != observation.SourceFileID || commit.SafeFact.FileGeneration != observation.Generation || commit.SafeFact.MetadataParserVersion != MetadataParserVersion || commit.SafeFact.OwningThreadID != threadID {
					return fmt.Errorf("metadata safe fact does not match source commit: %d", observation.SourceFileID)
				}
				var currentThread sql.NullString
				var generation, device, inode, size, mtime int64
				var fileStatus, currentPath string
				err := tx.Private(func(private storage.PrivateTx) error {
					return private.QueryRow("SELECT thread_id,file_generation,device_id,inode,observed_size,observed_mtime_ns,file_status,current_path FROM codex_source_files WHERE source_file_id=?", observation.SourceFileID).Scan(&currentThread, &generation, &device, &inode, &size, &mtime, &fileStatus, &currentPath)
				})
				if err != nil {
					return err
				}
				if generation != observation.Generation || device != observation.Identity.DeviceID || inode != observation.Identity.Inode || currentPath != observation.CurrentPath {
					return fmt.Errorf("metadata source precondition changed: %d", observation.SourceFileID)
				}
				previous := nullableString(currentThread)
				if !sameOptionalString(previous, observation.BoundThreadID) {
					return fmt.Errorf("metadata source binding precondition changed: %d", observation.SourceFileID)
				}
				if previous == nil || *previous != commit.SafeFact.OwningThreadID {
					bindingChanged = append(bindingChanged, observation.SourceFileID)
				}
				if commit.AcceptedProof != nil {
					proof := commit.AcceptedProof
					if err := validateMetadataAppendProof(observation, *proof, generation, device, inode, size, mtime, fileStatus); err != nil {
						return err
					}
					if proof.ObservedSize != size || proof.ObservedMTimeNS != mtime {
						acceptedPhysicalMutation = true
					}
				}
			}
			activeBeforeNeeded := acceptedPhysicalMutation || len(bindingChanged) > 0 || metadataPatchAffectsActiveProjection(group.Patch)
			var activeBefore []byte
			if activeBeforeNeeded {
				if deps.ProjectActiveCompaction == nil {
					return fmt.Errorf("metadata active compaction probe is required")
				}
				var err error
				activeBefore, err = deps.ProjectActiveCompaction(tx)
				if err != nil {
					return err
				}
			}
			previousRoot := copyString(currentRoot)
			if group.Patch != nil {
				mutation, err := tx.UpsertThreadNoRevision(domain.SessionIdentity{ThreadID: group.Patch.ThreadID, Source: group.Patch.Source, NativeSessionID: group.Patch.NativeSessionID}, *group.Patch)
				if err != nil {
					return err
				}
				threadChanged = mutation.VisibleChanged
			}
			for _, commit := range group.Sources {
				if commit.AcceptedProof != nil {
					proof := commit.AcceptedProof
					result, err := privateExecResult(tx, "UPDATE codex_source_files SET observed_size=?,observed_mtime_ns=?,file_status='present' WHERE source_file_id=? AND file_generation=? AND device_id=? AND inode=? AND current_path=? AND observed_size=? AND observed_mtime_ns=? AND file_status='present'", proof.ObservedSize, proof.ObservedMTimeNS, proof.SourceFileID, proof.Generation, proof.Identity.DeviceID, proof.Identity.Inode, commit.Observation.CurrentPath, commit.Observation.AcceptedObservedSize, commit.Observation.AcceptedObservedMTimeNS)
					if err != nil {
						return err
					}
					changed, err := result.RowsAffected()
					if err != nil || changed != 1 {
						return fmt.Errorf("metadata accepted proof compare-and-swap failed: %d", proof.SourceFileID)
					}
				}
				if err := privateExec(tx, "UPDATE codex_source_files SET thread_id=? WHERE source_file_id=? AND file_generation=?", commit.SafeFact.OwningThreadID, commit.Observation.SourceFileID, commit.Observation.Generation); err != nil {
					return err
				}
				if err := writeMetadataFact(tx, commit.SafeFact); err != nil {
					return err
				}
			}
			var nextRoot *string
			if err := tx.Private(func(private storage.PrivateTx) error {
				var value sql.NullString
				err := private.QueryRow("SELECT root_session_id FROM threads WHERE thread_id=?", threadID).Scan(&value)
				if err == nil && value.Valid {
					nextRoot = &value.String
				}
				if errors.Is(err, sql.ErrNoRows) {
					return nil
				}
				return err
			}); err != nil {
				return err
			}
			rootChanged := !sameOptionalString(previousRoot, nextRoot)
			if rootChanged || len(bindingChanged) > 0 {
				if deps.ReconcileUsageBinding == nil {
					return fmt.Errorf("metadata usage binding reconciler is required")
				}
				var err error
				callbackVisible, needsShadow, invalidated, retryIDs, err = deps.ReconcileUsageBinding(tx, threadID, previousRoot, nextRoot, bindingChanged, committedAtMS)
				if err != nil {
					return err
				}
			}
			for _, commit := range group.Sources {
				checkpointOffset := commit.PlainCheckpointOffset
				if commit.Observation.Compressed {
					checkpointOffset = commit.Observation.AcceptedObservedSize
					if commit.AcceptedProof != nil {
						checkpointOffset = commit.AcceptedProof.ObservedSize
					}
				}
				status := commit.CheckpointStatus
				if status == "" {
					status = "ready"
				}
				if err := privateExec(tx, "INSERT INTO codex_source_checkpoints(source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,processing_status,last_successful_scan_at_ms,last_error_code) VALUES(?, 'metadata', ?, ?, ?, ?, ?, ?) ON CONFLICT(source_file_id,consumer_kind) DO UPDATE SET parser_version=excluded.parser_version,committed_offset=excluded.committed_offset,guard_hash=excluded.guard_hash,processing_status=excluded.processing_status,last_successful_scan_at_ms=excluded.last_successful_scan_at_ms,last_error_code=excluded.last_error_code", commit.Observation.SourceFileID, commit.SafeFact.MetadataParserVersion, checkpointOffset, commit.GuardHash, status, committedAtMS, commit.CheckpointErrorCode); err != nil {
					return err
				}
			}
			if activeBeforeNeeded {
				activeAfter, err := deps.ProjectActiveCompaction(tx)
				if err != nil {
					return err
				}
				activeVisible = !bytes.Equal(activeBefore, activeAfter)
			}
			if threadChanged || callbackVisible || activeVisible {
				_, err := tx.BumpDataRevision()
				if err != nil {
					return err
				}
			}
			return nil
		})
		if err != nil {
			return outcome, err
		}
		outcome.CommittedThreads++
		outcome.VisibleChanged = outcome.VisibleChanged || threadChanged || callbackVisible || activeVisible
		outcome.NeedsShadowBuild = outcome.NeedsShadowBuild || needsShadow
		outcome.InvalidatedSourceFileIDs = append(outcome.InvalidatedSourceFileIDs, invalidated...)
		outcome.RetryRootIDs = append(outcome.RetryRootIDs, retryIDs...)
	}
	sort.Slice(outcome.InvalidatedSourceFileIDs, func(i, j int) bool { return outcome.InvalidatedSourceFileIDs[i] < outcome.InvalidatedSourceFileIDs[j] })
	outcome.InvalidatedSourceFileIDs = uniqueInt64(outcome.InvalidatedSourceFileIDs)
	sort.Strings(outcome.RetryRootIDs)
	outcome.RetryRootIDs = uniqueStrings(outcome.RetryRootIDs)
	return outcome, nil
}

func writeMetadataFact(tx *source.WriteTx, fact MetadataEvidence) error {
	if fact.SourceFileID <= 0 || fact.FileGeneration <= 0 || fact.MetadataParserVersion < 0 || fact.ResolvedThroughOffset < 0 || fact.OwningThreadID == "" || fact.UpdatedAtMS < 0 {
		return fmt.Errorf("invalid metadata safe fact")
	}
	if fact.ContinuationState == "" {
		fact.ContinuationState = "unstable"
	}
	if fact.QualityStatus == "" {
		fact.QualityStatus = "partial"
	}
	if fact.OwnershipConfidence == "" {
		fact.OwnershipConfidence = "unresolved"
	}
	var cwd, cwdProv, cwdOffset any
	if fact.CWD != nil {
		cwd, cwdProv, cwdOffset = fact.CWD.Value, fact.CWD.Provenance, fact.CWD.Offset
	}
	var parent, parentProv, parentOffset any
	if fact.ParentThreadIDHint != nil {
		parent, parentProv, parentOffset = fact.ParentThreadIDHint.Value, fact.ParentThreadIDHint.Provenance, fact.ParentThreadIDHint.Offset
	}
	var role, roleProv, roleOffset any
	if fact.AgentRoleHint != nil {
		role, roleProv, roleOffset = fact.AgentRoleHint.Value, fact.AgentRoleHint.Provenance, fact.AgentRoleHint.Offset
	}
	var agentPath, agentPathProv, agentPathOffset any
	if fact.AgentPath != nil {
		agentPath, agentPathProv, agentPathOffset = fact.AgentPath.Value, fact.AgentPath.Provenance, fact.AgentPath.Offset
	}
	return privateExec(tx, `INSERT INTO codex_rollout_metadata_facts(source_file_id,file_generation,metadata_parser_version,resolved_through_offset,owning_thread_id,continuation_state,cwd,cwd_provenance,cwd_record_offset,created_at_ms,latest_context_model,latest_context_at_ms,parent_thread_id_hint,parent_hint_provenance,parent_hint_record_offset,agent_role_hint,agent_role_provenance,agent_role_record_offset,agent_path,agent_path_provenance,agent_path_record_offset,replay_start_offset,owning_records_start_offset,ownership_confidence,fact_quality_status,updated_at_ms,latest_context_turn_id,relationship_conflict) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(source_file_id) DO UPDATE SET file_generation=excluded.file_generation,metadata_parser_version=excluded.metadata_parser_version,resolved_through_offset=excluded.resolved_through_offset,owning_thread_id=excluded.owning_thread_id,continuation_state=excluded.continuation_state,cwd=excluded.cwd,cwd_provenance=excluded.cwd_provenance,cwd_record_offset=excluded.cwd_record_offset,created_at_ms=excluded.created_at_ms,latest_context_model=excluded.latest_context_model,latest_context_at_ms=excluded.latest_context_at_ms,parent_thread_id_hint=excluded.parent_thread_id_hint,parent_hint_provenance=excluded.parent_hint_provenance,parent_hint_record_offset=excluded.parent_hint_record_offset,agent_role_hint=excluded.agent_role_hint,agent_role_provenance=excluded.agent_role_provenance,agent_role_record_offset=excluded.agent_role_record_offset,agent_path=excluded.agent_path,agent_path_provenance=excluded.agent_path_provenance,agent_path_record_offset=excluded.agent_path_record_offset,replay_start_offset=excluded.replay_start_offset,owning_records_start_offset=excluded.owning_records_start_offset,ownership_confidence=excluded.ownership_confidence,fact_quality_status=excluded.fact_quality_status,updated_at_ms=excluded.updated_at_ms,latest_context_turn_id=excluded.latest_context_turn_id,relationship_conflict=excluded.relationship_conflict`, fact.SourceFileID, fact.FileGeneration, fact.MetadataParserVersion, fact.ResolvedThroughOffset, fact.OwningThreadID, fact.ContinuationState, cwd, cwdProv, cwdOffset, fact.CreatedAtMS, fact.LatestContextModel, fact.LatestContextAtMS, parent, parentProv, parentOffset, role, roleProv, roleOffset, agentPath, agentPathProv, agentPathOffset, fact.ReplayStartOffset, fact.OwningRecordsStartOffset, fact.OwnershipConfidence, fact.QualityStatus, fact.UpdatedAtMS, fact.LatestContextTurnID, fact.RelationshipConflict)
}

func LoadMetadataEvidence(reader storage.PrivateReader, sourceFileID int64) (MetadataEvidence, bool, error) {
	var fact MetadataEvidence
	var cwd, cwdProvenance, parent, parentProvenance, role, roleProvenance, agentPath, agentPathProvenance sql.NullString
	var contextModel, contextTurnID sql.NullString
	var cwdOffset, parentOffset, roleOffset, agentPathOffset, createdAt, contextAt, contextOffset, replayOffset, owningOffset, relationshipConflict sql.NullInt64
	err := reader.QueryRow(`SELECT source_file_id,file_generation,metadata_parser_version,resolved_through_offset,owning_thread_id,continuation_state,cwd,cwd_provenance,cwd_record_offset,created_at_ms,latest_context_model,latest_context_at_ms,parent_thread_id_hint,parent_hint_provenance,parent_hint_record_offset,agent_role_hint,agent_role_provenance,agent_role_record_offset,agent_path,agent_path_provenance,agent_path_record_offset,replay_start_offset,owning_records_start_offset,ownership_confidence,fact_quality_status,updated_at_ms,latest_context_turn_id,relationship_conflict FROM codex_rollout_metadata_facts WHERE source_file_id=?`, sourceFileID).Scan(
		&fact.SourceFileID, &fact.FileGeneration, &fact.MetadataParserVersion, &fact.ResolvedThroughOffset, &fact.OwningThreadID, &fact.ContinuationState,
		&cwd, &cwdProvenance, &cwdOffset, &createdAt, &contextModel, &contextAt,
		&parent, &parentProvenance, &parentOffset, &role, &roleProvenance, &roleOffset,
		&agentPath, &agentPathProvenance, &agentPathOffset, &replayOffset, &owningOffset,
		&fact.OwnershipConfidence, &fact.QualityStatus, &fact.UpdatedAtMS, &contextTurnID, &relationshipConflict)
	if errors.Is(err, sql.ErrNoRows) {
		return MetadataEvidence{}, false, nil
	}
	if err != nil {
		return MetadataEvidence{}, false, err
	}
	if cwd.Valid {
		fact.CWD = &rollout.MetadataStringCandidate{Value: cwd.String, Provenance: cwdProvenance.String, Offset: cwdOffset.Int64}
	}
	if parent.Valid {
		fact.ParentThreadIDHint = &rollout.MetadataStringCandidate{Value: parent.String, Provenance: parentProvenance.String, Offset: parentOffset.Int64}
	}
	if role.Valid {
		fact.AgentRoleHint = &rollout.MetadataStringCandidate{Value: role.String, Provenance: roleProvenance.String, Offset: roleOffset.Int64}
	}
	if agentPath.Valid {
		fact.AgentPath = &rollout.MetadataStringCandidate{Value: agentPath.String, Provenance: agentPathProvenance.String, Offset: agentPathOffset.Int64}
	}
	fact.CreatedAtMS, fact.LatestContextAtMS, fact.LatestContextRecordOffset = nullableInt64(createdAt), nullableInt64(contextAt), nullableInt64(contextOffset)
	if contextModel.Valid {
		fact.LatestContextModel = &contextModel.String
	}
	if contextTurnID.Valid {
		fact.LatestContextTurnID = &contextTurnID.String
	}
	fact.ReplayStartOffset, fact.OwningRecordsStartOffset = nullableInt64(replayOffset), nullableInt64(owningOffset)
	fact.RelationshipConflict = relationshipConflict.Valid && relationshipConflict.Int64 != 0
	fact.HasConflict = fact.QualityStatus == "conflict"
	return fact, true, nil
}

func nullableInt64(value sql.NullInt64) *int64 {
	if !value.Valid {
		return nil
	}
	result := value.Int64
	return &result
}

func privateExec(tx *source.WriteTx, query string, args ...any) error {
	_, err := privateExecResult(tx, query, args...)
	return err
}

func privateExecResult(tx *source.WriteTx, query string, args ...any) (sql.Result, error) {
	var result sql.Result
	err := tx.Private(func(private storage.PrivateTx) error {
		var err error
		result, err = private.Exec(query, args...)
		return err
	})
	return result, err
}

func validateMetadataAppendProof(observation rollout.SourceObservation, proof rollout.AcceptedSourceProof, generation, device, inode, size, mtime int64, status string) error {
	if proof.SourceFileID != observation.SourceFileID || proof.Generation != generation || proof.Identity != observation.Identity || device != proof.Identity.DeviceID || inode != proof.Identity.Inode {
		return fmt.Errorf("metadata accepted proof mismatch: %d", observation.SourceFileID)
	}
	if proof.FileStatus != rollout.SourceFilePresent || status != string(rollout.SourceFilePresent) ||
		size != observation.AcceptedObservedSize || mtime != observation.AcceptedObservedMTimeNS ||
		proof.ObservedSize != observation.DiscoveryObservedSize || proof.ObservedMTimeNS != observation.DiscoveryObservedMTimeNS ||
		proof.ObservedSize <= size || proof.ObservedSize < 0 || proof.ObservedMTimeNS < 0 {
		return fmt.Errorf("metadata proof is not a frozen append observation: %d", observation.SourceFileID)
	}
	return nil
}

func metadataPatchAffectsActiveProjection(patch *domain.ResolvedThreadPatch) bool {
	if patch == nil {
		return false
	}
	return patch.RootSessionID.Kind() != domain.PatchKeep || patch.AgentRole.Kind() != domain.PatchKeep || patch.ParentThreadID.Kind() != domain.PatchKeep
}

func hasMetadataIdentity(input MetadataResolveInput, id string, facts []MetadataEvidence) bool {
	if _, ok := findStateFact(input.State.Threads, id); ok || len(facts) > 0 {
		return true
	}
	_, ok := input.Session.Get(id)
	if ok {
		return true
	}
	for _, thread := range input.Existing {
		if thread.ThreadID == id {
			return true
		}
	}
	return false
}

func metadataCommitThreadID(group MetadataThreadCommit) string {
	if group.Patch != nil {
		return group.Patch.ThreadID
	}
	if len(group.Sources) > 0 {
		return group.Sources[0].SafeFact.OwningThreadID
	}
	return ""
}
func mergeMetadataCandidate(target **rollout.MetadataStringCandidate, incoming *rollout.MetadataStringCandidate, conflict *bool, priority func(rollout.MetadataStringCandidate) int) {
	if incoming == nil {
		return
	}
	if *target == nil || priority(*incoming) > priority(**target) {
		*target = copyCandidate(incoming)
		return
	}
	if priority(*incoming) == priority(**target) && incoming.Value != (**target).Value {
		*conflict = true
	}
}
func parentCandidatePriority(value rollout.MetadataStringCandidate) int {
	switch value.Provenance {
	case "session_meta_parent":
		return 3
	case "subagent_source":
		return 2
	case "forked_from_id":
		return 1
	}
	return 0
}
func roleCandidatePriority(value rollout.MetadataStringCandidate) int {
	if value.Provenance == "subagent_source" {
		return 2
	}
	return 1
}
func agentPathCandidatePriority(value rollout.MetadataStringCandidate) int {
	if value.Provenance == "session_meta" {
		return 2
	}
	return 1
}
func candidatePriority(provenance string) int {
	if provenance == "session_meta" {
		return 2
	}
	return 1
}
func isLaterMetadataContext(incoming rollout.RecordMetadataEvidence, current MetadataEvidence) bool {
	a, b := contextOrder(incoming.LatestContextTurnID, incoming.LatestContextAtMS), contextOrder(current.LatestContextTurnID, current.LatestContextAtMS)
	if a != b {
		return a > b
	}
	return incoming.LatestContextRecordOffset != nil && (current.LatestContextRecordOffset == nil || *incoming.LatestContextRecordOffset > *current.LatestContextRecordOffset)
}
func contextOrder(turn *string, timestamp *int64) int64 {
	if timestamp != nil {
		return *timestamp
	}
	if turn != nil {
		return 1
	}
	return 0
}
func (e MetadataEvidence) recordEvidence() rollout.RecordMetadataEvidence {
	return rollout.RecordMetadataEvidence{LatestContextModel: e.LatestContextModel, LatestContextTurnID: e.LatestContextTurnID, LatestContextAtMS: e.LatestContextAtMS, LatestContextRecordOffset: e.LatestContextRecordOffset}
}
func copyInt64(value *int64) *int64 {
	if value == nil {
		return nil
	}
	result := *value
	return &result
}
func copyString(value *string) *string {
	if value == nil {
		return nil
	}
	result := *value
	return &result
}
func copyCandidate(value *rollout.MetadataStringCandidate) *rollout.MetadataStringCandidate {
	if value == nil {
		return nil
	}
	result := *value
	return &result
}
func statesHas(states map[string]StateThreadFact, id string) bool   { _, ok := states[id]; return ok }
func hasExisting(existing map[string]domain.Thread, id string) bool { _, ok := existing[id]; return ok }
func findStateFact(facts []StateThreadFact, id string) (StateThreadFact, bool) {
	for _, fact := range facts {
		if fact.ThreadID == id {
			return fact, true
		}
	}
	return StateThreadFact{}, false
}
func oldParent(value domain.Thread, exists bool) *string {
	if exists {
		return value.ParentThreadID
	}
	return nil
}
func oldRoot(value domain.Thread, exists bool) *string {
	if exists {
		return value.RootSessionID
	}
	return nil
}
func oldTitle(value domain.Thread, exists bool) *string {
	if exists {
		return value.Title
	}
	return nil
}
func oldProjectPath(value domain.Thread, exists bool) *string {
	if exists {
		return value.ProjectPath
	}
	return nil
}
func oldProjectName(value domain.Thread, exists bool) *string {
	if exists {
		return value.ProjectName
	}
	return nil
}
func oldModel(value domain.Thread, exists bool) *string {
	if exists {
		return value.MetadataModel
	}
	return nil
}
func oldCreated(value domain.Thread, exists bool) *int64 {
	if exists {
		return value.CreatedAtMS
	}
	return nil
}
func oldUpdated(value domain.Thread, exists bool) *int64 {
	if exists {
		return value.UpdatedAtMS
	}
	return nil
}
func setOrClearString(patch *domain.Patch[string], desired string) {
	// Set confirms the current relationship even when it equals the stored value; Keep preserves history without proving it.
	value := desired
	*patch = domain.Set(value)
}
func setOptionalMetadata(patch *domain.Patch[string], old, desired *string, complete bool) {
	if desired != nil {
		if old == nil || *old != *desired {
			*patch = domain.Set(*desired)
		}
	} else if complete && old != nil {
		*patch = domain.Clear[string]()
	}
}
func setOptionalInt(patch *domain.Patch[int64], old, desired *int64, complete bool) {
	if desired != nil {
		if old == nil || *old != *desired {
			*patch = domain.Set(*desired)
		}
	} else if complete && old != nil {
		*patch = domain.Clear[int64]()
	}
}
func patchHasClear(patch domain.ResolvedThreadPatch) bool {
	return patch.ParentThreadID.Kind() == domain.PatchClear || patch.RootSessionID.Kind() == domain.PatchClear || patch.Title.Kind() == domain.PatchClear || patch.ProjectName.Kind() == domain.PatchClear || patch.ProjectPath.Kind() == domain.PatchClear || patch.MetadataModel.Kind() == domain.PatchClear || patch.CreatedAtMS.Kind() == domain.PatchClear || patch.UpdatedAtMS.Kind() == domain.PatchClear
}
func hasPatchMutation(patch domain.ResolvedThreadPatch) bool {
	return patch.ParentThreadID.Kind() != domain.PatchKeep || patch.RootSessionID.Kind() != domain.PatchKeep || patch.AgentRole.Kind() != domain.PatchKeep || patch.Title.Kind() != domain.PatchKeep || patch.ProjectName.Kind() != domain.PatchKeep || patch.ProjectPath.Kind() != domain.PatchKeep || patch.ProjectKind.Kind() != domain.PatchKeep || patch.MetadataModel.Kind() != domain.PatchKeep || patch.CreatedAtMS.Kind() != domain.PatchKeep || patch.UpdatedAtMS.Kind() != domain.PatchKeep || patch.Archived.Kind() != domain.PatchKeep
}
func nullableString(value sql.NullString) *string {
	if !value.Valid {
		return nil
	}
	result := value.String
	return &result
}
func sameOptionalString(a, b *string) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return *a == *b
}
func uniqueInt64(values []int64) []int64 {
	if len(values) == 0 {
		return values
	}
	out := values[:1]
	for _, v := range values[1:] {
		if out[len(out)-1] != v {
			out = append(out, v)
		}
	}
	return out
}
func uniqueStrings(values []string) []string {
	if len(values) == 0 {
		return values
	}
	out := values[:1]
	for _, v := range values[1:] {
		if out[len(out)-1] != v {
			out = append(out, v)
		}
	}
	return out
}
