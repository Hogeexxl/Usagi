package main

import (
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	inputTokens  = 110
	outputTokens = 55
	sessionID    = "windows-smoke-session"
)

type fixture struct {
	fixtureDir string
	key        string
	now        time.Time
	logMu      sync.Mutex
}

func main() {
	if len(os.Args) < 2 {
		log.Fatal("expected healthcheck or web command")
	}
	switch os.Args[1] {
	case "healthcheck":
		if _, err := os.Stat(os.Getenv("USAGI_WINDOWS_MAGPIE_OFFLINE_FILE")); err == nil {
			os.Exit(1)
		}
		return
	case "web":
		runWeb(os.Args[2:])
	default:
		log.Fatalf("unsupported command %q", os.Args[1])
	}
}

func runWeb(arguments []string) {
	address := ""
	for index := 0; index < len(arguments); index++ {
		switch arguments[index] {
		case "--addr":
			if index+1 >= len(arguments) {
				log.Fatal("--addr requires a value")
			}
			address = arguments[index+1]
			index++
		case "--no-open":
		default:
			log.Fatalf("unsupported argument %q", arguments[index])
		}
	}
	if address == "" {
		log.Fatal("missing --addr")
	}
	key := os.Getenv("MAGPIE_WEB_KEY")
	if key == "" {
		log.Fatal("missing MAGPIE_WEB_KEY")
	}
	fixtureDir := os.Getenv("USAGI_WINDOWS_MAGPIE_FIXTURE_DIR")
	if fixtureDir == "" {
		log.Fatal("missing USAGI_WINDOWS_MAGPIE_FIXTURE_DIR")
	}
	if err := os.MkdirAll(fixtureDir, 0700); err != nil {
		log.Fatal(err)
	}
	f := &fixture{
		fixtureDir: fixtureDir,
		key:        key,
		now:        time.Now().Round(time.Second),
	}
	if err := os.WriteFile(filepath.Join(fixtureDir, "web.args"), []byte(strings.Join(arguments, "\n")), 0600); err != nil {
		log.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(fixtureDir, "magpie.pid"), []byte(strconv.Itoa(os.Getpid())), 0600); err != nil {
		log.Fatal(err)
	}

	server := &http.Server{
		Addr:              address,
		Handler:           f,
		ReadHeaderTimeout: 5 * time.Second,
	}
	fmt.Printf("magpie fixture web on http://%s\n", address)
	if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		log.Fatal(err)
	}
}

func (f *fixture) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path == "/" && r.URL.Query().Get("k") == f.key {
		http.SetCookie(w, &http.Cookie{
			Name:     "magpie-web",
			Value:    f.key,
			Path:     "/",
			HttpOnly: true,
		})
		f.writeJSON(w, http.StatusOK, map[string]any{})
		return
	}
	cookie, err := r.Cookie("magpie-web")
	if err != nil || cookie.Value != f.key {
		f.writeJSON(w, http.StatusUnauthorized, map[string]string{"error": "unauthorized"})
		return
	}
	f.record(r.Method + " " + r.URL.RequestURI())

	switch r.URL.Path {
	case "/api/sessions/progress":
		f.writeJSON(w, http.StatusOK, map[string]any{
			"indexing": false,
			"files":    1,
			"done":     1,
			"bytes":    0,
			"read":     0,
		})
	case "/api/sessions":
		f.writeJSON(w, http.StatusOK, map[string]any{
			"sessions": []any{map[string]any{
				"agent": "codex",
				"id":    sessionID,
				"cwd":   `C:\fixture\Usagi`,
				"title": "Windows Magpie recovery session",
				"start": f.now,
				"last":  f.now,
				"name":  "Codex",
				"path":  `C:\fixture\Usagi\session.jsonl`,
			}},
			"terminal": false,
			"dirs":     []string{},
		})
	case "/api/usage/quotas":
		f.writeJSON(w, http.StatusOK, []any{map[string]any{
			"provider": "claude",
			"name":     "Claude",
			"windows":  []any{},
		}})
	case "/api/usage":
		f.writeJSON(w, http.StatusOK, map[string]any{
			"period": f.period(r),
			"bucket": "day",
			"series": []any{map[string]any{
				"label":  f.now.Format(time.DateOnly),
				"time":   f.now,
				"calls":  1,
				"input":  inputTokens,
				"output": outputTokens,
			}},
			"calls":  1,
			"input":  inputTokens,
			"output": outputTokens,
		})
	case "/api/usage/requests":
		f.writeJSON(w, http.StatusOK, map[string]any{
			"period": f.period(r),
			"rows": []any{map[string]any{
				"t":            f.now,
				"agent":        "codex",
				"agentName":    "Codex",
				"provider":     "openai",
				"providerName": "OpenAI",
				"model":        "gpt-6-luna",
				"in":           inputTokens,
				"out":          outputTokens,
				"ms":           1000,
				"ttft_ms":      100,
				"status":       200,
				"session":      sessionID,
				"cost":         0,
				"priced":       false,
				"source":       "magpie-windows-fixture",
			}},
			"offset":    0,
			"total":     1,
			"bucket":    "day",
			"series":    []any{map[string]any{"label": f.now.Format(time.DateOnly), "time": f.now, "calls": 1, "input": inputTokens, "output": outputTokens}},
			"providers": []any{map[string]any{"id": "openai", "name": "OpenAI", "icon": ""}},
			"agents":    []any{map[string]any{"id": "codex", "name": "Codex", "icon": ""}},
			"purposes":  []string{},
			"accounts":  []any{},
			"through":   map[string]any{},
			"direct":    map[string]any{},
			"calls":     1,
			"input":     inputTokens,
			"output":    outputTokens,
		})
	default:
		f.writeJSON(w, http.StatusNotFound, map[string]string{"error": "not found"})
	}
}

func (f *fixture) period(r *http.Request) string {
	period := r.URL.Query().Get("period")
	if period == "" {
		return "all"
	}
	return period
}

func (f *fixture) writeJSON(w http.ResponseWriter, status int, value any) {
	body, err := json.Marshal(value)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Content-Length", strconv.Itoa(len(body)))
	w.WriteHeader(status)
	_, _ = w.Write(body)
}

func (f *fixture) record(value string) {
	f.logMu.Lock()
	defer f.logMu.Unlock()
	file, err := os.OpenFile(filepath.Join(f.fixtureDir, "requests.log"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0600)
	if err != nil {
		return
	}
	defer file.Close()
	_, _ = file.WriteString(strings.TrimSpace(value) + "\n")
}
