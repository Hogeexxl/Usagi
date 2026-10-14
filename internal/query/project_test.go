package query

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

func TestProjectOf(t *testing.T) {
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatalf("UserHomeDir: %v", err)
	}
	codexRoot := filepath.Join(home, "Documents", "Codex")
	scratchRoot := filepath.Join(home, "Library", "Application Support", "Claude", "scratch-workspaces")
	separator := string(filepath.Separator)
	codexRawPath := codexRoot + separator + "2026-10-14" + separator + "task-slug" + separator + ".." + separator + "task-slug" + separator + "child"

	tests := []struct {
		name    string
		cwd     string
		matched bool
		want    ProjectRef
	}{
		{name: "unmatched metadata is unknown", cwd: "/some/path", want: ProjectRef{Kind: ProjectKindUnknown}},
		{name: "empty matched cwd is projectless", matched: true, want: ProjectRef{Kind: ProjectKindProjectless}},
		{name: "home directory remains a project", cwd: home, matched: true, want: ProjectRef{Kind: ProjectKindProject, Path: home, Name: filepath.Base(home)}},
		{name: "regular project", cwd: "/Users/test/my-project", matched: true, want: ProjectRef{Kind: ProjectKindProject, Path: "/Users/test/my-project", Name: "my-project"}},
		{name: "Codex date and slug with children", cwd: codexRawPath, matched: true, want: ProjectRef{Kind: ProjectKindWorkspace, Path: codexRawPath}},
		{name: "Codex path without slug is ordinary", cwd: filepath.Join(codexRoot, "2026-10-14"), matched: true, want: ProjectRef{Kind: ProjectKindProject, Path: filepath.Join(codexRoot, "2026-10-14"), Name: "2026-10-14"}},
		{name: "Codex path without date is ordinary", cwd: filepath.Join(codexRoot, "adhoc", "task-slug"), matched: true, want: ProjectRef{Kind: ProjectKindProject, Path: filepath.Join(codexRoot, "adhoc", "task-slug"), Name: "task-slug"}},
		{name: "Codex invalid date is ordinary", cwd: filepath.Join(codexRoot, "2026-02-30", "task-slug"), matched: true, want: ProjectRef{Kind: ProjectKindProject, Path: filepath.Join(codexRoot, "2026-02-30", "task-slug"), Name: "task-slug"}},
		{name: "Claude scratch workspace", cwd: filepath.Join(scratchRoot, "scratch-1", "child"), matched: true, want: ProjectRef{Kind: ProjectKindWorkspace, Path: filepath.Join(scratchRoot, "scratch-1", "child")}},
		{name: "scratch-workspaces in an unrelated path is ordinary", cwd: "/repo/scratch-workspaces/session-1", matched: true, want: ProjectRef{Kind: ProjectKindProject, Path: "/repo/scratch-workspaces/session-1", Name: "session-1"}},
		{name: "Claude tmp workspace", cwd: "/tmp/claude-501/child", matched: true, want: ProjectRef{Kind: ProjectKindWorkspace, Path: "/tmp/claude-501/child"}},
		{name: "Claude private tmp workspace", cwd: "/private/tmp/claude-501", matched: true, want: ProjectRef{Kind: ProjectKindWorkspace, Path: "/private/tmp/claude-501"}},
		{name: "empty Claude sandbox id is ordinary", cwd: "/tmp/claude-", matched: true, want: ProjectRef{Kind: ProjectKindProject, Path: "/tmp/claude-", Name: "claude-"}},
		{name: "ordinary tmp directory is a project", cwd: "/tmp/my-project", matched: true, want: ProjectRef{Kind: ProjectKindProject, Path: "/tmp/my-project", Name: "my-project"}},
		{name: "Magpie workspace under macOS temp root", cwd: "/private/var/folders/xx/yy/T/magpie-claude-work-501/child", matched: true, want: ProjectRef{Kind: ProjectKindWorkspace, Path: "/private/var/folders/xx/yy/T/magpie-claude-work-501/child"}},
		{name: "Magpie workspace under unprefixed macOS temp root", cwd: "/var/folders/xx/yy/T/magpie-claude-work-501", matched: true, want: ProjectRef{Kind: ProjectKindWorkspace, Path: "/var/folders/xx/yy/T/magpie-claude-work-501"}},
		{name: "Magpie workspace directly under tmp", cwd: "/tmp/magpie-claude-work-501/child", matched: true, want: ProjectRef{Kind: ProjectKindWorkspace, Path: "/tmp/magpie-claude-work-501/child"}},
		{name: "Magpie name elsewhere is ordinary", cwd: "/repo/magpie-claude-work-501", matched: true, want: ProjectRef{Kind: ProjectKindProject, Path: "/repo/magpie-claude-work-501", Name: "magpie-claude-work-501"}},
		{name: "Magpie name below unrelated tmp child is ordinary", cwd: "/tmp/other/magpie-claude-work-501", matched: true, want: ProjectRef{Kind: ProjectKindProject, Path: "/tmp/other/magpie-claude-work-501", Name: "magpie-claude-work-501"}},
		{name: "Magpie name below T is ordinary", cwd: "/var/folders/xx/yy/T/other/magpie-claude-work-501", matched: true, want: ProjectRef{Kind: ProjectKindProject, Path: "/var/folders/xx/yy/T/other/magpie-claude-work-501", Name: "magpie-claude-work-501"}},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got := projectOf(magpie.Session{Cwd: test.cwd}, test.matched)
			if got != test.want {
				t.Fatalf("projectOf() = %+v, want %+v", got, test.want)
			}
		})
	}
}

func TestMatchesProjects(t *testing.T) {
	workspace := ProjectRef{Kind: ProjectKindWorkspace, Path: "/tmp/claude-1", Name: "unused"}
	projectA := ProjectRef{Kind: ProjectKindProject, Path: "/work/one/same-name", Name: "same-name"}
	projectB := ProjectRef{Kind: ProjectKindProject, Path: "/work/two/same-name", Name: "same-name"}

	workspaceSelection := []ProjectSelection{{Kind: ProjectKindWorkspace}}
	if !matchesProjects(workspace, workspaceSelection) {
		t.Fatal("workspace selection did not match workspace")
	}
	if matchesProjects(ProjectRef{Kind: ProjectKindProject, Path: workspace.Path}, workspaceSelection) {
		t.Fatal("workspace selection matched a regular project with the same path")
	}
	if !matchesProjects(projectA, []ProjectSelection{{Kind: ProjectKindProject, Path: projectA.Path}}) ||
		matchesProjects(projectB, []ProjectSelection{{Kind: ProjectKindProject, Path: projectA.Path}}) {
		t.Fatal("regular project selections did not distinguish full paths")
	}
	union := []ProjectSelection{{Kind: ProjectKindProject, Path: projectA.Path}, {Kind: ProjectKindWorkspace}}
	if !matchesProjects(projectA, union) || !matchesProjects(workspace, union) || matchesProjects(projectB, union) {
		t.Fatal("multiple project selections did not retain union semantics")
	}
}
