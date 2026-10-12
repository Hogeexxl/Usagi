package app

import (
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestRuntimeDescriptorPath(t *testing.T) {
	home := filepath.Join(t.TempDir(), "home")
	want := filepath.Join(home, "Library", "Application Support", "Usagi", "runtime.json")
	if got := runtimeDescriptorPath(home); got != want {
		t.Fatalf("runtimeDescriptorPath = %q, want %q", got, want)
	}
}

func TestRuntimeDescriptorBuildModeBehavior(t *testing.T) {
	root := t.TempDir()
	path := filepath.Join(root, "Usagi", "runtime.json")
	descriptor := RuntimeDescriptor{SchemaVersion: 1, PID: 12345, Port: 37891, BaseURL: "http://127.0.0.1:37891"}

	if !RuntimeDescriptorEnabled() {
		if err := writeRuntimeDescriptor(path, descriptor); err != nil {
			t.Fatal(err)
		}
		if err := removeRuntimeDescriptor(path); err != nil {
			t.Fatal(err)
		}
		if _, err := os.Stat(filepath.Dir(path)); !os.IsNotExist(err) {
			t.Fatalf("development descriptor operation created parent directory: %v", err)
		}

		if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
			t.Fatal(err)
		}
		original := []byte("existing production descriptor")
		if err := os.WriteFile(path, original, 0600); err != nil {
			t.Fatal(err)
		}
		if err := writeRuntimeDescriptor(path, descriptor); err != nil {
			t.Fatal(err)
		}
		if err := removeRuntimeDescriptor(path); err != nil {
			t.Fatal(err)
		}
		got, err := os.ReadFile(path)
		if err != nil || !reflect.DeepEqual(got, original) {
			t.Fatalf("development descriptor operation changed existing file: %q, %v", got, err)
		}
		return
	}

	if err := writeRuntimeDescriptor(path, descriptor); err != nil {
		t.Fatal(err)
	}
	checkRuntimeDescriptor(t, path, descriptor)

	replacement := RuntimeDescriptor{SchemaVersion: 1, PID: 23456, Port: 37892, BaseURL: "http://127.0.0.1:37892"}
	if err := writeRuntimeDescriptor(path, replacement); err != nil {
		t.Fatal(err)
	}
	checkRuntimeDescriptor(t, path, replacement)

	if err := removeRuntimeDescriptor(path); err != nil {
		t.Fatal(err)
	}
	if err := removeRuntimeDescriptor(path); err != nil {
		t.Fatalf("removing absent runtime descriptor: %v", err)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatalf("descriptor still exists after removal: %v", err)
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
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if mode := info.Mode().Perm(); mode != 0600 {
		t.Fatalf("descriptor mode = %04o, want 0600", mode)
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
