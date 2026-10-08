package platform

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

func TestResolvePathsExplicit(t *testing.T) {
	paths, err := ResolvePaths(filepath.Join("relative", "usagi.sqlite3"))
	if err != nil {
		t.Fatal(err)
	}
	want, err := filepath.Abs(filepath.Join("relative", "usagi.sqlite3"))
	if err != nil {
		t.Fatal(err)
	}
	if paths.ActivePath != filepath.Clean(want) {
		t.Fatalf("ActivePath = %q, want %q", paths.ActivePath, filepath.Clean(want))
	}
	if len(paths.LegacyCandidates) != 0 {
		t.Fatalf("LegacyCandidates = %v, want none", paths.LegacyCandidates)
	}
}

func TestResolvePathsDefault(t *testing.T) {
	paths, err := ResolvePaths("")
	if err != nil {
		t.Fatal(err)
	}
	if !filepath.IsAbs(paths.ActivePath) {
		t.Fatalf("ActivePath = %q, want absolute path", paths.ActivePath)
	}
	if filepath.Base(paths.ActivePath) != "usagi.sqlite3" {
		t.Fatalf("ActivePath = %q, want usagi.sqlite3", paths.ActivePath)
	}
	if len(paths.LegacyCandidates) != 2 {
		t.Fatalf("LegacyCandidates = %v, want two candidates", paths.LegacyCandidates)
	}
	if filepath.Base(paths.LegacyCandidates[0]) != "mu.sqlite3" || filepath.Base(paths.LegacyCandidates[1]) != "mu.sqlite3" {
		t.Fatalf("LegacyCandidates = %v, want mu.sqlite3 paths", paths.LegacyCandidates)
	}
	switch runtime.GOOS {
	case "darwin":
		home, err := os.UserHomeDir()
		if err != nil {
			t.Fatal(err)
		}
		want := filepath.Join(home, "Library", "Application Support", "Usagi", "usagi.sqlite3")
		if paths.ActivePath != want {
			t.Fatalf("ActivePath = %q, want %q", paths.ActivePath, want)
		}
	case "windows":
		want := filepath.Join(os.Getenv("LOCALAPPDATA"), "Usagi", "usagi.sqlite3")
		if paths.ActivePath != want {
			t.Fatalf("ActivePath = %q, want %q", paths.ActivePath, want)
		}
	}
}

func TestNormalizePathCleansComponents(t *testing.T) {
	got, err := NormalizePath(filepath.Join("relative", "..", "usagi.sqlite3"))
	if err != nil {
		t.Fatal(err)
	}
	if !filepath.IsAbs(got) || filepath.Base(got) != "usagi.sqlite3" {
		t.Fatalf("NormalizePath() = %q, want cleaned absolute path", got)
	}
}
