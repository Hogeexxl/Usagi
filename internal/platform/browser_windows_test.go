package platform

import (
	"errors"
	"reflect"
	"testing"
)

func TestBrowserOpenUsesWindowsShell(t *testing.T) {
	original := openBrowser
	t.Cleanup(func() { openBrowser = original })

	wantErr := errors.New("shell execute failed")
	var opened []string
	openBrowser = func(target string) error {
		opened = append(opened, target)
		return wantErr
	}
	target := "http://127.0.0.1:37891/"
	if err := (Browser{}).Open(target); !errors.Is(err, wantErr) {
		t.Fatalf("Browser.Open() error = %v, want %v", err, wantErr)
	}
	if !reflect.DeepEqual(opened, []string{target}) {
		t.Fatalf("opened URLs = %v, want %q", opened, target)
	}
}

func TestBrowserCanBeDisabledByEnvironmentOnWindows(t *testing.T) {
	original := openBrowser
	t.Cleanup(func() { openBrowser = original })
	openBrowser = func(string) error {
		t.Fatal("disabled browser invoked Windows Shell")
		return nil
	}
	t.Setenv(disableBrowserEnvironment, "1")
	if err := (Browser{}).Open("http://127.0.0.1:37891/"); err != nil {
		t.Fatalf("Browser.Open() with browser disabled: %v", err)
	}
}
