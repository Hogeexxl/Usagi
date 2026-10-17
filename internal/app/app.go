package app

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"time"

	"github.com/Hogeexxl/Usagi/internal/api"
	"github.com/Hogeexxl/Usagi/internal/database"
	"github.com/Hogeexxl/Usagi/internal/platform"
	"github.com/Hogeexxl/Usagi/internal/projection"
	"github.com/Hogeexxl/Usagi/internal/query"
)

const magpieRetryInterval = 5 * time.Second

type BrowserOpener interface {
	Open(string) error
}

type dataRootResolver func() (string, error)

type Dependencies struct {
	RuntimeFactory RuntimeFactory
	Browser        BrowserOpener
	StaticFS       fs.FS

	dataRootResolver dataRootResolver
}

func Run(ctx context.Context, dependencies Dependencies) error {
	return run(ctx, dependencies, RuntimePortRange())
}

func run(ctx context.Context, dependencies Dependencies, ports PortRange) (runErr error) {
	if dependencies.RuntimeFactory == nil || dependencies.Browser == nil || dependencies.StaticFS == nil {
		return errors.New("app: runtime factory, browser, and static filesystem must be provided")
	}
	var err error
	dataRoot := ""
	if dependencies.dataRootResolver != nil {
		dataRoot, err = dependencies.dataRootResolver()
	} else {
		dataRoot, err = platform.UsagiDataDir()
	}
	if err != nil {
		return fmt.Errorf("app: resolve data root: %w", err)
	}
	descriptorPath := runtimeDescriptorPath(dataRoot)

	bound, err := BindLoopback(ports)
	if err != nil {
		return fmt.Errorf("app: bind runtime port: %w", err)
	}
	if bound.Existing != nil {
		openDashboard(dependencies.Browser, bound.Existing.String()+"/")
		return nil
	}
	listener := bound.Listener
	defer listener.Close()

	baseURL, port, err := listenerURL(listener)
	if err != nil {
		return err
	}
	_, location, err := api.LocalTimezone()
	if err != nil {
		return fmt.Errorf("app: resolve local time zone: %w", err)
	}
	if err := os.MkdirAll(dataRoot, 0700); err != nil {
		return fmt.Errorf("app: create database directory: %w", err)
	}
	if err := os.Chmod(dataRoot, 0700); err != nil {
		return fmt.Errorf("app: set database directory permissions: %w", err)
	}
	db, err := database.Open(filepath.Join(dataRoot, "usagi.sqlite"))
	if err != nil {
		return fmt.Errorf("app: open database: %w", err)
	}
	defer func() {
		if err := db.Close(); err != nil && runErr == nil {
			runErr = fmt.Errorf("app: close database: %w", err)
		}
	}()
	var dataRevision int64
	if err := db.QueryRowContext(ctx, "SELECT CAST(value AS INTEGER) FROM app_meta WHERE key = 'data_revision'").Scan(&dataRevision); err != nil {
		return fmt.Errorf("app: read database revision: %w", err)
	}

	runCtx, cancelRun := context.WithCancel(ctx)
	supervisorCtx, cancelSupervisor := context.WithCancel(context.Background())
	defer cancelRun()
	defer cancelSupervisor()

	supervisor := NewMagpieSupervisor(dependencies.RuntimeFactory, magpieRetryInterval)
	queryService := query.NewRuntimeService(db, supervisor, location)
	projectionService := projection.NewService(queryService)
	revisions := api.NewRevisionService(dataRevision)
	quotaSnapshot := &api.QuotaSnapshot{}

	config := api.DefaultConfig()
	config.ListenAddress = listener.Addr().String()
	config.RuntimeMode = runtimeMode()
	config.StaticFS = dependencies.StaticFS
	server, err := api.NewServer(config, api.Dependencies{
		Projection: projectionService, Query: queryService, Revisions: revisions,
		RuntimeStatus: supervisor, QuotaSnapshot: quotaSnapshot, ShutdownRequester: api.ShutdownRequester(cancelRun),
	})
	if err != nil {
		_ = supervisor.Close()
		return fmt.Errorf("app: create API server: %w", err)
	}

	serveDone := make(chan struct{})
	var serveErr error
	go func() {
		serveErr = server.Serve(runCtx, listener)
		close(serveDone)
	}()
	go func() {
		supervisor.Run(supervisorCtx)
	}()

	descriptorWritten := false
	defer func() {
		cancelRun()
		<-serveDone
		cancelSupervisor()
		if err := supervisor.Close(); err != nil && runErr == nil {
			runErr = fmt.Errorf("app: close Magpie supervisor: %w", err)
		}
		if descriptorWritten {
			if err := removeRuntimeDescriptor(descriptorPath, os.Getpid(), runtimeMode()); err != nil {
				slog.Error("remove runtime descriptor", "error", err)
			}
		}
	}()

	if err := waitForHealth(runCtx, baseURL, serveDone); err != nil {
		if runCtx.Err() != nil {
			return nil
		}
		if serveErr != nil {
			return fmt.Errorf("app: HTTP server stopped before health became ready: %w", serveErr)
		}
		return fmt.Errorf("app: HTTP server stopped before health became ready: %w", err)
	}

	if err := writeRuntimeDescriptor(descriptorPath, RuntimeDescriptor{
		SchemaVersion: 1,
		PID:           os.Getpid(),
		Port:          int(port),
		BaseURL:       baseURL,
		RuntimeMode:   runtimeMode(),
	}); err != nil {
		return fmt.Errorf("app: write runtime descriptor: %w", err)
	}
	descriptorWritten = true

	openDashboard(dependencies.Browser, baseURL+"/")

	select {
	case <-runCtx.Done():
	case <-serveDone:
		if runCtx.Err() != nil {
			return nil
		}
		if serveErr != nil {
			return fmt.Errorf("app: HTTP server stopped unexpectedly: %w", serveErr)
		}
		return errors.New("app: HTTP server stopped unexpectedly")
	}

	return nil
}

