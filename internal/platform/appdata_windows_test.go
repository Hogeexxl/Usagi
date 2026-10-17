package platform

import (
	"path/filepath"
	"testing"

	"golang.org/x/sys/windows"
)

func TestUsagiDataDirUsesLocalAppData(t *testing.T) {
	localAppData, err := windows.KnownFolderPath(windows.FOLDERID_LocalAppData, 0)
	if err != nil {
		t.Fatal(err)
	}

	got, err := UsagiDataDir()
	if err != nil {
		t.Fatal(err)
	}
	want := filepath.Join(localAppData, "Usagi")
	if got != want {
		t.Fatalf("UsagiDataDir() = %q, want %q", got, want)
	}
}
