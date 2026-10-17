package platform

import (
	"fmt"
	"os"
	"path/filepath"
)

func UsagiDataDir() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("resolve user home directory: %w", err)
	}
	return filepath.Join(home, "Library", "Application Support", "Usagi"), nil
}
