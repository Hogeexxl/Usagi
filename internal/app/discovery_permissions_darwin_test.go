package app

import (
	"os"
	"path/filepath"
	"testing"
)

func TestRuntimeDescriptorPermissions(t *testing.T) {
	dataRoot := filepath.Join(t.TempDir(), "Usagi")
	path := filepath.Join(dataRoot, runtimeDescriptorName("production"))
	descriptor := RuntimeDescriptor{
		SchemaVersion: 1,
		PID:           12345,
		Port:          37891,
		BaseURL:       "http://127.0.0.1:37891",
		RuntimeMode:   "production",
	}
	if err := writeRuntimeDescriptor(path, descriptor); err != nil {
		t.Fatal(err)
	}

	directoryInfo, err := os.Stat(dataRoot)
	if err != nil {
		t.Fatal(err)
	}
	if mode := directoryInfo.Mode().Perm(); mode != 0700 {
		t.Fatalf("descriptor directory mode = %04o, want 0700", mode)
	}
	fileInfo, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if mode := fileInfo.Mode().Perm(); mode != 0600 {
		t.Fatalf("descriptor mode = %04o, want 0600", mode)
	}
}
