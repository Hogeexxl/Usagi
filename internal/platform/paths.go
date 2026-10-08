package platform

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
)

type Paths struct {
	ActivePath       string
	LegacyCandidates []string
}

func ResolvePaths(explicit string) (Paths, error) {
	if explicit != "" {
		active, err := NormalizePath(explicit)
		if err != nil {
			return Paths{}, err
		}
		return Paths{ActivePath: active}, nil
	}

	root, err := dataLocalDirectory()
	if err != nil {
		return Paths{}, err
	}
	active, err := NormalizePath(filepath.Join(root, "Usagi", "usagi.sqlite3"))
	if err != nil {
		return Paths{}, err
	}
	legacyUsagi, err := NormalizePath(filepath.Join(root, "Usagi", "mu.sqlite3"))
	if err != nil {
		return Paths{}, err
	}
	legacyMiniUsage, err := NormalizePath(filepath.Join(root, "MiniUsage", "mu.sqlite3"))
	if err != nil {
		return Paths{}, err
	}
	return Paths{
		ActivePath:       active,
		LegacyCandidates: []string{legacyUsagi, legacyMiniUsage},
	}, nil
}

func NormalizePath(path string) (string, error) {
	absolute, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	return filepath.Clean(absolute), nil
}

func dataLocalDirectory() (string, error) {
	switch runtime.GOOS {
	case "darwin":
		home, err := os.UserHomeDir()
		if err != nil {
			return "", fmt.Errorf("resolve user home directory: %w", err)
		}
		return filepath.Join(home, "Library", "Application Support"), nil
	case "windows":
		localAppData := os.Getenv("LOCALAPPDATA")
		if localAppData == "" {
			return "", fmt.Errorf("LOCALAPPDATA is empty")
		}
		return localAppData, nil
	default:
		return "", fmt.Errorf("unsupported platform %q", runtime.GOOS)
	}
}
