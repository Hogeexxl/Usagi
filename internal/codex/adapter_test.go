package codex

import (
	"context"
	"errors"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
)

func TestConfigAdapterDescriptorAndAvailabilityResolveEachTime(t *testing.T) {
	resolver := &fakeConfigResolver{}
	adapter, err := NewAdapterWithResolver(resolver)
	if err != nil {
		t.Fatal(err)
	}
	if got := adapter.Descriptor(); got.ID != domain.SourceCodex || got.DisplayName != "Codex" {
		t.Fatalf("Descriptor() = %+v", got)
	}
	for range 2 {
		availability, err := adapter.Availability(context.Background())
		if err != nil || availability.Kind() != source.AvailabilityAvailable {
			t.Fatalf("Availability() = %v, %v", availability.Kind(), err)
		}
	}
	if resolver.calls != 2 {
		t.Fatalf("resolver calls = %d, want 2", resolver.calls)
	}
}

func TestConfigAdapterInvalidConfigIsTypedFailure(t *testing.T) {
	adapter, err := NewAdapterWithResolver(&fakeConfigResolver{err: &ConfigError{Code: "CODEX_METADATA_HOME_MISMATCH"}})
	if err != nil {
		t.Fatal(err)
	}
	availability, err := adapter.Availability(context.Background())
	if availability.Kind() != source.AvailabilityInvalid || source.ErrorCode(err) != "CODEX_METADATA_HOME_MISMATCH" {
		t.Fatalf("Availability() = %v, %v", availability.Kind(), err)
	}
}

func TestConfigAdapterResolverErrorUsesSourceFailureCode(t *testing.T) {
	adapter, err := NewAdapterWithResolver(&fakeConfigResolver{err: errors.New("resolver failure")})
	if err != nil {
		t.Fatal(err)
	}
	_, err = adapter.Availability(context.Background())
	if source.ErrorCode(err) != "CODEX_HOME_RESOLUTION_FAILED" {
		t.Fatalf("ErrorCode() = %q", source.ErrorCode(err))
	}
}

type fakeConfigResolver struct {
	config Config
	err    error
	calls  int
}

func (resolver *fakeConfigResolver) Resolve() ConfigResolution {
	resolver.calls++
	return ConfigResolution{Config: resolver.config, Err: resolver.err}
}
