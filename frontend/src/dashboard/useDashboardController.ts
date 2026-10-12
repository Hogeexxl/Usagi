import { useCallback, useEffect, useRef, useState } from "react";

import {
  canonicalDashboardFilters,
  dashboardQueryKey,
  dashboardRangesEqual,
  usagiClient,
  type UsagiClient,
} from "../data/usagiClient";
import { createRevisionFeed, type RevisionEventSource, type RevisionFeed, type RevisionFeedEvent } from "../data/revisionFeed";
import {
  type DashboardRange,
  type RevisionTuple,
  type RuntimeStatusResponse,
  type SummaryResponse,
  type DashboardFilters,
  type FilterOptionsResponse,
  UsagiClientError,
} from "../data/types";

export type LoadState = "initial" | "loading" | "ready" | "error";

export type DashboardViewModel = {
  range: DashboardRange;
  filters: DashboardFilters;
  metrics: SummaryResponse["usage"] | null;
  session_count: number;
  data_revision: number;
  reload_key: number;
  epoch_reset_key: number;
  filter_options: FilterOptionsResponse | null;
  filter_options_loading: boolean;
  filter_options_error_code?: string;
  agentFilterActive: boolean;
  modelFilterActive: boolean;
  projectFilterActive: boolean;
  anyFilterActive: boolean;
  load_state: LoadState;
  refreshing: boolean;
  error_code?: string;
  select_range: (range: DashboardRange) => void;
  select_filters: (filters: DashboardFilters) => void;
  clear_filters: () => void;
  retry_filter_options: () => void;
  retry_load: () => void;
  request_refresh: () => void;
};

type InternalState = {
  range: DashboardRange;
  filters: DashboardFilters;
  snapshot: SummaryResponse | null;
  load_state: LoadState;
  summary_loading: boolean;
  refreshing: boolean;
  reload_key: number;
  epoch_reset_key: number;
  error_code?: string;
  status: RuntimeStatusResponse | null;
  filter_options: FilterOptionsResponse | null;
  filter_options_loading: boolean;
  filter_options_error_code?: string;
};

export type DashboardControllerOptions = {
  client?: UsagiClient;
  eventSourceFactory?: (url: string) => RevisionEventSource;
  pollIntervalMs?: number;
  revisionFeed?: RevisionFeed;
};

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function clientErrorCode(error: unknown): string {
  return error instanceof UsagiClientError ? error.code : "HTTP_ERROR";
}

const EMPTY_FILTERS: DashboardFilters = { agents: [], models: [], projects: [] };

