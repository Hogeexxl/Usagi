package api

import (
	"context"
	"errors"
	"io/fs"
	"net"
	"net/http"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/projection"
	"github.com/Hogeexxl/Usagi/internal/query"
)

type Config struct {
	ListenAddress          string
	AppVersion             string
	StaticDir              string
	StaticFS               fs.FS
	UsageRefreshInterval   time.Duration
	SessionRefreshInterval time.Duration
	QuotaRefreshInterval   time.Duration
}

func DefaultConfig() Config {
	return Config{
		ListenAddress:          "127.0.0.1:37991",
		AppVersion:             "0.2.6",
		StaticDir:              "frontend/dist",
		UsageRefreshInterval:   30 * time.Second,
		SessionRefreshInterval: 30 * time.Second,
		QuotaRefreshInterval:   60 * time.Second,
	}
}

type ProjectionService interface {
	Summary(context.Context, projection.ScopeQuery) (projection.SummaryView, error)
	ModelDistribution(context.Context, projection.ScopeQuery) (projection.ModelDistributionView, error)
	ProjectDistribution(context.Context, projection.ScopeQuery) (projection.ProjectDistributionView, error)
	FilterOptions(context.Context) (projection.FilterOptionsView, error)
	Sessions(context.Context, projection.ScopeQuery) (projection.SessionListView, error)
	SessionDetail(context.Context, projection.ScopeQuery, projection.ThreadKey) (projection.SessionDetailView, error)
	Quotas(context.Context, bool) (projection.QuotaView, error)
}

type RevisionSource interface {
	Current() RevisionTuple
	BumpData() RevisionTuple
	BumpStatus() RevisionTuple
	Subscribe() (<-chan RevisionTuple, func())
}

type ShutdownRequester func()

type WatcherQuery interface {
	UsageSnapshot(context.Context) (magpie.UsageSummary, error)
	RefreshSessions(context.Context) ([]query.SessionRefreshFact, error)
}

type Dependencies struct {
	Projection        ProjectionService
	Query             WatcherQuery
	Revisions         RevisionSource
	RuntimeStatus     RuntimeStatusSource
	QuotaSnapshot     *QuotaSnapshot
	ShutdownRequester ShutdownRequester
}

type Server struct {
	config            Config
	projection        ProjectionService
	query             WatcherQuery
	revisions         RevisionSource
	runtime           RuntimeStatusSource
	quotas            *QuotaSnapshot
	static            http.Handler
	startOnce         sync.Once
	shutdownOnce      sync.Once
	shutdownRequester ShutdownRequester
}

func NewServer(config Config, dependencies Dependencies) (*Server, error) {
	config = completeConfig(config)
	if err := validateListenAddress(config.ListenAddress); err != nil {
		return nil, err
	}
	if err := validateRefreshIntervals(config); err != nil {
		return nil, err
	}
	if dependencies.Projection == nil || dependencies.Query == nil || dependencies.Revisions == nil || dependencies.RuntimeStatus == nil || dependencies.QuotaSnapshot == nil || dependencies.ShutdownRequester == nil {
		return nil, errors.New("api: server dependencies must be provided")
	}
	staticFS := config.StaticFS
	if staticFS == nil {
		staticFS = os.DirFS(config.StaticDir)
	}
	return &Server{
		config: config, projection: dependencies.Projection, query: dependencies.Query, revisions: dependencies.Revisions,
		runtime: dependencies.RuntimeStatus, quotas: dependencies.QuotaSnapshot,
		static: newStaticHandler(staticFS), shutdownRequester: dependencies.ShutdownRequester,
	}, nil
}

func completeConfig(config Config) Config {
	defaults := DefaultConfig()
	if config.ListenAddress == "" {
		config.ListenAddress = defaults.ListenAddress
	}
	if config.AppVersion == "" {
		config.AppVersion = defaults.AppVersion
	}
	if config.StaticDir == "" && config.StaticFS == nil {
		config.StaticDir = defaults.StaticDir
	}
	if config.UsageRefreshInterval == 0 {
		config.UsageRefreshInterval = defaults.UsageRefreshInterval
	}
	if config.SessionRefreshInterval == 0 {
		config.SessionRefreshInterval = defaults.SessionRefreshInterval
	}
	if config.QuotaRefreshInterval == 0 {
		config.QuotaRefreshInterval = defaults.QuotaRefreshInterval
	}
	return config
}

