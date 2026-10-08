package domain

import (
	"strings"
	"testing"
)

func TestThreadIdentityValidation(t *testing.T) {
	for _, source := range []SourceID{SourceCodex, SourceAntigravity} {
		identity, err := NewSessionIdentity("codex:session-1", source, "session-1")
		if err != nil {
			t.Fatalf("NewSessionIdentity(%q): %v", source, err)
		}
		if err := identity.Validate(); err != nil {
			t.Fatalf("SessionIdentity.Validate(%q): %v", source, err)
		}
	}

	for _, value := range []string{"", "Codex", "1source", "source:", "source name", "é", " codex"} {
		if _, err := NewSourceID(value); err == nil {
			t.Errorf("NewSourceID(%q) succeeded", value)
		}
	}
	if _, err := NewSourceID(strings.Repeat("a", 65)); err == nil {
		t.Error("NewSourceID accepted more than 64 bytes")
	}
	if _, err := NewSourceID(strings.Repeat("a", 64)); err != nil {
		t.Errorf("NewSourceID rejected 64 bytes: %v", err)
	}
	if _, err := NewSourceID("a9_-z"); err != nil {
		t.Errorf("NewSourceID rejected valid slug: %v", err)
	}

	for _, identity := range []SessionIdentity{
		{ThreadID: "", Source: SourceCodex, NativeSessionID: "native"},
		{ThreadID: "\u2003", Source: SourceCodex, NativeSessionID: "native"},
		{ThreadID: "thread\x00id", Source: SourceCodex, NativeSessionID: "native"},
		{ThreadID: "thread", Source: SourceID("Bad"), NativeSessionID: "native"},
		{ThreadID: "thread", Source: SourceCodex, NativeSessionID: " \t"},
		{ThreadID: "thread", Source: SourceCodex, NativeSessionID: "native\u0085id"},
	} {
		if err := identity.Validate(); err == nil {
			t.Errorf("SessionIdentity.Validate accepted %+v", identity)
		}
	}
}
