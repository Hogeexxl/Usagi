package magpie

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strconv"
	"time"
)

type Client struct {
	baseURL    string
	httpClient *http.Client
	// gateway reports where the Magpie desktop app's gateway listens. It is
	// optional: without it Quotas only returns what the web sidecar knows.
	gateway GatewayLocator
}

// GatewayLocator tells the client which Magpie gateway to ask for quotas the
// web sidecar cannot read itself.
type GatewayLocator interface {
	GatewayURL() string
}

// WithGateway returns a client that also reads the Claude subscription quota
// from the Magpie gateway. The web sidecar never signs in to Claude, so only
// the desktop app's gateway can answer for it.
func (c *Client) WithGateway(gateway GatewayLocator) *Client {
	clone := *c
	clone.gateway = gateway
	return &clone
}

// NewClient constructs a client for a Magpie Web API endpoint.
func NewClient(baseURL string, httpClient *http.Client) *Client {
	return &Client{baseURL: baseURL, httpClient: httpClient}
}

// ErrDayDigestsUnavailable reports a Magpie that does not return a per-day
// usage series for the requested window.
var ErrDayDigestsUnavailable = errors.New("magpie usage series is not bucketed by day")

type HTTPError struct {
	StatusCode int
}

func (e *HTTPError) Error() string {
	return fmt.Sprintf("magpie web request returned HTTP status %d", e.StatusCode)
}

type WebError struct {
	Err error
}

func (e *WebError) Error() string {
	return e.Err.Error()
}

func (e *WebError) Unwrap() error {
	return e.Err
}

func wrapWebError(err error) error {
	return &WebError{Err: err}
}

func (c *Client) Usage(ctx context.Context, period string) (UsageSummary, error) {
	query := url.Values{}
	if period != "" {
		query.Set("period", period)
	}

	var usage UsageSummary
	if err := c.getJSON(ctx, "/api/usage", query, &usage); err != nil {
		return UsageSummary{}, err
	}
	return usage, nil
}

func (c *Client) UsageRequests(ctx context.Context, q UsageRequestsQuery) (UsageRequestsPage, error) {
	query := url.Values{}
	if q.Period != "" {
		query.Set("period", q.Period)
	}
	if q.Day != "" {
		query.Set("day", q.Day)
	}
	if q.Model != "" {
		query.Set("model", q.Model)
	}
	if q.Agent != "" {
		query.Set("agent", q.Agent)
	}
	if q.Provider != "" {
		query.Set("provider", q.Provider)
	}
	if q.Purpose != "" {
		query.Set("purpose", q.Purpose)
	}
	if q.Account != "" {
		query.Set("account", q.Account)
	}
	if q.Failed {
		query.Set("failed", "1")
	}
	if q.Search != "" {
		query.Set("q", q.Search)
	}
	if q.Via != "" {
		query.Set("via", q.Via)
	}
	if q.Offset != 0 {
		query.Set("offset", strconv.Itoa(q.Offset))
	}
	if q.Limit != 0 {
		query.Set("limit", strconv.Itoa(q.Limit))
	}

	var page UsageRequestsPage
	if err := c.getJSON(ctx, "/api/usage/requests", query, &page); err != nil {
		return UsageRequestsPage{}, err
	}
	return page, nil
}

// UsageDayDigests reads Magpie's per-day chart series for an inclusive local
// date window and fingerprints each day. Magpie only buckets by day for windows
// up to 60 days, so callers must chunk longer spans.
func (c *Client) UsageDayDigests(ctx context.Context, from, to string) ([]UsageDayDigest, error) {
	query := url.Values{"period": {from + ".." + to}, "limit": {"1"}}
	var page struct {
		Bucket string            `json:"bucket"`
		Series []json.RawMessage `json:"series"`
	}
	if err := c.getJSON(ctx, "/api/usage/requests", query, &page); err != nil {
		return nil, err
	}
	if page.Bucket != "day" {
		return nil, ErrDayDigestsUnavailable
	}
	digests := make([]UsageDayDigest, 0, len(page.Series))
	for _, raw := range page.Series {
		digest, err := usageDayDigest(raw)
		if err != nil {
			return nil, wrapWebError(err)
		}
		digests = append(digests, digest)
	}
	return digests, nil
}

