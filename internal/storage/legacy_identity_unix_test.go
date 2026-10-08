//go:build !windows

package storage

import "testing"

func assertWindowsExclusiveSourceOpen(t *testing.T, path string) {
	t.Helper()
	t.Fatal("Windows handle assertion called on Unix")
}
