package platform

import (
	"fmt"
	"path/filepath"

	"golang.org/x/sys/windows"
)

func UsagiDataDir() (string, error) {
	localAppData, err := windows.KnownFolderPath(windows.FOLDERID_LocalAppData, 0)
	if err != nil {
		return "", fmt.Errorf("resolve local app data directory: %w", err)
	}
	return filepath.Join(localAppData, "Usagi"), nil
}
