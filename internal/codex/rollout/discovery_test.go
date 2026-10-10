package rollout

import (
	"os"
	"path/filepath"
	"testing"
)

func TestDiscoverFindsNestedPlainAndZstdSegments(t *testing.T) {
	home := t.TempDir()
	sessions := filepath.Join(home, "sessions", "2026", "10", "10")
	archived := filepath.Join(home, "archived_sessions", "2025", "12")
	if err := os.MkdirAll(sessions, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(archived, 0o755); err != nil {
		t.Fatal(err)
	}
	plain := filepath.Join(sessions, "rollout-2026-10-10T00-00-00-12345678-1234-4234-8234-123456789abc_2.jsonl")
	zstd := filepath.Join(archived, "rollout-long-thread-name.jsonl.zst")
	writeFile(t, plain, []byte("{}\n"))
	writeFile(t, zstd, []byte("not decoded during discovery"))

	snapshot := Discover(home, 42)
	if snapshot.StartedAtMS != 42 || snapshot.Sessions != RegionComplete || snapshot.Archived != RegionComplete {
		t.Fatalf("snapshot state = %+v", snapshot)
	}
	if len(snapshot.Files) != 2 {
		t.Fatalf("files = %+v", snapshot.Files)
	}
	if snapshot.Files[0].Area != AreaSessions || snapshot.Files[0].ThreadIDCandidate != "12345678-1234-4234-8234-123456789abc" {
		t.Fatalf("plain file = %+v", snapshot.Files[0])
	}
	if !snapshot.Files[1].Compressed || snapshot.Files[1].ThreadIDCandidate != "" {
		t.Fatalf("zstd file = %+v", snapshot.Files[1])
	}
}

func TestDiscoverDeduplicatesPhysicalAliasWithSessionsPriority(t *testing.T) {
	home := t.TempDir()
	sessions := filepath.Join(home, "sessions")
	archived := filepath.Join(home, "archived_sessions")
	if err := os.MkdirAll(sessions, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(archived, 0o755); err != nil {
		t.Fatal(err)
	}
	sessionPath := filepath.Join(sessions, "rollout-same.jsonl")
	archivedPath := filepath.Join(archived, "rollout-alias.jsonl")
	writeFile(t, sessionPath, []byte("{}\n"))
	if err := os.Link(sessionPath, archivedPath); err != nil {
		t.Skipf("hardlinks unavailable: %v", err)
	}

	snapshot := Discover(home, 0)
	if len(snapshot.Files) != 1 || snapshot.Files[0].Path != sessionPath {
		t.Fatalf("files = %+v, want sessions alias only", snapshot.Files)
	}
}

func TestDiscoverMissingAndInvalidRoots(t *testing.T) {
	home := t.TempDir()
	snapshot := Discover(home, -1)
	if snapshot.StartedAtMS != 0 || snapshot.Sessions != RegionComplete || snapshot.Archived != RegionComplete {
		t.Fatalf("missing roots = %+v", snapshot)
	}
	if err := os.WriteFile(filepath.Join(home, "sessions"), []byte("file"), 0o600); err != nil {
		t.Fatal(err)
	}
	snapshot = Discover(home, 1)
	if snapshot.Sessions != RegionUnavailable || snapshot.Archived != RegionComplete {
		t.Fatalf("invalid root state = sessions %d, archived %d", snapshot.Sessions, snapshot.Archived)
	}
}

func TestDiscoverRejectsSymlinkRootAndFile(t *testing.T) {
	home := t.TempDir()
	target := t.TempDir()
	if err := os.Symlink(target, filepath.Join(home, "sessions")); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}
	if err := os.MkdirAll(filepath.Join(home, "archived_sessions"), 0o755); err != nil {
		t.Fatal(err)
	}
	writeFile(t, filepath.Join(target, "rollout-outside.jsonl"), []byte("{}\n"))
	if err := os.Symlink(filepath.Join(target, "rollout-outside.jsonl"), filepath.Join(home, "archived_sessions", "rollout-link.jsonl")); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}
	snapshot := Discover(home, 1)
	if snapshot.Sessions != RegionUnavailable || len(snapshot.Files) != 0 {
		t.Fatalf("symlink root was followed: %+v", snapshot)
	}
}

func TestDiscoveryIdentitySurvivesRenameAndChangesOnReplacement(t *testing.T) {
	home := t.TempDir()
	sessions := filepath.Join(home, "sessions")
	if err := os.MkdirAll(sessions, 0o755); err != nil {
		t.Fatal(err)
	}
	oldPath := filepath.Join(sessions, "rollout-old.jsonl")
	newPath := filepath.Join(sessions, "rollout-new.jsonl")
	writeFile(t, oldPath, []byte("{}\n"))
	before := Discover(home, 1)
	if len(before.Files) != 1 {
		t.Fatalf("initial discovery = %+v", before)
	}
	if err := os.Rename(oldPath, newPath); err != nil {
		t.Fatal(err)
	}
	renamed := Discover(home, 2)
	if len(renamed.Files) != 1 || renamed.Files[0].Identity != before.Files[0].Identity || renamed.Files[0].Path != newPath {
		t.Fatalf("rename changed physical identity: before=%+v after=%+v", before.Files, renamed.Files)
	}
	parkedPath := filepath.Join(sessions, "old-rollout-preserved.tmp")
	if err := os.Rename(newPath, parkedPath); err != nil {
		t.Fatal(err)
	}
	replacement, err := os.CreateTemp(sessions, "replacement-*.tmp")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := replacement.Write([]byte("{\"replacement\":true}\n")); err != nil {
		replacement.Close()
		t.Fatal(err)
	}
	if err := replacement.Close(); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(replacement.Name(), newPath); err != nil {
		t.Fatal(err)
	}
	afterReplacement := Discover(home, 3)
	if len(afterReplacement.Files) != 1 || afterReplacement.Files[0].Identity == renamed.Files[0].Identity {
		t.Fatalf("replacement kept old physical identity: before=%+v after=%+v", renamed.Files, afterReplacement.Files)
	}
}

func writeFile(t *testing.T, path string, content []byte) {
	t.Helper()
	if err := os.WriteFile(path, content, 0o600); err != nil {
		t.Fatal(err)
	}
}
