package app

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
)

type RuntimeDescriptor struct {
	SchemaVersion int    `json:"schema_version"`
	PID           int    `json:"pid"`
	Port          int    `json:"port"`
	BaseURL       string `json:"base_url"`
	RuntimeMode   string `json:"runtime_mode"`
}

func RuntimeDescriptorPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("resolve user home directory: %w", err)
	}
	return runtimeDescriptorPath(home), nil
}

func runtimeDescriptorPath(home string) string {
	return filepath.Join(home, "Library", "Application Support", "Usagi", runtimeDescriptorName(runtimeMode()))
}

func runtimeDescriptorName(mode string) string {
	if mode == "production" {
		return "runtime.json"
	}
	return "runtime-dev.json"
}

func WriteRuntimeDescriptor(descriptor RuntimeDescriptor) error {
	if descriptor.RuntimeMode != runtimeMode() {
		return fmt.Errorf("runtime descriptor mode %q does not match build mode %q", descriptor.RuntimeMode, runtimeMode())
	}
	path, err := RuntimeDescriptorPath()
	if err != nil {
		return err
	}
	return writeRuntimeDescriptor(path, descriptor)
}

func writeRuntimeDescriptor(path string, descriptor RuntimeDescriptor) error {
	contents, err := json.Marshal(descriptor)
	if err != nil {
		return fmt.Errorf("encode runtime descriptor: %w", err)
	}
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return fmt.Errorf("create runtime descriptor directory: %w", err)
	}
	temporary, err := os.CreateTemp(filepath.Dir(path), ".runtime-*.tmp")
	if err != nil {
		return fmt.Errorf("create temporary runtime descriptor: %w", err)
	}
	temporaryPath := temporary.Name()
	closed := false
	renamed := false
	defer func() {
		if !closed {
			_ = temporary.Close()
		}
		if !renamed {
			_ = os.Remove(temporaryPath)
		}
	}()

	if err := temporary.Chmod(0600); err != nil {
		return fmt.Errorf("set runtime descriptor permissions: %w", err)
	}
	if _, err := temporary.Write(contents); err != nil {
		return fmt.Errorf("write temporary runtime descriptor: %w", err)
	}
	if err := temporary.Sync(); err != nil {
		return fmt.Errorf("sync temporary runtime descriptor: %w", err)
	}
	if err := temporary.Close(); err != nil {
		closed = true
		return fmt.Errorf("close temporary runtime descriptor: %w", err)
	}
	closed = true
	if err := os.Rename(temporaryPath, path); err != nil {
		return fmt.Errorf("replace runtime descriptor: %w", err)
	}
	renamed = true
	return nil
}

func RemoveRuntimeDescriptor(pid int) error {
	path, err := RuntimeDescriptorPath()
	if err != nil {
		return err
	}
	return removeRuntimeDescriptor(path, pid, runtimeMode())
}

func removeRuntimeDescriptor(path string, pid int, mode string) error {
	contents, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("read runtime descriptor before removal: %w", err)
	}
	var descriptor RuntimeDescriptor
	if err := json.Unmarshal(contents, &descriptor); err != nil {
		return nil
	}
	if descriptor.PID != pid || descriptor.RuntimeMode != mode {
		return nil
	}
	if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("remove runtime descriptor: %w", err)
	}
	return nil
}
