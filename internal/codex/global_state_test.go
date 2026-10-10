package codex

import (
	"errors"
	"path/filepath"
	"strings"
	"testing"
)

type failingGlobalReader struct{ sent bool }

func (r *failingGlobalReader) Read(p []byte) (int, error) {
	if r.sent {
		return 0, errors.New("injected read failure")
	}
	r.sent = true
	return copy(p, `{"projectless-thread-ids":["thread-a"],`), nil
}

func TestGlobalStateFourStatusesAndProjectionAllowlist(t *testing.T) {
	absent := ReadGlobalState(filepath.Join(t.TempDir(), "missing.json"))
	if absent.Status != GlobalStateNotPresent {
		t.Fatalf("missing state status: %v", absent.Status)
	}
	malformed := readGlobalState(strings.NewReader(`{"projectless-thread-ids":[`))
	if malformed.Status != GlobalStateMalformed {
		t.Fatalf("syntax/EOF status: %v", malformed.Status)
	}
	complete := readGlobalState(strings.NewReader(`{"projectless-thread-ids":["thread-a","thread-a"],"thread-project-assignments":{"thread-b":{"discard":{"private":"value"}}}}`))
	if complete.Status != GlobalStateComplete || len(complete.ProjectlessThreadIDs) != 1 || !complete.HasAssignment("thread-b") {
		t.Fatalf("complete allowlist projection: %#v", complete)
	}
	if !hasDiagnosticCode(complete.Diagnostics, "duplicate_thread_id") {
		t.Fatalf("duplicate fact diagnostic missing: %#v", complete.Diagnostics)
	}
	if unreadable := readGlobalState(&failingGlobalReader{}); unreadable.Status != GlobalStateUnreadable {
		t.Fatalf("reader failure status: %v", unreadable.Status)
	}
	directory := t.TempDir()
	if unreadable := ReadGlobalState(directory); unreadable.Status != GlobalStateUnreadable {
		t.Fatalf("directory read must be unreadable: %v", unreadable.Status)
	}
}

func TestGlobalStateInvalidIDAndMissingFieldAreMalformed(t *testing.T) {
	for _, input := range []string{`{"projectless-thread-ids":["bad\u0000id"],"thread-project-assignments":{}}`, `{"thread-project-assignments":{}}`} {
		if got := readGlobalState(strings.NewReader(input)).Status; got != GlobalStateMalformed {
			t.Fatalf("input %s: got status %v", input, got)
		}
	}
}

func hasDiagnosticCode(diagnostics []GlobalStateDiagnostic, code string) bool {
	for _, diagnostic := range diagnostics {
		if diagnostic.Code == code {
			return true
		}
	}
	return false
}
