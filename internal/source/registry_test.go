package source

import (
	"context"
	"errors"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/domain"
)

type testAdapter struct {
	descriptor      Descriptor
	availability    Availability
	availabilityErr error
	runErr          error
	descriptorCalls int
}

func (a *testAdapter) Descriptor() Descriptor {
	a.descriptorCalls++
	return a.descriptor
}

func (a *testAdapter) Availability(context.Context) (Availability, error) {
	return a.availability, a.availabilityErr
}

func (a *testAdapter) RunScan(context.Context, RunContext) error {
	return a.runErr
}

func mustDescriptor(t *testing.T, source domain.SourceID, name string) Descriptor {
	t.Helper()
	descriptor, err := NewDescriptor(source, name)
	if err != nil {
		t.Fatal(err)
	}
	return descriptor
}

func TestRegistryRejectsDuplicateSource(t *testing.T) {
	first := &testAdapter{descriptor: mustDescriptor(t, domain.SourceCodex, "Codex")}
	second := &testAdapter{descriptor: mustDescriptor(t, domain.SourceCodex, "Another Codex")}
	if _, err := NewRegistry(first, second); err == nil {
		t.Fatal("NewRegistry accepted a duplicate source ID")
	}
	if first.descriptorCalls != 1 || second.descriptorCalls != 1 {
		t.Fatalf("descriptor calls = %d/%d; want one call per adapter", first.descriptorCalls, second.descriptorCalls)
	}
}

func TestRegistrySourceIDsSortedAndImmutable(t *testing.T) {
	zeta := &testAdapter{descriptor: mustDescriptor(t, domain.SourceID("zeta"), "Zeta")}
	codex := &testAdapter{descriptor: mustDescriptor(t, domain.SourceCodex, "Codex")}
	registry, err := NewRegistry(zeta, codex)
	if err != nil {
		t.Fatal(err)
	}
	ids := registry.SourceIDs()
	if len(ids) != 2 || ids[0] != domain.SourceCodex || ids[1] != domain.SourceID("zeta") {
		t.Fatalf("SourceIDs() = %v", ids)
	}
	ids[0] = domain.SourceID("mutated")
	if got := registry.SourceIDs(); got[0] != domain.SourceCodex {
		t.Fatalf("mutating SourceIDs() changed Registry: %v", got)
	}
	if _, ok := registry.Get(domain.SourceID("mutated")); ok {
		t.Fatal("Registry lookup observed a mutated ID slice")
	}
}

func TestRegistryFrozenDescriptorIdentity(t *testing.T) {
	initial := mustDescriptor(t, domain.SourceID("frozen"), "Initial")
	later := mustDescriptor(t, domain.SourceID("changed"), "Changed")
	adapter := &changingDescriptorAdapter{first: initial, later: later}
	registry, err := NewRegistry(adapter)
	if err != nil {
		t.Fatal(err)
	}
	ids := registry.SourceIDs()
	if len(ids) != 1 || ids[0] != initial.ID {
		t.Fatalf("SourceIDs() = %v", ids)
	}
	gotDescriptor, ok := registry.Descriptor(initial.ID)
	if !ok || gotDescriptor != initial {
		t.Fatalf("Descriptor(%q) = %+v, %t", initial.ID, gotDescriptor, ok)
	}
	gotAdapter, ok := registry.Get(initial.ID)
	if !ok || gotAdapter != adapter {
		t.Fatalf("Get(%q) = %T, %t", initial.ID, gotAdapter, ok)
	}
	if _, ok := registry.Get(later.ID); ok {
		t.Fatal("Registry adopted the adapter's later Descriptor")
	}
	if adapter.calls != 1 {
		t.Fatalf("Descriptor calls = %d; want only the registration read", adapter.calls)
	}
	db := openSourceTestDB(t)
	run, err := NewStorageFactory(db).Context(context.Background(), "scan-frozen", gotDescriptor)
	if err != nil {
		t.Fatal(err)
	}
	if run.Source() != initial.ID || run.Storage().source != initial.ID || run.ScanID() != "scan-frozen" {
		t.Fatalf("RunContext did not use frozen identity: source=%q scan=%q", run.Source(), run.ScanID())
	}
}

type changingDescriptorAdapter struct {
	first Descriptor
	later Descriptor
	calls int
}

func (a *changingDescriptorAdapter) Descriptor() Descriptor {
	a.calls++
	if a.calls == 1 {
		return a.first
	}
	return a.later
}

func (*changingDescriptorAdapter) Availability(context.Context) (Availability, error) {
	return Available(), nil
}

func (*changingDescriptorAdapter) RunScan(context.Context, RunContext) error {
	return nil
}

