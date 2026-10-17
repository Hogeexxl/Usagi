//go:build !windows

package magpie

import "os/exec"

func configureCommand(*exec.Cmd) {}
