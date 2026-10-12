package api

import (
	"cmp"
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"slices"
	"sync"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/projection"
	"github.com/Hogeexxl/Usagi/internal/query"
)

type watcherQuery interface {
	UsageSnapshot(context.Context) (magpie.UsageSummary, error)
	RefreshSessions(context.Context) ([]query.SessionRefreshFact, error)
}

type watcherQuotaService interface {
	Quotas(context.Context, bool) (projection.QuotaView, error)
}

type QuotaSnapshot struct {
	mu     sync.RWMutex
	view   projection.QuotaView
	loaded bool
	err    error
}

func (s *QuotaSnapshot) Current() (projection.QuotaView, bool, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.view, s.loaded, s.err
}

func (s *QuotaSnapshot) Replace(view projection.QuotaView) {
	s.mu.Lock()
	s.view = view
	s.loaded = true
	s.err = nil
	s.mu.Unlock()
}

func (s *QuotaSnapshot) RecordError(err error) {
	s.mu.Lock()
	s.err = err
	s.mu.Unlock()
}

type watchers struct {
	query              watcherQuery
	quotaService       watcherQuotaService
	revisions          RevisionService
	quotaSnapshot      *QuotaSnapshot
	usageInterval      time.Duration
	sessionInterval    time.Duration
	quotaInterval      time.Duration
	usageMu            sync.Mutex
	usageFingerprint   [32]byte
	usageHasBaseline   bool
	sessionMu          sync.Mutex
	sessionFingerprint [32]byte
	sessionHasBaseline bool
}

func newWatchers(queryService watcherQuery, quotaService watcherQuotaService, revisions RevisionService, quotaSnapshot *QuotaSnapshot, usageInterval, sessionInterval, quotaInterval time.Duration) *watchers {
	return &watchers{
		query:           queryService,
		quotaService:    quotaService,
		revisions:       revisions,
		quotaSnapshot:   quotaSnapshot,
		usageInterval:   usageInterval,
		sessionInterval: sessionInterval,
		quotaInterval:   quotaInterval,
	}
}

func (w *watchers) start(ctx context.Context) {
	go w.run(ctx, w.usageInterval, w.refreshUsage)
	go w.run(ctx, w.sessionInterval, w.refreshSessions)
	go w.run(ctx, w.quotaInterval, w.refreshQuota)
}

func (w *watchers) run(ctx context.Context, interval time.Duration, refresh func(context.Context)) {
	refresh(ctx)
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			refresh(ctx)
		}
	}
}

func (w *watchers) refreshUsage(ctx context.Context) {
	summary, err := w.query.UsageSnapshot(ctx)
	if err != nil {
		return
	}
	fingerprint, err := usageFingerprint(summary)
	if err != nil {
		return
	}
	w.usageMu.Lock()
	defer w.usageMu.Unlock()
	if !w.usageHasBaseline {
		w.usageFingerprint = fingerprint
		w.usageHasBaseline = true
		return
	}
	if fingerprint != w.usageFingerprint {
		w.usageFingerprint = fingerprint
		w.revisions.BumpData()
	}
}

func (w *watchers) refreshSessions(ctx context.Context) {
	facts, err := w.query.RefreshSessions(ctx)
	if err != nil {
		return
	}
	fingerprint, err := jsonFingerprint(facts)
	if err != nil {
		return
	}
	w.sessionMu.Lock()
	defer w.sessionMu.Unlock()
	if !w.sessionHasBaseline {
		w.sessionFingerprint = fingerprint
		w.sessionHasBaseline = true
		return
	}
	if fingerprint != w.sessionFingerprint {
		w.sessionFingerprint = fingerprint
		w.revisions.BumpData()
	}
}

func (w *watchers) refreshQuota(ctx context.Context) {
	view, err := w.quotaService.Quotas(ctx, false)
	if err != nil {
		if ctx.Err() == nil {
			w.quotaSnapshot.RecordError(err)
		}
		return
	}
	w.quotaSnapshot.Replace(view)
}

func usageFingerprint(summary magpie.UsageSummary) ([32]byte, error) {
	type totals struct {
		Calls        int
		Errors       int
		Input        int
		Output       int
		CacheRead    int
		CacheWrite   int
		CacheWrite1h int
		Reasoning    int
		Cost         float64
		Unpriced     int
	}
	type group struct {
		ID       string
		Provider string
		Model    string
		Account  string
		Agent    string
		Name     string
		Sub      string
		Icon     string
		totals
	}
	type point struct {
		Label string
		Time  time.Time
		totals
	}
	toTotals := func(value magpie.UsageTotals) totals {
		return totals{
			Calls: value.Calls, Errors: value.Errors, Input: value.Input, Output: value.Output,
			CacheRead: value.CacheRead, CacheWrite: value.CacheWrite, CacheWrite1h: value.CacheWrite1h,
			Reasoning: value.Reasoning, Cost: value.Cost, Unpriced: value.Unpriced,
		}
	}
	toGroup := func(value magpie.UsageGroup) group {
		return group{
			ID: value.ID, Provider: value.Provider, Model: value.Model, Account: value.Account,
			Agent: value.Agent, Name: value.Name, Sub: value.Sub, Icon: value.Icon,
			totals: toTotals(value.UsageTotals),
		}
	}
	type usageFacts struct {
		Totals     totals
		Series     []point
		Agents     []group
		Models     []group
		Accounts   []group
		CallerKeys []group
	}
	facts := usageFacts{Totals: toTotals(summary.UsageTotals)}
	for _, value := range summary.Series {
		facts.Series = append(facts.Series, point{Label: value.Label, Time: value.Time, totals: toTotals(value.UsageTotals)})
	}
	for _, value := range summary.Agents {
		facts.Agents = append(facts.Agents, toGroup(value))
	}
	for _, value := range summary.Models {
		facts.Models = append(facts.Models, toGroup(value))
	}
	for _, value := range summary.Accounts {
		facts.Accounts = append(facts.Accounts, toGroup(value))
	}
	for _, value := range summary.CallerKeys {
		facts.CallerKeys = append(facts.CallerKeys, toGroup(value))
	}
	sortValuesJSON(facts.Series)
	sortValuesJSON(facts.Agents)
	sortValuesJSON(facts.Models)
	sortValuesJSON(facts.Accounts)
	sortValuesJSON(facts.CallerKeys)
	data, err := json.Marshal(facts)
	if err != nil {
		return [32]byte{}, fmt.Errorf("api: fingerprint usage snapshot: %w", err)
	}
	return sha256.Sum256(data), nil
}

func sortValuesJSON[T any](values []T) {
	slices.SortFunc(values, func(a, b T) int {
		left, _ := json.Marshal(a)
		right, _ := json.Marshal(b)
		return cmp.Compare(string(left), string(right))
	})
}

func jsonFingerprint(value any) ([32]byte, error) {
	data, err := json.Marshal(value)
	if err != nil {
		return [32]byte{}, fmt.Errorf("api: fingerprint snapshot: %w", err)
	}
	return sha256.Sum256(data), nil
}
