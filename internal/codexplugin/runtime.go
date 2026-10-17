package codexplugin

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

var (
	ErrNotRunning        = errors.New("Usagi is not running")
	ErrInvalidDescriptor = errors.New("invalid Usagi runtime descriptor")
	ErrInvalidIdentity   = errors.New("invalid Usagi runtime identity")
)

type RuntimeTarget struct {
	PID         int
	Port        int
	BaseURL     string
	RuntimeMode string
}

type RuntimeResolver interface {
	Resolve(context.Context) (RuntimeTarget, error)
}

type runtimeDescriptor struct {
	SchemaVersion int    `json:"schema_version"`
	PID           int    `json:"pid"`
	Port          int    `json:"port"`
	BaseURL       string `json:"base_url"`
	RuntimeMode   string `json:"runtime_mode"`
}

type runtimeResolver struct {
	descriptorPath string
	client         *http.Client
}

func NewProductionRuntimeResolver() (RuntimeResolver, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, fmt.Errorf("resolve user home directory: %w", err)
	}
	return &runtimeResolver{
		descriptorPath: filepath.Join(home, "Library", "Application Support", "Usagi", "runtime.json"),
		client:         NewHTTPClient(),
	}, nil
}

func NewHTTPClient() *http.Client {
	return &http.Client{
		Transport: &http.Transport{Proxy: nil},
		Timeout:   5 * time.Second,
		CheckRedirect: func(request *http.Request, via []*http.Request) error {
			if len(via) > 0 && !sameOrigin(via[len(via)-1].URL, request.URL) {
				return http.ErrUseLastResponse
			}
			return nil
		},
	}
}

func sameOrigin(left, right *url.URL) bool {
	return strings.EqualFold(left.Scheme, right.Scheme) && strings.EqualFold(left.Host, right.Host)
}

func (r *runtimeResolver) Resolve(ctx context.Context) (RuntimeTarget, error) {
	contents, err := os.ReadFile(r.descriptorPath)
	if errors.Is(err, os.ErrNotExist) {
		return RuntimeTarget{}, ErrNotRunning
	}
	if err != nil {
		return RuntimeTarget{}, fmt.Errorf("%w: read descriptor: %v", ErrNotRunning, err)
	}
	var descriptor runtimeDescriptor
	if err := json.Unmarshal(contents, &descriptor); err != nil {
		return RuntimeTarget{}, fmt.Errorf("%w: decode JSON: %v", ErrInvalidDescriptor, err)
	}
	target, err := validateRuntimeDescriptor(descriptor)
	if err != nil {
		return RuntimeTarget{}, err
	}
	if err := r.validateIdentity(ctx, target); err != nil {
		return RuntimeTarget{}, err
	}
	return target, nil
}

func validateRuntimeDescriptor(descriptor runtimeDescriptor) (RuntimeTarget, error) {
	if descriptor.SchemaVersion != 1 || descriptor.RuntimeMode != "production" || descriptor.PID <= 0 || descriptor.Port < 1 || descriptor.Port > 65535 {
		return RuntimeTarget{}, ErrInvalidDescriptor
	}
	baseURL, err := url.Parse(descriptor.BaseURL)
	if err != nil || baseURL.Scheme != "http" || baseURL.Host == "" || baseURL.User != nil || baseURL.RawQuery != "" || baseURL.Fragment != "" || (baseURL.Path != "" && baseURL.Path != "/") {
		return RuntimeTarget{}, ErrInvalidDescriptor
	}
	host := baseURL.Hostname()
	ip := net.ParseIP(host)
	if !(ip != nil && ip.IsLoopback()) && !strings.EqualFold(host, "localhost") {
		return RuntimeTarget{}, ErrInvalidDescriptor
	}
	port, err := strconv.Atoi(baseURL.Port())
	if err != nil || port < 1 || port > 65535 || port != descriptor.Port {
		return RuntimeTarget{}, ErrInvalidDescriptor
	}
	return RuntimeTarget{
		PID: descriptor.PID, Port: descriptor.Port, BaseURL: baseURL.String(), RuntimeMode: descriptor.RuntimeMode,
	}, nil
}

func (r *runtimeResolver) validateIdentity(ctx context.Context, target RuntimeTarget) error {
	healthRequest, err := http.NewRequestWithContext(ctx, http.MethodGet, runtimeEndpoint(target, "/api/health"), nil)
	if err != nil {
		return fmt.Errorf("%w: create health request: %v", ErrInvalidIdentity, err)
	}
	healthRequest.Header.Set("Accept", "application/json")
	healthResponse, err := r.client.Do(healthRequest)
	if err != nil {
		return fmt.Errorf("%w: health request: %v", ErrInvalidIdentity, err)
	}
	healthMarkers := healthResponse.Header.Values("X-Usagi-App")
	healthOK := healthResponse.StatusCode >= http.StatusOK && healthResponse.StatusCode < http.StatusMultipleChoices && len(healthMarkers) == 1 && healthMarkers[0] == "Usagi"
	_ = healthResponse.Body.Close()
	if !healthOK {
		return ErrInvalidIdentity
	}

	infoRequest, err := http.NewRequestWithContext(ctx, http.MethodGet, runtimeEndpoint(target, "/api/v1/info"), nil)
	if err != nil {
		return fmt.Errorf("%w: create info request: %v", ErrInvalidIdentity, err)
	}
	infoRequest.Header.Set("Accept", "application/json")
	infoResponse, err := r.client.Do(infoRequest)
	if err != nil {
		return fmt.Errorf("%w: info request: %v", ErrInvalidIdentity, err)
	}
	defer infoResponse.Body.Close()
	if infoResponse.StatusCode < http.StatusOK || infoResponse.StatusCode >= http.StatusMultipleChoices {
		return ErrInvalidIdentity
	}
	body, err := io.ReadAll(io.LimitReader(infoResponse.Body, 4097))
	if err != nil || len(body) > 4096 {
		return ErrInvalidIdentity
	}
	var info struct {
		RuntimeMode string `json:"runtime_mode"`
	}
	if err := json.Unmarshal(body, &info); err != nil || info.RuntimeMode != "production" {
		return ErrInvalidIdentity
	}
	return nil
}

func runtimeEndpoint(target RuntimeTarget, path string) string {
	baseURL, _ := url.Parse(target.BaseURL)
	return baseURL.ResolveReference(&url.URL{Path: path}).String()
}
