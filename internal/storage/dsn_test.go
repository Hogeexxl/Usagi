package storage

import (
	"net/url"
	"strings"
	"testing"
)

func TestSQLiteURI(t *testing.T) {
	got, err := buildSQLiteURI("/tmp/usagi.sqlite3", writerURIParams())
	if err != nil {
		t.Fatal(err)
	}
	parsed, err := url.Parse(got)
	if err != nil {
		t.Fatal(err)
	}
	if parsed.Scheme != "file" || parsed.Path != "/tmp/usagi.sqlite3" {
		t.Fatalf("URI = %q, want file path /tmp/usagi.sqlite3", got)
	}
	params := parsed.Query()
	for key, value := range map[string]string{
		"_journal_mode": "WAL",
		"_synchronous":  "NORMAL",
		"_foreign_keys": "1",
		"_busy_timeout": "5000",
		"_txlock":       "immediate",
	} {
		if params.Get(key) != value {
			t.Errorf("query %q = %q, want %q", key, params.Get(key), value)
		}
	}
}

func TestSQLiteURISpecialCharacters(t *testing.T) {
	path := "/tmp/用户 Usagi?name#draft%2.sqlite3"
	got, err := buildSQLiteURI(path, nil)
	if err != nil {
		t.Fatal(err)
	}
	parsed, err := url.Parse(got)
	if err != nil {
		t.Fatal(err)
	}
	if parsed.Path != path {
		t.Fatalf("URI path = %q, want %q", parsed.Path, path)
	}
	for _, escaped := range []string{"%3F", "%23", "%252", "%E7%94%A8"} {
		if !strings.Contains(strings.ToUpper(got), strings.ToUpper(escaped)) {
			t.Errorf("URI %q does not contain escaped component %q", got, escaped)
		}
	}
}

func TestSQLiteURIWindowsDrive(t *testing.T) {
	got, err := buildSQLiteURI(`C:\Users\用户\Usagi DB\usagi.sqlite3`, nil)
	if err != nil {
		t.Fatal(err)
	}
	parsed, err := url.Parse(got)
	if err != nil {
		t.Fatal(err)
	}
	if parsed.Scheme != "file" || parsed.Host != "" || parsed.Path != "/C:/Users/用户/Usagi DB/usagi.sqlite3" {
		t.Fatalf("URI = %q, want Windows drive file URI", got)
	}
}

func TestSQLiteURIWindowsUNC(t *testing.T) {
	got, err := buildSQLiteURI(`\\server\share\Usagi DB\usagi.sqlite3`, nil)
	if err != nil {
		t.Fatal(err)
	}
	parsed, err := url.Parse(got)
	if err != nil {
		t.Fatal(err)
	}
	if parsed.Scheme != "file" || parsed.Host != "server" || parsed.Path != "/share/Usagi DB/usagi.sqlite3" {
		t.Fatalf("URI = %q, want Windows UNC file URI", got)
	}
}
