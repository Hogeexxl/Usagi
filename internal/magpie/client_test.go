package magpie

import (
	"context"
	"errors"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
)

func TestClientEndpoints(t *testing.T) {
	tests := []struct {
		name  string
		path  string
		query url.Values
		call  func(*testing.T, *Client)
	}{
		{
			name:  "usage period",
			path:  "/api/usage",
			query: url.Values{"period": {"today"}},
			call: func(t *testing.T, client *Client) {
				got, err := client.Usage(context.Background(), "today")
				if err != nil {
					t.Fatal(err)
				}
				if got.Period != "today" || got.Input != 42 {
					t.Fatalf("Usage() = %+v", got)
				}
			},
		},
		{
			name:  "usage empty period",
			path:  "/api/usage",
			query: url.Values{},
			call: func(t *testing.T, client *Client) {
				if _, err := client.Usage(context.Background(), ""); err != nil {
					t.Fatal(err)
				}
			},
		},
		{
			name: "usage requests filters",
			path: "/api/usage/requests",
			query: url.Values{
				"period":   {"today"},
				"day":      {"2026-10-11"},
				"model":    {"model & gpt"},
				"agent":    {"codex"},
				"provider": {"open ai"},
				"purpose":  {"code"},
				"account":  {"primary"},
				"failed":   {"1"},
				"q":        {"hello world & more"},
				"via":      {"desktop"},
				"offset":   {"20"},
				"limit":    {"50"},
			},
			call: func(t *testing.T, client *Client) {
				got, err := client.UsageRequests(context.Background(), UsageRequestsQuery{
					Period: "today", Day: "2026-10-11", Model: "model & gpt", Agent: "codex",
					Provider: "open ai", Purpose: "code", Account: "primary", Failed: true,
					Search: "hello world & more", Via: "desktop", Offset: 20, Limit: 50,
				})
				if err != nil {
					t.Fatal(err)
				}
				if len(got.Rows) != 1 {
					t.Fatalf("UsageRequests() rows = %d, want 1", len(got.Rows))
				}
				row := got.Rows[0]
				if row.Effort != "high" || row.Session != "session-1" || row.Subagent != "worker" ||
					row.ParentAgent != "codex" || row.Cost != 0.125 {
					t.Fatalf("UsageRequests() row = %+v", row)
				}
			},
		},
		{
			name:  "usage requests omit zero values",
			path:  "/api/usage/requests",
			query: url.Values{},
			call: func(t *testing.T, client *Client) {
				if _, err := client.UsageRequests(context.Background(), UsageRequestsQuery{}); err != nil {
					t.Fatal(err)
				}
			},
		},
		{
			name:  "quotas asked",
			path:  "/api/usage/quotas",
			query: url.Values{"asked": {"1"}},
			call: func(t *testing.T, client *Client) {
				got, err := client.Quotas(context.Background(), true)
				if err != nil {
					t.Fatal(err)
				}
				checkQuota(t, got, true)
			},
		},
		{
			name:  "quotas not asked",
			path:  "/api/usage/quotas",
			query: url.Values{},
			call: func(t *testing.T, client *Client) {
				got, err := client.Quotas(context.Background(), false)
				if err != nil {
					t.Fatal(err)
				}
				checkQuota(t, got, false)
			},
		},
		{
			name:  "sessions limit",
			path:  "/api/sessions",
			query: url.Values{"limit": {"10"}},
			call: func(t *testing.T, client *Client) {
				got, err := client.Sessions(context.Background(), 10)
				if err != nil {
					t.Fatal(err)
				}
				if len(got.Sessions) != 1 || got.Sessions[0].Cwd != "/project" ||
					len(got.Sessions[0].Models) != 1 || len(got.Sessions[0].Via) != 1 ||
					got.Sessions[0].Via[0].Effort != "high" {
					t.Fatalf("Sessions() = %+v", got)
				}
			},
		},
		{
			name:  "sessions omit zero limit",
			path:  "/api/sessions",
			query: url.Values{},
			call: func(t *testing.T, client *Client) {
				if _, err := client.Sessions(context.Background(), 0); err != nil {
					t.Fatal(err)
				}
			},
		},
		{
			name:  "session key",
			path:  "/api/sessions/one",
			query: url.Values{"key": {"codex:session /one"}},
			call: func(t *testing.T, client *Client) {
				got, err := client.Session(context.Background(), "codex:session /one")
				if err != nil {
					t.Fatal(err)
				}
				if got.Cwd != "/project" || len(got.Models) != 1 || len(got.Via) != 1 {
					t.Fatalf("Session() = %+v", got)
				}
			},
		},
		{
			name:  "session stats days",
			path:  "/api/sessions/stats",
			query: url.Values{"days": {"7"}},
			call: func(t *testing.T, client *Client) {
				got, err := client.SessionStats(context.Background(), 7)
				if err != nil {
					t.Fatal(err)
				}
				if len(got.Days) != 1 || got.Days[0].Date != "2026-10-11" || got.Days[0].Usage[0].Cost != 0.25 {
					t.Fatalf("SessionStats() = %+v", got)
				}
			},
		},
		{
			name:  "session stats omit zero days",
			path:  "/api/sessions/stats",
			query: url.Values{},
			call: func(t *testing.T, client *Client) {
				if _, err := client.SessionStats(context.Background(), 0); err != nil {
					t.Fatal(err)
				}
			},
		},
		{
			name: "session overview filters",
			path: "/api/sessions/overview",
			query: url.Values{
				"days":  {"3"},
				"agent": {"codex"},
				"model": {"model & gpt"},
				"cwd":   {"/Users/hogee/Projects/Usagi & notes"},
			},
			call: func(t *testing.T, client *Client) {
				got, err := client.SessionOverview(context.Background(), SessionOverviewQuery{
					Days: 3, Agent: "codex", Model: "model & gpt", Cwd: "/Users/hogee/Projects/Usagi & notes",
				})
				if err != nil {
					t.Fatal(err)
				}
				if len(got.Tools.Top) != 1 || got.Tools.Top[0].Name != "shell" ||
					len(got.Skills.Top) != 1 || got.Skills.Top[0].Name != "review" ||
					len(got.Skills.Top[0].Projects) != 1 || got.Skills.Top[0].Projects[0].Name != "Usagi" {
					t.Fatalf("SessionOverview() = %+v", got)
				}
			},
		},
		{
			name:  "session overview omits zero values",
			path:  "/api/sessions/overview",
			query: url.Values{},
			call: func(t *testing.T, client *Client) {
				if _, err := client.SessionOverview(context.Background(), SessionOverviewQuery{}); err != nil {
					t.Fatal(err)
				}
			},
		},
		{
			name:  "session progress",
			path:  "/api/sessions/progress",
			query: url.Values{},
			call: func(t *testing.T, client *Client) {
				got, err := client.SessionProgress(context.Background())
				if err != nil {
					t.Fatal(err)
				}
				if !got.Indexing || got.Files != 8 || got.Done != 3 || got.Bytes != 1024 || got.Read != 512 {
					t.Fatalf("SessionProgress() = %+v", got)
				}
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.Method != http.MethodGet {
					t.Errorf("method = %q, want GET", r.Method)
				}
				if r.URL.Path != test.path {
					t.Errorf("path = %q, want %q", r.URL.Path, test.path)
				}
				if r.URL.RawQuery != test.query.Encode() {
					t.Errorf("raw query = %q, want %q", r.URL.RawQuery, test.query.Encode())
				}
				if r.URL.Query().Has("k") {
					t.Error("business API request contains the Web Key query parameter")
				}
				if r.URL.Path == "/api/usage/quotas" {
					reading := "0"
					if r.URL.Query().Get("asked") == "1" {
						reading = "1"
					}
					w.Header().Set("X-Magpie-Reading", reading)
				}
				_, _ = w.Write([]byte(clientResponseJSON(r.URL.Path)))
			}))
			t.Cleanup(server.Close)

			client := &Client{baseURL: server.URL, httpClient: server.Client()}
			test.call(t, client)
		})
	}
}

