package query

import (
	"path/filepath"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

func projectOf(session magpie.Session, matched bool) ProjectRef {
	if !matched {
		return ProjectRef{Kind: ProjectKindUnknown}
	}
	if session.Cwd == "" {
		return ProjectRef{Kind: ProjectKindProjectless}
	}
	return ProjectRef{Kind: ProjectKindProject, Path: session.Cwd, Name: filepath.Base(filepath.Clean(session.Cwd))}
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
