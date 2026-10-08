package domain

import "testing"

func TestThreadPatchValueValidation(t *testing.T) {
	base, err := NewResolvedThreadPatch(validSessionIdentity(t), 10)
	if err != nil {
		t.Fatal(err)
	}

	set := Set("value")
	if value, ok := set.Value(); !ok || value != "value" || set.Kind() != PatchSet {
		t.Fatalf("Set patch = (%q, %t), kind %d", value, ok, set.Kind())
	}
	for _, patch := range []Patch[string]{Keep[string](), Clear[string]()} {
		if value, ok := patch.Value(); ok || value != "" {
			t.Errorf("non-Set patch returned (%q, %t)", value, ok)
		}
	}

	invalid := []struct {
		name   string
		change func(*ResolvedThreadPatch)
	}{
		{"blank parent", func(p *ResolvedThreadPatch) { p.ParentThreadID = Set(" \u2003") }},
		{"control title", func(p *ResolvedThreadPatch) { p.Title = Set("title\x7f") }},
		{"relative project path", func(p *ResolvedThreadPatch) { p.ProjectPath = Set("project/path") }},
		{"negative resolved time", func(p *ResolvedThreadPatch) { p.ResolvedAtMS = -1 }},
		{"negative created time", func(p *ResolvedThreadPatch) { p.CreatedAtMS = Set(int64(-1)) }},
		{"negative updated time", func(p *ResolvedThreadPatch) { p.UpdatedAtMS = Set(int64(-1)) }},
		{"unknown agent role", func(p *ResolvedThreadPatch) { p.AgentRole = Set(AgentRole("worker")) }},
		{"unknown project kind", func(p *ResolvedThreadPatch) { p.ProjectKind = Set(ProjectKind("workspace")) }},
		{"unknown quality", func(p *ResolvedThreadPatch) { p.MetadataQualityStatus = MetadataQualityStatus("unknown") }},
		{"invalid patch state", func(p *ResolvedThreadPatch) { p.Title = Patch[string]{kind: PatchKind(9)} }},
		{"clear without full resolution", func(p *ResolvedThreadPatch) { p.Title = Clear[string]() }},
		{"clear role", func(p *ResolvedThreadPatch) { p.FullResolution = true; p.AgentRole = Clear[AgentRole]() }},
		{"clear project kind", func(p *ResolvedThreadPatch) { p.FullResolution = true; p.ProjectKind = Clear[ProjectKind]() }},
		{"clear archived", func(p *ResolvedThreadPatch) { p.FullResolution = true; p.Archived = Clear[bool]() }},
		{"unknown role with root", func(p *ResolvedThreadPatch) { p.AgentRole = Set(AgentRole("unknown")); p.RootSessionID = Set("root") }},
		{"main role with parent", func(p *ResolvedThreadPatch) { p.AgentRole = Set(AgentRole("main")); p.ParentThreadID = Set("parent") }},
	}
	for _, test := range invalid {
		t.Run(test.name, func(t *testing.T) {
			patch := base
			test.change(&patch)
			if err := patch.Validate(); err == nil {
				t.Fatal("Validate succeeded")
			}
		})
	}

	patch := base
	patch.FullResolution = true
	patch.Title = Clear[string]()
	if err := patch.Validate(); err != nil {
		t.Fatalf("full-resolution Clear rejected: %v", err)
	}
}

func TestNewResolvedThreadPatchDefaults(t *testing.T) {
	identity := validSessionIdentity(t)
	patch, err := NewResolvedThreadPatch(identity, 42)
	if err != nil {
		t.Fatal(err)
	}
	if patch.ThreadID != identity.ThreadID || patch.Source != identity.Source || patch.NativeSessionID != identity.NativeSessionID {
		t.Fatalf("identity was not preserved: %+v", patch)
	}
	if patch.ResolvedAtMS != 42 || patch.MetadataQualityStatus != "complete" || patch.FullResolution {
		t.Fatalf("unexpected defaults: %+v", patch)
	}
	for name, kind := range map[string]PatchKind{
		"parent_thread_id": patch.ParentThreadID.Kind(),
		"root_session_id":  patch.RootSessionID.Kind(),
		"agent_role":       patch.AgentRole.Kind(),
		"title":            patch.Title.Kind(),
		"project_name":     patch.ProjectName.Kind(),
		"project_path":     patch.ProjectPath.Kind(),
		"project_kind":     patch.ProjectKind.Kind(),
		"metadata_model":   patch.MetadataModel.Kind(),
		"created_at_ms":    patch.CreatedAtMS.Kind(),
		"updated_at_ms":    patch.UpdatedAtMS.Kind(),
		"archived":         patch.Archived.Kind(),
	} {
		if kind != PatchKeep {
			t.Errorf("%s kind = %d, want Keep", name, kind)
		}
	}
}

func validSessionIdentity(t *testing.T) SessionIdentity {
	t.Helper()
	identity, err := NewSessionIdentity("codex:thread-1", SourceCodex, "thread-1")
	if err != nil {
		t.Fatal(err)
	}
	return identity
}
