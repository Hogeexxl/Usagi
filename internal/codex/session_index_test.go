package codex

import (
	"strings"
	"testing"
)

func TestSessionIndexThreadIDAllowlistHalfLineAndDuplicateChoice(t *testing.T) {
	input := strings.Join([]string{
		`{"thread_id":"thread-a","thread_name":"fallback","updated_at":100}`,
		`{"id":"thread-a","thread_id":"ignored","thread_name":"newer","updated_at":200}`,
		`{"thread_id":"thread-b","thread_name":"retained","updated_at":100}`,
		`{"thread_id":"thread-c","thread_name":"half"}`,
	}, "\n")
	snapshot := readSessionSnapshot(strings.NewReader(input))
	if snapshot.Status != SessionSourcePartial {
		t.Fatalf("expected trailing half line, got %v", snapshot.Status)
	}
	if got := snapshot.Names["thread-a"].ThreadName; got != "newer" {
		t.Fatalf("id must win over thread_id and newer timestamp: %q", got)
	}
	if got := snapshot.Names["thread-b"].ThreadName; got != "retained" {
		t.Fatalf("thread_id fallback lost: %q", got)
	}
	if _, ok := snapshot.Names["thread-c"]; ok {
		t.Fatal("half line must not contribute a fact")
	}
}
