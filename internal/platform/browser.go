//go:build darwin

package platform

import "os"

const disableBrowserEnvironment = "USAGI_DISABLE_BROWSER"

type Browser struct{}

func (Browser) Open(target string) error {
	if os.Getenv(disableBrowserEnvironment) == "1" {
		return nil
	}
	return openBrowser(target)
}
