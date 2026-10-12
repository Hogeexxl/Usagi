//go:build darwin

package platform

import "os/exec"

var openBrowser = func(target string) error {
	return exec.Command("/usr/bin/open", target).Run()
}
