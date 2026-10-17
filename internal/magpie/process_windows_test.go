package magpie

import (
	"os/exec"
	"testing"

	"golang.org/x/sys/windows"
)

func TestConfigureCommandUsesNoWindowCreationFlag(t *testing.T) {
	cmd := exec.Command("magpie.exe")
	configureCommand(cmd)
	if cmd.SysProcAttr == nil {
		t.Fatal("configureCommand() did not set SysProcAttr")
	}
	if got := cmd.SysProcAttr.CreationFlags; got != windows.CREATE_NO_WINDOW {
		t.Fatalf("CreationFlags = %#x, want %#x", got, windows.CREATE_NO_WINDOW)
	}
}