func validateListenAddress(address string) error {
	host, port, err := net.SplitHostPort(address)
	if err != nil {
		return errors.New("api: listen address must include a loopback host and port")
	}
	if !strings.EqualFold(host, "localhost") {
		ip := net.ParseIP(host)
		if ip == nil || !ip.IsLoopback() {
			return errors.New("api: listen address must use a loopback host")
		}
	}
	portNumber, err := strconv.Atoi(port)
	if err != nil || portNumber < 0 || portNumber > 65535 {
		return errors.New("api: listen address must use a valid port")
	}
	return nil
}

func validateRefreshIntervals(config Config) error {
	if config.UsageRefreshInterval <= 0 || config.SessionRefreshInterval <= 0 || config.QuotaRefreshInterval <= 0 {
		return errors.New("api: refresh intervals must be positive")
	}
	return nil
}

func (s *Server) Start(ctx context.Context) {
	s.startOnce.Do(func() {
		startRuntimeStatusSubscription(ctx, s.runtime, s.revisions)
		newWatchers(s.query, s.projection, s.revisions, s.quotas, s.config.UsageRefreshInterval, s.config.SessionRefreshInterval, s.config.QuotaRefreshInterval).start(ctx, s.runtime)
	})
}

func (s *Server) ListenAddress() string {
	return s.config.ListenAddress
}

func (s *Server) HTTPServer() *http.Server {
	return &http.Server{Addr: s.config.ListenAddress, Handler: s}
}

func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("X-Usagi-App", "Usagi")
	w.Header().Set("X-Usagi-Version", s.config.AppVersion)
	apiPath := isAPIPath(r.URL.Path)
	if apiPath {
		w.Header().Set("Cache-Control", "no-store")
	}
	if !allowRequest(w, r) {
		return
	}
	if apiPath {
		if s.servePublicV1Route(w, r) {
			return
		}
		s.serveInternalAPI(w, r)
		return
	}
	s.static.ServeHTTP(w, r)
}

func (s *Server) servePublicV1Route(w http.ResponseWriter, r *http.Request) bool {
	switch r.URL.Path {
	case publicV1InfoPath:
		if r.Method == http.MethodGet {
			s.handlePublicV1Info(w, r)
		} else {
			writeAPIError(w, http.StatusNotFound, ErrorNotFound)
		}
	case publicV1RevisionPath:
		if r.Method == http.MethodGet {
			s.handlePublicV1Revision(w, r)
		} else {
			writeAPIError(w, http.StatusNotFound, ErrorNotFound)
		}
	case publicV1EventsPath:
		if r.Method == http.MethodGet {
			s.handlePublicV1Events(w, r)
		} else {
			writeAPIError(w, http.StatusNotFound, ErrorNotFound)
		}
	case publicV1StatusPath:
		if r.Method == http.MethodGet {
			s.handlePublicV1Status(w, r)
		} else {
			writeAPIError(w, http.StatusNotFound, ErrorNotFound)
		}
	case publicV1QuotaPath:
		if r.Method == http.MethodGet {
			s.handlePublicV1Quota(w, r)
		} else {
			writeAPIError(w, http.StatusNotFound, ErrorNotFound)
		}
	case publicV1SummaryPath:
		if r.Method == http.MethodGet {
			s.handlePublicV1Summary(w, r)
		} else {
			writeAPIError(w, http.StatusNotFound, ErrorNotFound)
		}
	default:
		return false
	}
	return true
}

func isAPIPath(path string) bool {
	return path == "/api" || strings.HasPrefix(path, "/api/")
}