export function useDashboardController(options: DashboardControllerOptions = {}): DashboardViewModel {
  const client = options.client ?? usagiClient;
  const ownedRevisionFeedRef = useRef<RevisionFeed | null>(null);
  const revisionFeedRef = useRef<RevisionFeed | null>(null);
  if (!revisionFeedRef.current) {
    revisionFeedRef.current = options.revisionFeed ?? createRevisionFeed({
      client,
      eventSourceFactory: options.eventSourceFactory,
      pollIntervalMs: options.pollIntervalMs,
    });
    if (!options.revisionFeed) ownedRevisionFeedRef.current = revisionFeedRef.current;
  }

  const [state, setState] = useState<InternalState>({
    range: { key: "today" },
    filters: { agents: [], models: [], projects: [] },
    snapshot: null,
    load_state: "initial",
    summary_loading: true,
    refreshing: false,
    reload_key: 0,
    epoch_reset_key: 0,
    status: null,
    filter_options: null,
    filter_options_loading: true,
  });
  const stateRef = useRef(state);
  const snapshotsRef = useRef(new Map<string, SummaryResponse>());
  const summaryAbortRef = useRef<AbortController | null>(null);
  const summaryGenerationRef = useRef(0);
  const statusAbortRef = useRef<AbortController | null>(null);
  const statusGenerationRef = useRef(0);
  const filterOptionsAbortRef = useRef<AbortController | null>(null);
  const filterOptionsGenerationRef = useRef(0);
  // Options last loaded per range, so switching back shows them at once.
  const filterOptionsCacheRef = useRef(new Map<string, FilterOptionsResponse>());
  const acceptedTupleRef = useRef<RevisionTuple>({ data_revision: 0, status_revision: 0 });

  const commit = useCallback((next: InternalState | ((current: InternalState) => InternalState)) => {
    const value = typeof next === "function" ? next(stateRef.current) : next;
    stateRef.current = value;
    setState(value);
  }, []);

  const loadSummary = useCallback((range: DashboardRange, filters: DashboardFilters): Promise<void> => {
    const canonicalFilters = canonicalDashboardFilters(filters);
    const key = dashboardQueryKey(range, canonicalFilters);
    summaryAbortRef.current?.abort();
    const controller = new AbortController();
    summaryAbortRef.current = controller;
    const generation = ++summaryGenerationRef.current;
    if (key === dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) {
      commit((current) => ({
        ...current,
        summary_loading: true,
        load_state: current.snapshot ? "loading" : current.load_state === "initial" ? "initial" : "loading",
        error_code: undefined,
      }));
    }
    return client.summary(range, canonicalFilters, controller.signal).then(
      (response) => {
        if (controller.signal.aborted || generation !== summaryGenerationRef.current) return;
        if (key !== dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) return;
        if (response.data_revision < acceptedTupleRef.current.data_revision) {
          commit((current) => ({ ...current, summary_loading: false }));
          return;
        }
        snapshotsRef.current.set(key, response);
        acceptedTupleRef.current = { ...acceptedTupleRef.current, data_revision: response.data_revision };
        commit((current) => ({
          ...current,
          snapshot: response,
          summary_loading: false,
          load_state: "ready",
          error_code: undefined,
        }));
      },
      (error: unknown) => {
        if (controller.signal.aborted || generation !== summaryGenerationRef.current || isAbortError(error)) return;
        if (key !== dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) return;
        commit((current) => ({
          ...current,
          summary_loading: false,
          load_state: "error",
          error_code: clientErrorCode(error),
        }));
      },
    );
  }, [client, commit]);

  // Loads the options of the current range in the background. Options already
  // on screen stay until the new ones arrive, so menus never flash or nag.
  const loadFilterOptions = useCallback((): Promise<void> => {
    const range = stateRef.current.range;
    const key = dashboardQueryKey(range, EMPTY_FILTERS);
    filterOptionsAbortRef.current?.abort();
    const controller = new AbortController();
    filterOptionsAbortRef.current = controller;
    const generation = ++filterOptionsGenerationRef.current;
    const cached = filterOptionsCacheRef.current.get(key) ?? null;
    commit((current) => ({
      ...current,
      filter_options: cached,
      filter_options_loading: cached === null,
      filter_options_error_code: undefined,
    }));
    return client.filterOptions(range, controller.signal).then(
      (response) => {
        if (controller.signal.aborted || generation !== filterOptionsGenerationRef.current) return;
        const previous = filterOptionsCacheRef.current.get(key);
        // An older snapshot than the one shown is dropped; the revision that
        // made it old triggers another load.
        if (previous && response.data_revision < previous.data_revision) {
          commit((current) => ({ ...current, filter_options_loading: false }));
          return;
        }
        filterOptionsCacheRef.current.set(key, response);
        commit((current) => ({
          ...current,
          filter_options: response,
          filter_options_loading: false,
          filter_options_error_code: undefined,
        }));
      },
      (error: unknown) => {
        if (controller.signal.aborted || generation !== filterOptionsGenerationRef.current || isAbortError(error)) return;
        commit((current) => ({
          ...current,
          filter_options_loading: false,
          // Keep showing usable options; only report a failure with nothing to show.
          filter_options_error_code: current.filter_options ? undefined : clientErrorCode(error),
        }));
      },
    );
  }, [client, commit]);

  const loadStatus = useCallback((): Promise<void> => {
    statusAbortRef.current?.abort();
    const controller = new AbortController();
    statusAbortRef.current = controller;
    const generation = ++statusGenerationRef.current;
    return client.getStatus(controller.signal).then(
      (status) => {
        if (controller.signal.aborted || generation !== statusGenerationRef.current) return;
        const previousStatusRevision = acceptedTupleRef.current.status_revision;
        if (status.status_revision >= previousStatusRevision) {
          acceptedTupleRef.current = { ...acceptedTupleRef.current, status_revision: status.status_revision };
        }
        const wasReady = stateRef.current.status?.runtime_state === "ready";
        commit((current) => ({ ...current, status }));
        if (!wasReady && status.runtime_state === "ready") {
          if (!stateRef.current.snapshot) void loadSummary(stateRef.current.range, stateRef.current.filters);
          // Only a failed load is retried; one still in flight is left to finish.
          if (stateRef.current.filter_options_error_code) void loadFilterOptions();
        }
      },
      (error: unknown) => {
        if (controller.signal.aborted || generation !== statusGenerationRef.current || isAbortError(error)) return;
        commit((current) => ({
          ...current,
          error_code: clientErrorCode(error),
          load_state: current.snapshot ? current.load_state : "error",
        }));
      },
    );
  }, [client, commit, loadFilterOptions, loadSummary]);

  const handleRevision = useCallback((event: RevisionFeedEvent) => {
    const tuple = event.tuple;
    if (event.kind === "epoch_reset") {
      acceptedTupleRef.current = tuple;
      snapshotsRef.current.clear();
      summaryAbortRef.current?.abort();
      filterOptionsAbortRef.current?.abort();
      filterOptionsCacheRef.current.clear();
      const { range, filters } = stateRef.current;
      commit((current) => ({
        ...current,
        snapshot: null,
        status: null,
        epoch_reset_key: current.epoch_reset_key + 1,
        summary_loading: true,
        load_state: "loading",
        error_code: undefined,
        filter_options: null,
        filter_options_loading: true,
        filter_options_error_code: undefined,
      }));
      void loadSummary(range, filters);
      void loadFilterOptions();
      void loadStatus();
      return;
    }

    const previous = acceptedTupleRef.current;
    if (tuple.data_revision < previous.data_revision || tuple.status_revision < previous.status_revision) return;
    const dataAdvanced = tuple.data_revision > previous.data_revision;
    const statusAdvanced = tuple.status_revision > previous.status_revision;
    if (!dataAdvanced && !statusAdvanced) return;
    acceptedTupleRef.current = tuple;
    if (dataAdvanced) {
      const { range, filters } = stateRef.current;
      void loadSummary(range, filters);
      void loadFilterOptions();
    }
    if (statusAdvanced) void loadStatus();
  }, [commit, loadFilterOptions, loadStatus, loadSummary]);

  const selectRange = useCallback((range: DashboardRange) => {
    if (dashboardRangesEqual(range, stateRef.current.range)) return;
    const filters = stateRef.current.filters;
    const cached = snapshotsRef.current.get(dashboardQueryKey(range, filters)) ?? null;
    const snapshot = cached && cached.data_revision >= acceptedTupleRef.current.data_revision ? cached : null;
    commit((current) => ({
      ...current,
      range,
      snapshot,
      summary_loading: true,
      load_state: snapshot ? "loading" : "loading",
      error_code: undefined,
    }));
    void loadSummary(range, filters);
    void loadFilterOptions();
  }, [commit, loadFilterOptions, loadSummary]);

  const selectFilters = useCallback((filters: DashboardFilters) => {
    const canonical = canonicalDashboardFilters(filters);
    const range = stateRef.current.range;
    const key = dashboardQueryKey(range, canonical);
    if (key === dashboardQueryKey(range, stateRef.current.filters)) {
      commit((current) => ({ ...current, filters: canonical }));
      return;
    }
    const cached = snapshotsRef.current.get(key) ?? null;
    const snapshot = cached && cached.data_revision >= acceptedTupleRef.current.data_revision ? cached : null;
    commit((current) => ({
      ...current,
      filters: canonical,
      snapshot,
      summary_loading: true,
      load_state: "loading",
      error_code: undefined,
    }));
    void loadSummary(range, canonical);
  }, [commit, loadSummary]);

  const clearFilters = useCallback(() => {
    selectFilters({ agents: [], models: [], projects: [] });
  }, [selectFilters]);

  const retryFilterOptions = useCallback(() => {
    void loadFilterOptions();
  }, [loadFilterOptions]);

  const requestRefresh = useCallback(() => {
    const { range, filters } = stateRef.current;
    commit((current) => ({
      ...current,
      refreshing: true,
      reload_key: current.reload_key + 1,
    }));
    void Promise.allSettled([loadSummary(range, filters), loadFilterOptions()]).finally(() => {
      commit((current) => ({ ...current, refreshing: false }));
    });
  }, [commit, loadFilterOptions, loadSummary]);

  const retryLoad = useCallback(() => {
    const current = stateRef.current;
    if (current.load_state === "error" || !current.snapshot) void loadSummary(current.range, current.filters);
    if (!current.status) void loadStatus();
    if (current.filter_options_error_code) void loadFilterOptions();
    revisionFeedRef.current?.retry_now();
  }, [loadFilterOptions, loadStatus, loadSummary]);

  useEffect(() => {
    const { range, filters } = stateRef.current;
    void loadSummary(range, filters);
    void loadFilterOptions();
    void loadStatus();
    const unsubscribe = revisionFeedRef.current?.subscribe(handleRevision);
    return () => {
      summaryAbortRef.current?.abort();
      statusAbortRef.current?.abort();
      filterOptionsAbortRef.current?.abort();
      unsubscribe?.();
      ownedRevisionFeedRef.current?.dispose();
    };
    // This controller is intentionally mounted once for the page lifetime.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const current = state.snapshot;
  const agentFilterActive = state.filters.agents.length > 0;
  const modelFilterActive = state.filters.models.length > 0;
  const projectFilterActive = state.filters.projects.length > 0;
  return {
    range: state.range,
    filters: state.filters,
    metrics: current?.usage ?? null,
    session_count: current?.session_count ?? 0,
    data_revision: acceptedTupleRef.current.data_revision,
    reload_key: state.reload_key,
    epoch_reset_key: state.epoch_reset_key,
    filter_options: state.filter_options,
    filter_options_loading: state.filter_options_loading,
    filter_options_error_code: state.filter_options_error_code,
    agentFilterActive,
    modelFilterActive,
    projectFilterActive,
    anyFilterActive: agentFilterActive || modelFilterActive || projectFilterActive,
    load_state: state.load_state,
    refreshing: state.refreshing,
    error_code: state.error_code,
    select_range: selectRange,
    select_filters: selectFilters,
    clear_filters: clearFilters,
    retry_filter_options: retryFilterOptions,
    retry_load: retryLoad,
    request_refresh: requestRefresh,
  };
}
