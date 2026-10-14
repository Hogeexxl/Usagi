package query

import (
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

func projectOf(session magpie.Session, matched bool) ProjectRef {
	if !matched {
		return ProjectRef{Kind: ProjectKindUnknown}
	}
	if session.Cwd == "" {
		return ProjectRef{Kind: ProjectKindProjectless}
	}
	if isKnownWorkspacePath(session.Cwd) {
		return ProjectRef{Kind: ProjectKindWorkspace, Path: session.Cwd}
	}
	return ProjectRef{Kind: ProjectKindProject, Path: session.Cwd, Name: filepath.Base(filepath.Clean(session.Cwd))}
}

func isKnownWorkspacePath(cwd string) bool {
	path := filepath.Clean(cwd)
	home, _ := os.UserHomeDir()
	if home != "" {
		if parts, ok := pathComponentsBelow(filepath.Join(home, "Documents", "Codex"), path); ok && len(parts) >= 2 {
			if _, err := time.Parse(time.DateOnly, parts[0]); err == nil && parts[1] != "" {
				return true
			}
		}
		if parts, ok := pathComponentsBelow(filepath.Join(home, "Library", "Application Support", "Claude", "scratch-workspaces"), path); ok && len(parts) > 0 {
			return true
		}
	}

	for _, root := range []string{"/tmp", "/private/tmp"} {
		if parts, ok := pathComponentsBelow(root, path); ok && len(parts) > 0 {
			if hasNonemptyPrefix(parts[0], "claude-") || hasNonemptyPrefix(parts[0], "magpie-claude-work-") {
				return true
			}
		}
	}

	parts := strings.Split(strings.TrimPrefix(path, string(filepath.Separator)), string(filepath.Separator))
	var foldersIndex int
	if len(parts) >= 2 && parts[0] == "var" && parts[1] == "folders" {
		foldersIndex = 1
	} else if len(parts) >= 3 && parts[0] == "private" && parts[1] == "var" && parts[2] == "folders" {
		foldersIndex = 2
	} else {
		return false
	}
	return len(parts) > foldersIndex+4 && parts[foldersIndex+1] != "" && parts[foldersIndex+2] != "" &&
		parts[foldersIndex+3] == "T" && hasNonemptyPrefix(parts[foldersIndex+4], "magpie-claude-work-")
}

func pathComponentsBelow(root, path string) ([]string, bool) {
	relative, err := filepath.Rel(root, path)
	if err != nil || relative == "." || relative == ".." || filepath.IsAbs(relative) || strings.HasPrefix(relative, ".."+string(filepath.Separator)) {
		return nil, false
	}
	return strings.Split(relative, string(filepath.Separator)), true
}

func hasNonemptyPrefix(value, prefix string) bool {
	return strings.HasPrefix(value, prefix) && len(value) > len(prefix)
}

func matchesProjects(project ProjectRef, selections []ProjectSelection) bool {
	if len(selections) == 0 {
		return true
	}
	for _, selection := range selections {
		if selection.Kind == project.Kind && (selection.Kind != ProjectKindProject || selection.Path == project.Path) {
			return true
		}
	}
	return false
}