func (s *Server) serveInternalAPI(w http.ResponseWriter, r *http.Request) {
	if r.Method == http.MethodGet && r.URL.Path == "/api/health" {
		if !requestHasNoQuery(w, r) {
			return
		}
		w.WriteHeader(http.StatusNoContent)
		return
	}
	switch r.URL.Path {
	case "/api/revision":
		if r.Method == http.MethodGet {
			s.handleRevision(w, r)
			return
		}
	case "/api/events":
		if r.Method == http.MethodGet {
			s.handleEvents(w, r)
			return
		}
	case "/api/status":
		if r.Method == http.MethodGet {
			s.handleStatus(w, r)
			return
		}
	case "/api/usage/summary":
		if r.Method == http.MethodGet {
			s.handleSummary(w, r)
			return
		}
	case "/api/usage/model-distribution":
		if r.Method == http.MethodGet {
			s.handleModelDistribution(w, r)
			return
		}
	case "/api/usage/projects":
		if r.Method == http.MethodGet {
			s.handleProjectDistribution(w, r)
			return
		}
	case "/api/usage/filter-options":
		if r.Method == http.MethodGet {
			s.handleFilterOptions(w, r)
			return
		}
	case "/api/usage/sessions":
		if r.Method == http.MethodGet {
			s.handleSessions(w, r)
			return
		}
	case "/api/usage/session-detail":
		if r.Method == http.MethodGet {
			s.handleSessionDetail(w, r)
			return
		}
	case "/api/quota":
		if r.Method == http.MethodGet {
			s.handleQuota(w, r)
			return
		}
	case "/api/quota/refresh":
		if r.Method == http.MethodPost {
			s.handleQuotaRefresh(w, r)
			return
		}
	case "/api/service":
		if r.Method == http.MethodGet {
			s.handleService(w, r)
			return
		}
	case "/api/service/stop":
		if r.Method == http.MethodPost {
			s.handleServiceStop(w, r)
			return
		}
	}
	writeAPIError(w, http.StatusNotFound, ErrorNotFound)
}

func (s *Server) handleRevision(w http.ResponseWriter, r *http.Request) {
	if !requestHasNoQuery(w, r) {
		return
	}
	writeJSON(w, http.StatusOK, s.revisions.Current())
}

func (s *Server) handleStatus(w http.ResponseWriter, r *http.Request) {
	if !requestHasNoQuery(w, r) {
		return
	}
	state := s.runtime.Status()
	var lastErrorCode *string
	if state == magpie.RuntimeStatusUnavailable {
		code := ErrorMagpieUnavailable
		lastErrorCode = &code
	}
	writeJSON(w, http.StatusOK, statusResponseDTO{
		StatusRevision: s.revisions.Current().StatusRevision,
		RuntimeState:   string(state),
		LastErrorCode:  lastErrorCode,
	})
}

func (s *Server) requireRuntimeReady(w http.ResponseWriter) bool {
	if s.runtime.Status() == magpie.RuntimeStatusReady {
		return true
	}
	writeAPIError(w, http.StatusServiceUnavailable, ErrorMagpieUnavailable)
	return false
}

func (s *Server) handleQuota(w http.ResponseWriter, r *http.Request) {
	if !requestHasNoQuery(w, r) {
		return
	}
	view, loaded, _ := s.quotas.Current()
	if !loaded {
		view = projection.QuotaView{Reading: true, Items: []projection.QuotaProviderView{}}
	}
	writeJSON(w, http.StatusOK, mapQuotaDTO(view))
}

func (s *Server) handleQuotaRefresh(w http.ResponseWriter, r *http.Request) {
	if !requestHasNoQuery(w, r) {
		return
	}
	if !s.requireRuntimeReady(w) {
		return
	}
	view, err := s.projection.Quotas(r.Context(), true)
	if err != nil {
		s.quotas.RecordError(err)
		writeProjectionError(w, err)
		return
	}
	s.quotas.Replace(view)
	writeJSON(w, http.StatusOK, mapQuotaDTO(view))
}

func (s *Server) ListenAndServe() error {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	s.Start(ctx)
	return s.HTTPServer().ListenAndServe()
}

func (s *Server) Serve(ctx context.Context, listener net.Listener) error {
	s.Start(ctx)
	httpServer := &http.Server{Addr: s.config.ListenAddress, Handler: s}
	serveResult := make(chan error, 1)
	go func() {
		serveResult <- httpServer.Serve(listener)
	}()

	select {
	case err := <-serveResult:
		return normalizeServeError(err)
	case <-ctx.Done():
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		shutdownErr := httpServer.Shutdown(shutdownCtx)
		cancel()
		if errors.Is(shutdownErr, context.DeadlineExceeded) {
			_ = httpServer.Close()
		}
		return normalizeServeError(<-serveResult)
	}
}

func normalizeServeError(err error) error {
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}
