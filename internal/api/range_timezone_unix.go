//go:build !windows

package api

import (
	"os"
	"path/filepath"
	"strings"
)

func systemTimezoneFromOS() (string, error) {
	path, err := os.Readlink("/etc/localtime")
	if err != nil {
		return "", err
	}
	if !filepath.IsAbs(path) {
		path = filepath.Join("/etc", path)
	}
	return timezoneNameFromPath(path)
}

func timezoneNameFromPath(path string) (string, error) {
	resolved, err := filepath.EvalSymlinks(path)
	if err == nil {
		path = resolved
	}
	const marker = string(filepath.Separator) + "zoneinfo" + string(filepath.Separator)
	index := strings.LastIndex(path, marker)
	if index < 0 {
		return "", ErrLocalTimeUnavailable
	}
	name := path[index+len(marker):]
	if !validTimezoneName(name) {
		return "", ErrLocalTimeUnavailable
	}
	return name, nil
}