func (c *Client) Providers(ctx context.Context) (ProviderCatalogsResponse, error) {
	var providers ProviderCatalogsResponse
	if err := c.getJSON(ctx, "/api/providers", nil, &providers); err != nil {
		return ProviderCatalogsResponse{}, err
	}
	return providers, nil
}

func usageDayDigest(raw json.RawMessage) (UsageDayDigest, error) {
	var point struct {
		Time  time.Time `json:"time"`
		Calls int       `json:"calls"`
	}
	if err := json.Unmarshal(raw, &point); err != nil {
		return UsageDayDigest{}, err
	}
	// Re-encode through a generic value so key order and whitespace cannot
	// change the digest; json.Number keeps numbers byte-exact.
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	var value any
	if err := decoder.Decode(&value); err != nil {
		return UsageDayDigest{}, err
	}
	canonical, err := json.Marshal(value)
	if err != nil {
		return UsageDayDigest{}, err
	}
	return UsageDayDigest{
		Day:    point.Time.Format(time.DateOnly),
		Time:   point.Time,
		Calls:  point.Calls,
		Digest: sha256.Sum256(canonical),
	}, nil
}

func (c *Client) Quotas(ctx context.Context, asked bool) (QuotasResponse, error) {
	query := url.Values{}
	if asked {
		query.Set("asked", "1")
	}

	requestURL, err := url.Parse(c.baseURL)
	if err != nil {
		return QuotasResponse{}, err
	}
	requestURL.Path = "/api/usage/quotas"
	requestURL.RawPath = ""
	requestURL.RawQuery = query.Encode()

	request, err := http.NewRequestWithContext(ctx, http.MethodGet, requestURL.String(), nil)
	if err != nil {
		return QuotasResponse{}, err
	}
	response, err := c.httpClient.Do(request)
	if err != nil {
		return QuotasResponse{}, wrapWebError(err)
	}
	defer response.Body.Close()

	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices {
		return QuotasResponse{}, wrapWebError(&HTTPError{StatusCode: response.StatusCode})
	}

	var quotas []Quota
	if err := json.NewDecoder(response.Body).Decode(&quotas); err != nil {
		return QuotasResponse{}, wrapWebError(err)
	}
	if !hasQuotaProvider(quotas, claudeQuotaProvider) {
		quotas = append(quotas, c.gatewayQuotas(ctx, claudeQuotaProvider)...)
	}
	return QuotasResponse{
		Quotas:  quotas,
		Reading: response.Header.Get("X-Magpie-Reading") == "1",
	}, nil
}

const (
	claudeQuotaProvider = "claude"
	gatewayQuotasPath   = "/v1/magpie/quotas"
	gatewayQuotasWait   = 3 * time.Second
)

func hasQuotaProvider(quotas []Quota, provider string) bool {
	for _, quota := range quotas {
		if quota.Provider == provider {
			return true
		}
	}
	return false
}

// gatewayQuotas returns the gateway's quota entries for one provider. It is
// best effort: an absent desktop app or a gateway error just means no entry.
func (c *Client) gatewayQuotas(ctx context.Context, provider string) []Quota {
	if c.gateway == nil {
		return nil
	}
	base := c.gateway.GatewayURL()
	if base == "" {
		return nil
	}
	requestURL, err := url.Parse(base)
	if err != nil {
		return nil
	}
	requestURL.Path = gatewayQuotasPath
	requestURL.RawPath = ""

	ctx, cancel := context.WithTimeout(ctx, gatewayQuotasWait)
	defer cancel()
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, requestURL.String(), nil)
	if err != nil {
		return nil
	}
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		return nil
	}
	defer response.Body.Close()
	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices {
		return nil
	}

	var body struct {
		Data []Quota `json:"data"`
	}
	if err := json.NewDecoder(response.Body).Decode(&body); err != nil {
		return nil
	}
	var matched []Quota
	for _, quota := range body.Data {
		if quota.Provider == provider {
			matched = append(matched, quota)
		}
	}
	return matched
}

