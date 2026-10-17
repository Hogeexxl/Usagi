package platform

import (
	"fmt"

	"golang.org/x/sys/windows"
)

var openBrowser = func(target string) error {
	targetUTF16, err := windows.UTF16PtrFromString(target)
	if err != nil {
		return fmt.Errorf("encode browser URL: %w", err)
	}
	return windows.ShellExecute(0, nil, targetUTF16, nil, nil, windows.SW_SHOWNORMAL)
}
