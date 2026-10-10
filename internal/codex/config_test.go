package codex

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

func TestConfigResolveDefaultUsesCodexHomeFirst(t *testing.T) {
	home := t.TempDir()
	t.Setenv("CODEX_HOME", home)

	config, err := ResolveDefaultConfig()
	if err != nil {
		t.Fatal(err)
	}
	want, _ := NormalizePath(home)
	if config.Home != want {
		t.Fatalf("Home = %q, want %q", config.Home, want)
	}
	if config.Metadata.StateIndex != filepath.Join(want, stateIndexFilename) ||
		config.Metadata.SessionIndex != filepath.Join(want, sessionIndexFilename) ||
		config.Metadata.GlobalState != filepath.Join(want, globalStateFilename) {
		t.Fatalf("Metadata paths = %+v", config.Metadata)
	}
	wantFingerprint := sha256.Sum256([]byte(want))
	if len(config.HomeFingerprint) != 64 || config.HomeFingerprint != hex.EncodeToString(wantFingerprint[:]) {
		t.Fatalf("HomeFingerprint = %q", config.HomeFingerprint)
	}
}

func TestConfigResolveDefaultFallsBackToUserHome(t *testing.T) {
	userHome := t.TempDir()
	if err := os.Mkdir(filepath.Join(userHome, ".codex"), 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("CODEX_HOME", "")
	if runtime.GOOS == "windows" {
		t.Setenv("USERPROFILE", userHome)
	} else {
		t.Setenv("HOME", userHome)
	}

	config, err := ResolveDefaultConfig()
	if err != nil {
		t.Fatal(err)
	}
	want, _ := NormalizePath(filepath.Join(userHome, ".codex"))
	if config.Home != want {
		t.Fatalf("Home = %q, want %q", config.Home, want)
	}
}

func TestConfigResolveFromHomeRejectsExternalSymlinkAndMissingChild(t *testing.T) {
	home := t.TempDir()
	external := t.TempDir()
	if err := os.Symlink(external, filepath.Join(home, "state_5.sqlite")); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}

	_, err := ResolveConfigFromHome(home)
	var configErr *ConfigError
	if !errors.As(err, &configErr) || configErr.Code != "CODEX_METADATA_HOME_MISMATCH" {
		t.Fatalf("ResolveConfigFromHome() error = %v, want metadata mismatch", err)
	}
}

func TestConfigMetadataPathWithinHomeAllowsInternalSymlink(t *testing.T) {
	home := t.TempDir()
	inside := filepath.Join(home, "inside")
	if err := os.Mkdir(inside, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(inside, filepath.Join(home, "metadata-link")); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}
	if !metadataPathWithinHome(filepath.Join(home, "metadata-link", "missing.sqlite"), home) {
		t.Fatal("internal symlink to a missing child was rejected")
	}
}

func TestConfigMetadataPathWithinHomeRejectsSymlinkThatLeavesAndReenters(t *testing.T) {
	parent := t.TempDir()
	home := filepath.Join(parent, "home")
	if err := os.Mkdir(home, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(parent, filepath.Join(home, "outside")); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}
	if got := metadataPathWithinHome(filepath.Join(home, "outside", "home", "missing.sqlite"), home); got {
		t.Fatal("path through an external symlink was accepted")
	}
}

func TestConfigResolveFromHomeRequiresResolvableHome(t *testing.T) {
	_, err := ResolveConfigFromHome(filepath.Join(t.TempDir(), "missing"))
	var configErr *ConfigError
	if !errors.As(err, &configErr) || configErr.Code != "CODEX_HOME_RESOLUTION_FAILED" {
		t.Fatalf("ResolveConfigFromHome() error = %v, want home resolution failure", err)
	}
}

func TestConfigPathWithinUsesPathComponents(t *testing.T) {
	if pathWithin(filepath.Join(string(filepath.Separator), "home", "codex"), filepath.Join(string(filepath.Separator), "home", "codex2")) {
		t.Fatal("path with a shared textual prefix was treated as a child")
	}
}