func (c *Client) Sessions(ctx context.Context, limit int) (SessionsResponse, error) {
	query := url.Values{"limit": {strconv.Itoa(limit)}}

	var sessions SessionsResponse
	if err := c.getJSON(ctx, "/api/sessions", query, &sessions); err != nil {
		return SessionsResponse{}, err
	}
	return sessions, nil
}

func (c *Client) Session(ctx context.Context, key string) (Session, error) {
	query := url.Values{"key": {key}}
	var session Session
	if err := c.getJSON(ctx, "/api/sessions/one", query, &session); err != nil {
		return Session{}, err
	}
	return session, nil
}

func (c *Client) SessionStats(ctx context.Context, days int) (SessionStats, error) {
	query := url.Values{}
	if days > 0 {
		query.Set("days", strconv.Itoa(days))
	}

	var stats SessionStats
	if err := c.getJSON(ctx, "/api/sessions/stats", query, &stats); err != nil {
		return SessionStats{}, err
	}
	return stats, nil
}

func (c *Client) SessionOverview(ctx context.Context, q SessionOverviewQuery) (SessionOverview, error) {
	query := url.Values{}
	if q.Days != 0 {
		query.Set("days", strconv.Itoa(q.Days))
	}
	if q.Agent != "" {
		query.Set("agent", q.Agent)
	}
	if q.Model != "" {
		query.Set("model", q.Model)
	}
	if q.Cwd != "" {
		query.Set("cwd", q.Cwd)
	}

	var overview SessionOverview
	if err := c.getJSON(ctx, "/api/sessions/overview", query, &overview); err != nil {
		return SessionOverview{}, err
	}
	return overview, nil
}

func (c *Client) SessionProgress(ctx context.Context) (SessionProgress, error) {
	var progress SessionProgress
	if err := c.getJSON(ctx, "/api/sessions/progress", nil, &progress); err != nil {
		return SessionProgress{}, err
	}
	return progress, nil
}

func (c *Client) getJSON(ctx context.Context, path string, query url.Values, out any) error {
	requestURL, err := url.Parse(c.baseURL)
	if err != nil {
		return err
	}
	requestURL.Path = path
	requestURL.RawPath = ""
	requestURL.RawQuery = query.Encode()

	request, err := http.NewRequestWithContext(ctx, http.MethodGet, requestURL.String(), nil)
	if err != nil {
		return err
	}
	response, err := c.httpClient.Do(request)
	if err != nil {
		return wrapWebError(err)
	}
	defer response.Body.Close()

	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices {
		return wrapWebError(&HTTPError{StatusCode: response.StatusCode})
	}
	if err := json.NewDecoder(response.Body).Decode(out); err != nil {
		return wrapWebError(err)
	}
	return nil
}

func authenticateWeb(ctx context.Context, httpClient *http.Client, baseURL, key string) error {
	loginURL, err := url.Parse(baseURL)
	if err != nil {
		return err
	}
	loginURL.Path = "/"
	loginURL.RawPath = ""
	loginURL.RawQuery = "k=" + url.QueryEscape(key)

	request, err := http.NewRequestWithContext(ctx, http.MethodGet, loginURL.String(), nil)
	if err != nil {
		return err
	}
	response, err := httpClient.Do(request)
	if err != nil {
		var urlErr *url.Error
		if errors.As(err, &urlErr) {
			return urlErr.Err
		}
		return err
	}
	defer response.Body.Close()

	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices {
		return fmt.Errorf("magpie web authentication returned HTTP status %d", response.StatusCode)
	}
	return nil
}
