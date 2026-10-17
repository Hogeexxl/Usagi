package platform

import (
	"path/filepath"
	"testing"
)

func TestUsagiDataDirUsesApplicationSupport(t *testing.T) {
	home := filepath.Join(t.TempDir(), "home")
	t.Setenv("HOME", home)

	got, err := UsagiDataDir()
	if err != nil {
		t.Fatal(err)
	}
	want := filepath.Join(home, "Library", "Application Support", "Usagi")
	if got != want {
		t.Fatalf("UsagiDataDir() = %q, want %q", got, want)
	}
}