func listenerURL(listener net.Listener) (string, uint16, error) {
	host, portText, err := net.SplitHostPort(listener.Addr().String())
	if err != nil {
		return "", 0, fmt.Errorf("app: read runtime listener address: %w", err)
	}
	port, err := strconv.ParseUint(portText, 10, 16)
	if err != nil {
		return "", 0, fmt.Errorf("app: read runtime listener port: %w", err)
	}
	address := net.JoinHostPort(host, strconv.FormatUint(port, 10))
	baseURL := (&url.URL{Scheme: "http", Host: address}).String()
	return baseURL, uint16(port), nil
}

func waitForHealth(ctx context.Context, baseURL string, serveDone <-chan struct{}) error {
	client := &http.Client{
		Transport: &http.Transport{Proxy: nil},
		Timeout:   300 * time.Millisecond,
	}
	defer client.CloseIdleConnections()
	ticker := time.NewTicker(25 * time.Millisecond)
	defer ticker.Stop()

	for {
		select {
		case <-serveDone:
			return errors.New("HTTP server stopped")
		default:
		}

		request, err := http.NewRequestWithContext(ctx, http.MethodGet, baseURL+"/api/health", nil)
		if err != nil {
			return err
		}
		response, err := client.Do(request)
		if err == nil {
			_ = response.Body.Close()
			marker := response.Header.Values("X-Usagi-App")
			if response.StatusCode >= http.StatusOK && response.StatusCode < http.StatusMultipleChoices && len(marker) == 1 && marker[0] == "Usagi" {
				return nil
			}
		}

		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-serveDone:
			return errors.New("HTTP server stopped")
		case <-ticker.C:
		}
	}
}

func openDashboard(browser BrowserOpener, target string) {
	if err := browser.Open(target); err != nil {
		slog.Error("open dashboard", "url", target, "error", err)
	}
}