var typedNilDescriptorCalls int

type typedNilAdapter struct{}

func (*typedNilAdapter) Descriptor() Descriptor {
	typedNilDescriptorCalls++
	return Descriptor{}
}

func (*typedNilAdapter) Availability(context.Context) (Availability, error) {
	return Availability{}, nil
}

func (*typedNilAdapter) RunScan(context.Context, RunContext) error {
	return nil
}

func TestRegistryRejectsTypedNilAdapter(t *testing.T) {
	var ordinaryNil Adapter
	if _, err := NewRegistry(ordinaryNil); err == nil {
		t.Fatal("NewRegistry accepted a nil Adapter")
	}
	typedNilDescriptorCalls = 0
	var pointer *typedNilAdapter
	var adapter Adapter = pointer
	if _, err := NewRegistry(adapter); err == nil {
		t.Fatal("NewRegistry accepted a typed-nil Adapter")
	}
	if typedNilDescriptorCalls != 0 {
		t.Fatalf("typed-nil Descriptor calls = %d; want zero", typedNilDescriptorCalls)
	}
}

func TestAvailabilityZeroValueIsInvalid(t *testing.T) {
	if got := (Availability{}).Kind(); got != AvailabilityInvalid {
		t.Fatalf("zero Availability kind = %d; want Invalid", got)
	}
	if got := (Availability{}).Reason(); got != "" {
		t.Fatalf("zero Availability reason = %q", got)
	}
	if got := Unavailable("").Kind(); got != AvailabilityUnavailable || Unavailable("").Reason() != "" {
		t.Fatalf("Unavailable empty reason was not preserved: %+v", Unavailable(""))
	}
	if Available().Kind() != AvailabilityAvailable || NotInstalled().Kind() != AvailabilityNotInstalled {
		t.Fatal("Availability constructors returned an unexpected kind")
	}
}

func TestAvailabilityUnknownKindPreserved(t *testing.T) {
	unknown := Availability{kind: AvailabilityKind(255), reason: "ignored"}
	if got := unknown.Kind(); got != AvailabilityKind(255) {
		t.Fatalf("unknown Availability kind = %d", got)
	}
	if got := unknown.Reason(); got != "" {
		t.Fatalf("unknown Availability reason = %q; want empty", got)
	}
}

func TestAdapterErrorCodeValidationTiming(t *testing.T) {
	plain := errors.New("plain adapter failure")
	if got := ErrorCode(plain); got != "SOURCE_RUN_FAILED" {
		t.Fatalf("ErrorCode(plain) = %q", got)
	}
	if err := domain.ValidateErrorCode(ErrorCode(plain)); err != nil {
		t.Fatalf("default Adapter code is invalid: %v", err)
	}
	typed := NewAdapterErrorWithCode("invalid-code", "typed failure", plain)
	if got := ErrorCode(typed); got != "invalid-code" {
		t.Fatalf("ErrorCode(typed) = %q; invalid code must remain unchanged", got)
	}
	if err := domain.ValidateErrorCode(ErrorCode(typed)); err == nil {
		t.Fatal("Lifecycle Error Code validation accepted the invalid Typed code")
	}
	if got := typed.Error(); got != "typed failure" {
		t.Fatalf("Error() = %q", got)
	}
	if !errors.Is(typed, plain) {
		t.Fatal("AdapterError did not unwrap its cause")
	}
	fallback := NewAdapterErrorWithCode("CUSTOM", "", plain)
	if got := fallback.Error(); got != plain.Error() {
		t.Fatalf("empty-message Error() = %q", got)
	}
}

func TestDescriptorValidationAndDisplayOrder(t *testing.T) {
	for _, descriptor := range []Descriptor{
		{ID: domain.SourceID("Bad"), DisplayName: "Name"},
		{ID: domain.SourceCodex, DisplayName: " \u2003"},
		{ID: domain.SourceCodex, DisplayName: "Bad\x7fName"},
	} {
		if err := descriptor.Validate(); err == nil {
			t.Fatalf("Validate accepted %+v", descriptor)
		}
	}
	if got := (Descriptor{ID: domain.SourceCodex}).DisplayOrder(); got != 10 {
		t.Fatalf("Codex DisplayOrder() = %d", got)
	}
	if got := (Descriptor{ID: domain.SourceAntigravity}).DisplayOrder(); got != 20 {
		t.Fatalf("Antigravity DisplayOrder() = %d", got)
	}
	if got := (Descriptor{ID: domain.SourceID("other")}).DisplayOrder(); got != 65535 {
		t.Fatalf("other DisplayOrder() = %d", got)
	}
}
