package app

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/platform"
)

func TestRuntimeDescriptorPath(t *testing.T) {
	dataRoot := t.TempDir()
	want := filepath.Join(dataRoot, runtimeDescriptorName(runtimeMode()))
	if got := runtimeDescriptorPath(dataRoot); got != want {
		t.Fatalf("runtimeDescriptorPath = %q, want %q", got, want)
	}
	productionRoot, err := platform.UsagiDataDir()
	if err != nil {
		t.Fatal(err)
	}
	productionPath, err := RuntimeDescriptorPath()
	if err != nil {
		t.Fatal(err)
	}
	if want := runtimeDescriptorPath(productionRoot); productionPath != want {
		t.Fatalf("RuntimeDescriptorPath() = %q, want %q", productionPath, want)
	}
	if got := runtimeDescriptorName("production"); got != "runtime.json" {
		t.Fatalf("production descriptor name = %q", got)
	}
	if got := runtimeDescriptorName("development"); got != "runtime-dev.json" {
		t.Fatalf("development descriptor name = %q", got)
	}
}

func TestRuntimeDescriptorWriteAndModeOwnedRemoval(t *testing.T) {
	root := t.TempDir()
	directory := filepath.Join(root, "Usagi")
	productionPath := filepath.Join(directory, runtimeDescriptorName("production"))
	developmentPath := filepath.Join(directory, runtimeDescriptorName("development"))
	production := RuntimeDescriptor{SchemaVersion: 1, PID: 12345, Port: 37891, BaseURL: "http://127.0.0.1:37891", RuntimeMode: "production"}
	development := RuntimeDescriptor{SchemaVersion: 1, PID: 23456, Port: 37991, BaseURL: "http://127.0.0.1:37991", RuntimeMode: "development"}
	if err := writeRuntimeDescriptor(productionPath, production); err != nil {
		t.Fatal(err)
	}
	if err := writeRuntimeDescriptor(developmentPath, development); err != nil {
		t.Fatal(err)
	}
	checkRuntimeDescriptor(t, productionPath, production)
	checkRuntimeDescriptor(t, developmentPath, development)

	if err := removeRuntimeDescriptor(productionPath, production.PID+1, production.RuntimeMode); err != nil {
		t.Fatal(err)
	}
	checkRuntimeDescriptor(t, productionPath, production)
	if err := removeRuntimeDescriptor(productionPath, production.PID, development.RuntimeMode); err != nil {
		t.Fatal(err)
	}
	checkRuntimeDescriptor(t, productionPath, production)

	if err := removeRuntimeDescriptor(productionPath, production.PID, production.RuntimeMode); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(productionPath); !os.IsNotExist(err) {
		t.Fatalf("production descriptor still exists after removal: %v", err)
	}
	checkRuntimeDescriptor(t, developmentPath, development)
	if err := removeRuntimeDescriptor(productionPath, production.PID, production.RuntimeMode); err != nil {
		t.Fatalf("removing absent runtime descriptor: %v", err)
	}
	if err := removeRuntimeDescriptor(developmentPath, development.PID, development.RuntimeMode); err != nil {
		t.Fatal(err)
	}
}

func checkRuntimeDescriptor(t *testing.T, path string, want RuntimeDescriptor) {
	t.Helper()
	contents, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var got RuntimeDescriptor
	if err := json.Unmarshal(contents, &got); err != nil {
		t.Fatalf("descriptor JSON = %q: %v", contents, err)
	}
	if got != want {
		t.Fatalf("descriptor = %+v, want %+v", got, want)
	}
}

type fakeBrowser struct {
	opened []string
	err    error
}

func (b *fakeBrowser) Open(target string) error {
	b.opened = append(b.opened, target)
	return b.err
}
