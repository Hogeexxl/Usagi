package storage

import (
	"fmt"
	"net/url"
	pathpkg "path"
	"strings"

	"github.com/Hogeexxl/Usagi/internal/platform"
)

func buildSQLiteURI(path string, params url.Values) (string, error) {
	var uri url.URL
	uri.Scheme = "file"
	uri.RawQuery = params.Encode()

	switch {
	case strings.HasPrefix(path, `\\`):
		host, filePath, err := normalizeWindowsUNCPath(path)
		if err != nil {
			return "", err
		}
		uri.Host = host
		uri.Path = filePath
	case isWindowsDrivePath(path):
		filePath, err := normalizeWindowsDrivePath(path)
		if err != nil {
			return "", err
		}
		uri.Path = filePath
	default:
		absolute, err := platform.NormalizePath(path)
		if err != nil {
			return "", err
		}
		uri.Path = absolute
	}
	return uri.String(), nil
}

func writerURIParams() url.Values {
	return url.Values{
		"_busy_timeout": {"5000"},
		"_foreign_keys": {"1"},
		"_journal_mode": {"WAL"},
		"_synchronous":  {"NORMAL"},
		"_txlock":       {"immediate"},
	}
}

func readerURIParams() url.Values {
	return url.Values{
		"_busy_timeout": {"5000"},
		"_foreign_keys": {"1"},
		"_query_only":   {"1"},
		"mode":          {"ro"},
	}
}

func inspectionURIParams() url.Values {
	return url.Values{
		"_busy_timeout": {"5000"},
		"_query_only":   {"1"},
		"mode":          {"ro"},
	}
}

func maintenanceURIParams() url.Values {
	return url.Values{
		"_busy_timeout": {"5000"},
		"_foreign_keys": {"1"},
		"_txlock":       {"immediate"},
		"mode":          {"rw"},
	}
}

func importURIParams() url.Values {
	return url.Values{
		"_busy_timeout": {"5000"},
		"_foreign_keys": {"1"},
		"_query_only":   {"1"},
		"mode":          {"ro"},
	}
}

func isWindowsDrivePath(path string) bool {
	return len(path) >= 3 &&
		((path[0] >= 'a' && path[0] <= 'z') || (path[0] >= 'A' && path[0] <= 'Z')) &&
		path[1] == ':' && (path[2] == '/' || path[2] == '\\')
}

func normalizeWindowsDrivePath(path string) (string, error) {
	normalized := strings.ReplaceAll(path, `\`, "/")
	cleaned := pathpkg.Clean("/" + normalized[2:])
	return "/" + normalized[:2] + cleaned, nil
}

func normalizeWindowsUNCPath(path string) (string, string, error) {
	parts := strings.Split(strings.TrimLeft(strings.ReplaceAll(path, `\`, "/"), "/"), "/")
	if len(parts) < 2 || parts[0] == "" || parts[1] == "" {
		return "", "", fmt.Errorf("invalid Windows UNC path %q", path)
	}
	return parts[0], pathpkg.Clean("/" + strings.Join(parts[1:], "/")), nil
}
