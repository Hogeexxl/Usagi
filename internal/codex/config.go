package codex

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"os"
	"path/filepath"
	"strings"
)

const (
	stateIndexFilename   = "state_5.sqlite"
	sessionIndexFilename = "session_index.jsonl"
	globalStateFilename  = ".codex-global-state.json"
)

type MetadataPaths struct {
	StateIndex   string
	SessionIndex string
	GlobalState  string
}

type Config struct {
	Home            string
	HomeFingerprint string
	Metadata        MetadataPaths
}

type ConfigResolution struct {
	Config Config
	Err    error
}

type ConfigError struct {
	Code          string
	AttemptedHome string
	ResolvedHome  string
	MetadataPath  string
}

func (e *ConfigError) Error() string {
	if e == nil {
		return ""
	}
	return e.Code
}

func ResolveDefaultConfig() (Config, error) {
	if home := os.Getenv("CODEX_HOME"); home != "" {
		return ResolveConfigFromHome(home)
	}
	if home, err := os.UserHomeDir(); err == nil && home != "" {
		return ResolveConfigFromHome(filepath.Join(home, ".codex"))
	}
	return ResolveConfigFromHome(filepath.Join(".", ".codex"))
}

func ResolveConfigFromHome(home string) (Config, error) {
	normalized, err := NormalizePath(home)
	if err != nil {
		return Config{}, &ConfigError{Code: "CODEX_HOME_RESOLUTION_FAILED", AttemptedHome: home}
	}
	if !filepath.IsAbs(normalized) {
		return Config{}, &ConfigError{Code: "CODEX_HOME_NOT_ABSOLUTE", AttemptedHome: home, ResolvedHome: normalized}
	}
	if _, err := filepath.EvalSymlinks(normalized); err != nil {
		return Config{}, &ConfigError{Code: "CODEX_HOME_RESOLUTION_FAILED", AttemptedHome: home, ResolvedHome: normalized}
	}
	metadata := MetadataPaths{
		StateIndex:   filepath.Join(normalized, stateIndexFilename),
		SessionIndex: filepath.Join(normalized, sessionIndexFilename),
		GlobalState:  filepath.Join(normalized, globalStateFilename),
	}
	for _, path := range []string{metadata.StateIndex, metadata.SessionIndex, metadata.GlobalState} {
		if !metadataPathWithinHome(path, normalized) {
			return Config{}, &ConfigError{
				Code:          "CODEX_METADATA_HOME_MISMATCH",
				AttemptedHome: home,
				ResolvedHome:  normalized,
				MetadataPath:  path,
			}
		}
	}
	fingerprint := sha256.Sum256([]byte(normalized))
	return Config{Home: normalized, HomeFingerprint: hex.EncodeToString(fingerprint[:]), Metadata: metadata}, nil
}

func NormalizePath(path string) (string, error) {
	absolute, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	return filepath.Clean(absolute), nil
}

func metadataPathWithinHome(path, home string) bool {
	normalizedHome, err := NormalizePath(home)
	if err != nil {
		return false
	}
	normalizedPath, err := NormalizePath(path)
	if err != nil {
		return false
	}
	canonicalHome, err := filepath.EvalSymlinks(normalizedHome)
	if err != nil {
		return false
	}
	canonicalHome = filepath.Clean(canonicalHome)
	if !pathWithin(normalizedHome, normalizedPath) {
		return false
	}
	relative, err := filepath.Rel(normalizedHome, normalizedPath)
	if err != nil {
		return false
	}
	current := normalizedHome
	for _, component := range strings.Split(relative, string(filepath.Separator)) {
		if component == "" || component == "." {
			continue
		}
		current = filepath.Join(current, component)
		info, err := os.Lstat(current)
		if errors.Is(err, os.ErrNotExist) {
			return true
		}
		if err != nil {
			return false
		}
		if info.Mode()&os.ModeSymlink != 0 {
			resolved, err := filepath.EvalSymlinks(current)
			if err != nil || !pathWithin(canonicalHome, resolved) {
				return false
			}
		}
	}
	return true
}

func pathWithin(root, path string) bool {
	relative, err := filepath.Rel(root, path)
	if err != nil || filepath.IsAbs(relative) {
		return false
	}
	return relative == "." || (relative != ".." && !strings.HasPrefix(relative, ".."+string(filepath.Separator)))
}