func TestClientNon2xx(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, "unavailable", http.StatusServiceUnavailable)
	}))
	defer server.Close()

	client := &Client{baseURL: server.URL, httpClient: server.Client()}
	if _, err := client.Usage(context.Background(), "today"); err == nil || !strings.Contains(err.Error(), "503") {
		t.Fatalf("Usage() error = %v, want HTTP status 503", err)
	}
	if _, err := client.Quotas(context.Background(), false); err == nil || !strings.Contains(err.Error(), "503") {
		t.Fatalf("Quotas() error = %v, want HTTP status 503", err)
	}
}

func TestWebAuthentication(t *testing.T) {
	const key = "secret key+&/雪"
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet {
			t.Errorf("method = %q, want GET", r.Method)
		}
		switch r.URL.Path {
		case "/":
			if r.URL.Query().Has("k") {
				if want := "k=" + url.QueryEscape(key); r.URL.RawQuery != want {
					t.Errorf("login raw query = %q, want %q", r.URL.RawQuery, want)
				}
				http.SetCookie(w, &http.Cookie{Name: "magpie-web", Value: "authenticated", Path: "/", HttpOnly: true})
				http.Redirect(w, r, "/", http.StatusSeeOther)
				return
			}
			if cookie, err := r.Cookie("magpie-web"); err != nil || cookie.Value != "authenticated" {
				http.Error(w, "missing login cookie", http.StatusUnauthorized)
				return
			}
			w.WriteHeader(http.StatusOK)
		case "/api/sessions/progress":
			if r.URL.Query().Has("k") {
				t.Error("business API request contains the Web Key query parameter")
			}
			if cookie, err := r.Cookie("magpie-web"); err != nil || cookie.Value != "authenticated" {
				http.Error(w, "missing login cookie", http.StatusUnauthorized)
				return
			}
			_, _ = w.Write([]byte(clientResponseJSON(r.URL.Path)))
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	jar, err := cookiejar.New(nil)
	if err != nil {
		t.Fatal(err)
	}
	httpClient := &http.Client{Jar: jar}
	if err := authenticateWeb(context.Background(), httpClient, server.URL, key); err != nil {
		t.Fatal(err)
	}

	client := &Client{baseURL: server.URL, httpClient: httpClient}
	progress, err := client.SessionProgress(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if progress.Files != 8 || progress.Read != 512 {
		t.Fatalf("SessionProgress() after login = %+v", progress)
	}

	cancelledCtx, cancel := context.WithCancel(context.Background())
	cancel()
	err = authenticateWeb(cancelledCtx, httpClient, server.URL, key)
	if err == nil || !errors.Is(err, context.Canceled) {
		t.Fatal("authenticateWeb() did not preserve the context cancellation error")
	}
	if strings.Contains(err.Error(), key) || strings.Contains(err.Error(), url.QueryEscape(key)) {
		t.Fatal("authenticateWeb() exposed the Web Key in its error")
	}
}

func checkQuota(t *testing.T, got QuotasResponse, wantReading bool) {
	t.Helper()
	if got.Reading != wantReading || len(got.Quotas) != 1 {
		t.Fatalf("Quotas() = %+v", got)
	}
	quota := got.Quotas[0]
	if quota.Provider != "openai" || len(quota.Windows) != 1 || quota.Windows[0].Name != "5h" ||
		quota.Resets == nil || quota.Resets.Count != 1 || quota.Resets.Until == nil {
		t.Fatalf("Quotas() quota = %+v", quota)
	}
}

func clientResponseJSON(path string) string {
	switch path {
	case "/api/usage":
		return `{"period":"today","input":42,"output":7,"calls":2,"future_field":"ignored"}`
	case "/api/usage/requests":
		return `{"period":"today","total":1,"rows":[{"t":"2026-10-11T10:00:00Z","agent":"codex","effort":"high","session":"session-1","subagent":"worker","parent_agent":"codex","cost":0.125,"priced":true}]}`
	case "/api/usage/quotas":
		return `[{"provider":"openai","name":"OpenAI","windows":[{"name":"5h","used":0.4,"resetsAt":"2026-10-11T15:00:00Z"}],"resets":{"count":1,"until":"2026-10-11T15:00:00Z"}}]`
	case "/api/sessions", "/api/sessions/one":
		if path == "/api/sessions" {
			return `{"sessions":[{"agent":"codex","id":"session-1","cwd":"/project","models":[{"model":"gpt","cost":0.25,"priced":true,"input":10}],"via":[{"provider":"openai","model":"gpt","effort":"high","calls":1,"tokens":10,"last":"2026-10-11T10:00:00Z"}],"input":10}]}`
		}
		return `{"agent":"codex","id":"session-1","cwd":"/project","models":[{"model":"gpt","cost":0.25,"priced":true,"input":10}],"via":[{"provider":"openai","model":"gpt","effort":"high","calls":1,"tokens":10,"last":"2026-10-11T10:00:00Z"}],"input":10}`
	case "/api/sessions/stats":
		return `{"from":"2026-10-11","to":"2026-10-11","days":[{"date":"2026-10-11","usage":[{"agent":"codex","cwd":"/project","model":"gpt","cost":0.25,"priced":true,"input":10}]}],"agents":{"codex":"Codex"}}`
	case "/api/sessions/overview":
		return `{"count":1,"tools":{"calls":1,"top":[{"name":"shell","calls":1}]},"skills":{"calls":1,"top":[{"name":"review","projects":[{"name":"Usagi"}]}]}}`
	case "/api/sessions/progress":
		return `{"indexing":true,"files":8,"done":3,"bytes":1024,"read":512}`
	default:
		return `{}`
	}
}
