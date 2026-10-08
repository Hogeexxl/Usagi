package domain

import (
	"fmt"
	"path/filepath"
	"strings"
	"unicode"
)

type SessionIdentity struct {
	ThreadID        string
	Source          SourceID
	NativeSessionID string
}

func NewSessionIdentity(threadID string, source SourceID, nativeSessionID string) (SessionIdentity, error) {
	identity := SessionIdentity{
		ThreadID:        threadID,
		Source:          source,
		NativeSessionID: nativeSessionID,
	}
	if err := identity.Validate(); err != nil {
		return SessionIdentity{}, err
	}
	return identity, nil
}

func (identity SessionIdentity) Validate() error {
	if err := validateIdentityText(identity.ThreadID, "thread_id"); err != nil {
		return err
	}
	if err := identity.Source.Validate(); err != nil {
		return fmt.Errorf("source: %w", err)
	}
	return validateIdentityText(identity.NativeSessionID, "native_session_id")
}

func validateIdentityText(value string, field string) error {
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

type PatchKind uint8

const (
	PatchKeep PatchKind = iota
	PatchSet
	PatchClear
)

type Patch[T any] struct {
	kind  PatchKind
	value T
}

func Keep[T any]() Patch[T] {
	return Patch[T]{kind: PatchKeep}
}

func Set[T any](value T) Patch[T] {
	return Patch[T]{kind: PatchSet, value: value}
}

func Clear[T any]() Patch[T] {
	return Patch[T]{kind: PatchClear}
}

func (patch Patch[T]) Kind() PatchKind {
	return patch.kind
}

func (patch Patch[T]) Value() (T, bool) {
	if patch.kind == PatchSet {
		return patch.value, true
	}
	var zero T
	return zero, false
}

type AgentRole string

func (role AgentRole) Validate() error {
	switch role {
	case "main", "subagent", "unknown":
		return nil
	default:
		return fmt.Errorf("invalid agent_role: unknown value %q", role)
	}
}

type ProjectKind string

func (kind ProjectKind) Validate() error {
	switch kind {
	case "project", "projectless", "unknown":
		return nil
	default:
		return fmt.Errorf("invalid project_kind: unknown value %q", kind)
	}
}

type MetadataQualityStatus string

func (status MetadataQualityStatus) Validate() error {
	switch status {
	case "complete", "partial", "conflict":
		return nil
	default:
		return fmt.Errorf("invalid metadata_quality_status: unknown value %q", status)
	}
}

type ResolvedThreadPatch struct {
	ThreadID              string
	Source                SourceID
	NativeSessionID       string
	ParentThreadID        Patch[string]
	RootSessionID         Patch[string]
	AgentRole             Patch[AgentRole]
	Title                 Patch[string]
	ProjectName           Patch[string]
	ProjectPath           Patch[string]
	ProjectKind           Patch[ProjectKind]
	MetadataModel         Patch[string]
	CreatedAtMS           Patch[int64]
	UpdatedAtMS           Patch[int64]
	Archived              Patch[bool]
	MetadataQualityStatus MetadataQualityStatus
	ResolvedAtMS          int64
	FullResolution        bool
}

func NewResolvedThreadPatch(identity SessionIdentity, resolvedAtMS int64) (ResolvedThreadPatch, error) {
	if err := identity.Validate(); err != nil {
		return ResolvedThreadPatch{}, err
	}
	if resolvedAtMS < 0 {
		return ResolvedThreadPatch{}, fmt.Errorf("invalid resolved_at_ms: must be non-negative")
	}
	return ResolvedThreadPatch{
		ThreadID:              identity.ThreadID,
		Source:                identity.Source,
		NativeSessionID:       identity.NativeSessionID,
		ParentThreadID:        Keep[string](),
		RootSessionID:         Keep[string](),
		AgentRole:             Keep[AgentRole](),
		Title:                 Keep[string](),
		ProjectName:           Keep[string](),
		ProjectPath:           Keep[string](),
		ProjectKind:           Keep[ProjectKind](),
		MetadataModel:         Keep[string](),
		CreatedAtMS:           Keep[int64](),
		UpdatedAtMS:           Keep[int64](),
		Archived:              Keep[bool](),
		MetadataQualityStatus: "complete",
		ResolvedAtMS:          resolvedAtMS,
		FullResolution:        false,
	}, nil
}

func (patch ResolvedThreadPatch) Validate() error {
	identity := SessionIdentity{
		ThreadID:        patch.ThreadID,
		Source:          patch.Source,
		NativeSessionID: patch.NativeSessionID,
	}
	if err := identity.Validate(); err != nil {
		return err
	}
	if patch.ResolvedAtMS < 0 {
		return fmt.Errorf("invalid resolved_at_ms: must be non-negative")
	}
	if err := validatePatchTime(patch.CreatedAtMS, "created_at_ms"); err != nil {
		return err
	}
	if err := validatePatchTime(patch.UpdatedAtMS, "updated_at_ms"); err != nil {
		return err
	}
	for _, state := range []struct {
		kind  PatchKind
		field string
	}{
		{patch.ParentThreadID.Kind(), "parent_thread_id"},
		{patch.RootSessionID.Kind(), "root_session_id"},
		{patch.AgentRole.Kind(), "agent_role"},
		{patch.Title.Kind(), "title"},
		{patch.ProjectName.Kind(), "project_name"},
		{patch.ProjectPath.Kind(), "project_path"},
		{patch.ProjectKind.Kind(), "project_kind"},
		{patch.MetadataModel.Kind(), "metadata_model"},
		{patch.CreatedAtMS.Kind(), "created_at_ms"},
		{patch.UpdatedAtMS.Kind(), "updated_at_ms"},
		{patch.Archived.Kind(), "archived"},
	} {
		if state.kind > PatchClear {
			return fmt.Errorf("invalid patch state for %s", state.field)
		}
	}
	for _, value := range []struct {
		patch Patch[string]
		field string
	}{
		{patch.ParentThreadID, "parent_thread_id"},
		{patch.RootSessionID, "root_session_id"},
		{patch.Title, "title"},
		{patch.ProjectName, "project_name"},
		{patch.ProjectPath, "project_path"},
		{patch.MetadataModel, "metadata_model"},
	} {
		if err := validatePatchString(value.patch, value.field); err != nil {
			return err
		}
	}
	if value, ok := patch.ProjectPath.Value(); ok && !filepath.IsAbs(value) {
		return fmt.Errorf("invalid project_path: must be an absolute path")
	}
	if value, ok := patch.AgentRole.Value(); ok {
		if err := value.Validate(); err != nil {
			return err
		}
	}
	if value, ok := patch.ProjectKind.Value(); ok {
		if err := value.Validate(); err != nil {
			return err
		}
	}
	if err := patch.MetadataQualityStatus.Validate(); err != nil {
		return err
	}
	if patch.AgentRole.Kind() == PatchClear || patch.ProjectKind.Kind() == PatchClear || patch.Archived.Kind() == PatchClear {
		return fmt.Errorf("agent_role, project_kind, and archived patches cannot be cleared")
	}
	if !patch.FullResolution && patch.hasClear() {
		return fmt.Errorf("clear requires full-resolution metadata recomputation")
	}
	if role, ok := patch.AgentRole.Value(); ok && role == "unknown" && patch.RootSessionID.Kind() == PatchSet {
		return fmt.Errorf("unknown agent role cannot set root_session_id")
	}
	if role, ok := patch.AgentRole.Value(); ok && role == "main" && patch.ParentThreadID.Kind() == PatchSet {
		return fmt.Errorf("main agent role cannot set a parent thread")
	}
	return nil
}

func (patch ResolvedThreadPatch) hasClear() bool {
	return patch.ParentThreadID.Kind() == PatchClear ||
		patch.RootSessionID.Kind() == PatchClear ||
		patch.AgentRole.Kind() == PatchClear ||
		patch.Title.Kind() == PatchClear ||
		patch.ProjectName.Kind() == PatchClear ||
		patch.ProjectPath.Kind() == PatchClear ||
		patch.ProjectKind.Kind() == PatchClear ||
		patch.MetadataModel.Kind() == PatchClear ||
		patch.CreatedAtMS.Kind() == PatchClear ||
		patch.UpdatedAtMS.Kind() == PatchClear ||
		patch.Archived.Kind() == PatchClear
}

func validatePatchString(patch Patch[string], field string) error {
	if value, ok := patch.Value(); ok {
		return validateIdentityText(value, field)
	}
	return nil
}

func validatePatchTime(patch Patch[int64], field string) error {
	if value, ok := patch.Value(); ok && value < 0 {
		return fmt.Errorf("invalid %s: must be non-negative", field)
	}
	return nil
}

type Thread struct {
	ThreadID              string
	Source                SourceID
	NativeSessionID       string
	ParentThreadID        *string
	RootSessionID         *string
	AgentRole             AgentRole
	Title                 *string
	ProjectName           *string
	ProjectPath           *string
	ProjectKind           ProjectKind
	MetadataModel         *string
	CreatedAtMS           *int64
	UpdatedAtMS           *int64
	Archived              bool
	MetadataQualityStatus MetadataQualityStatus
	MetadataResolvedAtMS  int64
}
