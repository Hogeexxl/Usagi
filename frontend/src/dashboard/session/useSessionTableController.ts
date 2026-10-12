import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { createRevisionFeed, type RevisionFeed } from "../../data/revisionFeed";
import { canonicalDashboardFilters, dashboardQueryKey, threadKeyId, usagiClient, type UsagiClient } from "../../data/usagiClient";
import {
  UsagiClientError,
  type DashboardFilters,
  type DashboardRange,
  type SessionListItemDto,
  type SessionSortField,
  type SessionSortOrder,
} from "../../data/types";
import type { SessionControllerOptions, SessionLoadState, SessionPageState, SessionTableViewModel } from "./sessionTypes";

export const FRONTEND_PAGE_SIZE = 10;

type Snapshot = {
  query_key: string;
  range: DashboardRange;
  filters: DashboardFilters;
  data_revision: number;
  items: SessionListItemDto[];
};

type State = {
  range: DashboardRange;
  filters: DashboardFilters;
  snapshot: Snapshot | null;
  page: number;
  sort_by: SessionSortField;
  sort_order: SessionSortOrder;
  load_state: SessionLoadState;
  page_state: SessionPageState;
  error_code?: string;
};

const DEFAULT_SORT_BY: SessionSortField = "last_activity";
const DEFAULT_SORT_ORDER: SessionSortOrder = "desc";
const SORT_DEFAULTS: Record<SessionSortField, SessionSortOrder> = {
  last_activity: "desc",
  project: "asc",
  model: "asc",
  total_tokens: "desc",
  cost: "desc",
  cache_hit_rate: "desc",
};

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function errorCode(error: unknown): string {
  return error instanceof UsagiClientError ? error.code : "HTTP_ERROR";
}

function currentTimezone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "Asia/Shanghai";
}

function compareNullableText(left: string | null, right: string | null, order: SessionSortOrder): number {
  if (left === null || left === "" || right === null || right === "") {
    if (left === right || !left && !right) return 0;
    return !left ? 1 : -1;
  }
  const comparison = left.localeCompare(right, "en", { numeric: true });
  return order === "asc" ? comparison : -comparison;
}

function compareNullableNumber(left: number | null, right: number | null, order: SessionSortOrder): number {
  if (left === null || right === null) {
    if (left === right) return 0;
    return left === null ? 1 : -1;
  }
  const comparison = left < right ? -1 : left > right ? 1 : 0;
  return order === "asc" ? comparison : -comparison;
}

function compareSessions(left: SessionListItemDto, right: SessionListItemDto, sortBy: SessionSortField, order: SessionSortOrder): number {
  const projectName = (item: SessionListItemDto) => item.thread.project.kind === "project" ? item.thread.project.name : null;
  const modelName = (item: SessionListItemDto) => item.thread.model_effort[0]?.model ?? null;
  let comparison = 0;
  if (sortBy === "last_activity") comparison = compareNullableNumber(left.thread.last_usage_ms, right.thread.last_usage_ms, order);
  if (sortBy === "project") comparison = compareNullableText(projectName(left), projectName(right), order);
  if (sortBy === "model") comparison = compareNullableText(modelName(left), modelName(right), order);
  if (sortBy === "total_tokens") comparison = compareNullableNumber(left.inclusive_usage.total, right.inclusive_usage.total, order);
  if (sortBy === "cost") comparison = compareNullableNumber(left.inclusive_usage.cost, right.inclusive_usage.cost, order);
  if (sortBy === "cache_hit_rate") comparison = compareNullableNumber(left.inclusive_usage.cache_hit_rate, right.inclusive_usage.cache_hit_rate, order);
  return comparison || threadKeyId(left.thread.key).localeCompare(threadKeyId(right.thread.key));
}

function totalPages(totalItems: number): number {
  return Math.ceil(totalItems / FRONTEND_PAGE_SIZE);
}

export function useSessionTableController(
  range: DashboardRange,
  filters: DashboardFilters,
  options: SessionControllerOptions = {},
): SessionTableViewModel {
  const client = options.client ?? usagiClient;
  const canonicalFilters = useMemo(() => canonicalDashboardFilters(filters), [filters]);
  const queryKey = dashboardQueryKey(range, canonicalFilters);
  const ownedFeedRef = useRef<RevisionFeed | null>(null);
  const feedRef = useRef<RevisionFeed | null>(null);
  if (!feedRef.current) {
    feedRef.current = options.revisionFeed ?? createRevisionFeed({ client });
    if (!options.revisionFeed) ownedFeedRef.current = feedRef.current;
  }
  const snapshotsRef = useRef(new Map<string, Snapshot>());
  const requestAbortRef = useRef<AbortController | null>(null);
  const requestGenerationRef = useRef(0);
  const queryKeyRef = useRef<string | null>(null);
  const reloadKeyRef = useRef(options.reloadKey ?? 0);
  const stateRef = useRef<State>({
    range,
    filters: canonicalFilters,
    snapshot: null,
    page: 1,
    sort_by: DEFAULT_SORT_BY,
    sort_order: DEFAULT_SORT_ORDER,
    load_state: "initial",
    page_state: "idle",
  });
  const [state, setState] = useState<State>(stateRef.current);

  const commit = useCallback((next: State | ((current: State) => State)) => {
    const value = typeof next === "function" ? next(stateRef.current) : next;
    stateRef.current = value;
    setState(value);
  }, []);

  const loadSessions = useCallback((targetRange: DashboardRange, targetFilters: DashboardFilters) => {
    const canonical = canonicalDashboardFilters(targetFilters);
    const targetKey = dashboardQueryKey(targetRange, canonical);
    requestAbortRef.current?.abort();
    const controller = new AbortController();
    requestAbortRef.current = controller;
    const generation = ++requestGenerationRef.current;
    // Forced reloads (revision, manual refresh, retry) keep the cached rows on
    // screen until the new snapshot replaces them; epoch resets clear the cache first.
    const previous = snapshotsRef.current.get(targetKey) ?? null;
    if (targetKey === dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) {
      commit((current) => ({
        ...current,
        snapshot: previous,
        load_state: previous ? "refreshing" : "loading",
        page_state: "idle",
        error_code: undefined,
      }));
    }
    void client.getSessions({ range: targetRange, filters: canonical, signal: controller.signal }).then(
      (response) => {
        if (controller.signal.aborted || generation !== requestGenerationRef.current) return;
        if (targetKey !== dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) return;
        const requiredRevision = feedRef.current?.get_snapshot()?.data_revision ?? 0;
        if (response.data_revision < requiredRevision) {
          const cached = snapshotsRef.current.get(targetKey) ?? previous;
          commit((current) => ({
            ...current,
            snapshot: cached,
            load_state: cached ? "ready" : "error",
            page_state: "idle",
            error_code: cached ? undefined : "MAGPIE_UNAVAILABLE",
          }));
          return;
        }
        const snapshot: Snapshot = {
          query_key: targetKey,
          range: targetRange,
          filters: canonical,
          data_revision: response.data_revision,
          items: response.items,
        };
        snapshotsRef.current.set(targetKey, snapshot);
        const pages = totalPages(snapshot.items.length);
        commit((current) => ({
          ...current,
          snapshot,
          page: pages > 0 ? Math.min(current.page, pages) : 1,
          load_state: "ready",
          page_state: "idle",
          error_code: undefined,
        }));
      },
      (error: unknown) => {
        if (controller.signal.aborted || generation !== requestGenerationRef.current || isAbortError(error)) return;
        if (targetKey !== dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) return;
        commit((current) => ({
          ...current,
          load_state: "error",
          page_state: "idle",
          error_code: errorCode(error),
        }));
      },
    );
  }, [client, commit]);

  const goToPage = useCallback((targetPage: number) => {
    const pages = totalPages(stateRef.current.snapshot?.items.length ?? 0);
    if (!Number.isSafeInteger(targetPage) || targetPage < 1 || targetPage > pages || targetPage === stateRef.current.page) return;
    commit((current) => ({ ...current, page: targetPage, page_state: "idle" }));
  }, [commit]);

  const previousPage = useCallback(() => goToPage(stateRef.current.page - 1), [goToPage]);
  const nextPage = useCallback(() => goToPage(stateRef.current.page + 1), [goToPage]);

  const selectSort = useCallback((sortBy: SessionSortField) => {
    if (sortBy === stateRef.current.sort_by) {
      commit((current) => ({ ...current, sort_order: current.sort_order === "asc" ? "desc" : "asc" }));
      return;
    }
    commit((current) => ({ ...current, sort_by: sortBy, sort_order: SORT_DEFAULTS[sortBy] }));
  }, [commit]);

  const retryLoad = useCallback(() => {
    const current = stateRef.current;
    loadSessions(current.range, current.filters);
  }, [loadSessions]);

  useEffect(() => {
    if (queryKeyRef.current === null || queryKeyRef.current !== queryKey) {
      queryKeyRef.current = queryKey;
      const cached = snapshotsRef.current.get(queryKey) ?? null;
      commit((current) => ({
        ...current,
        range,
        filters: canonicalFilters,
        page: 1,
        snapshot: cached,
        load_state: cached ? "ready" : "loading",
        page_state: "idle",
        error_code: undefined,
      }));
      loadSessions(range, canonicalFilters);
    }
  }, [canonicalFilters, commit, loadSessions, queryKey, range]);

  useEffect(() => {
    const reloadKey = options.reloadKey ?? 0;
    if (reloadKeyRef.current === reloadKey) return;
    reloadKeyRef.current = reloadKey;
    const current = stateRef.current;
    loadSessions(current.range, current.filters);
  }, [loadSessions, options.reloadKey]);

  useEffect(() => {
    const unsubscribe = feedRef.current!.subscribe((event) => {
      const current = stateRef.current;
      if (event.kind === "epoch_reset") {
        requestAbortRef.current?.abort();
        requestGenerationRef.current += 1;
        snapshotsRef.current.clear();
        const { range: currentRange, filters: currentFilters } = current;
        commit((value) => ({
          ...value,
          snapshot: null,
          page: 1,
          load_state: "loading",
          page_state: "idle",
          error_code: undefined,
        }));
        loadSessions(currentRange, currentFilters);
        return;
      }
      if (current.snapshot && event.tuple.data_revision <= current.snapshot.data_revision) return;
      loadSessions(current.range, current.filters);
    }, (error: unknown) => {
      if (isAbortError(error)) return;
      commit((current) => ({ ...current, load_state: "error", error_code: "HTTP_ERROR" }));
    });
    return () => {
      unsubscribe();
      requestAbortRef.current?.abort();
      requestGenerationRef.current += 1;
      ownedFeedRef.current?.dispose();
    };
  }, [commit, loadSessions]);

  const snapshot = state.snapshot;
  const pages = totalPages(snapshot?.items.length ?? 0);
  const rows = snapshot
    ? [...snapshot.items]
        .sort((left, right) => compareSessions(left, right, state.sort_by, state.sort_order))
        .slice((state.page - 1) * FRONTEND_PAGE_SIZE, state.page * FRONTEND_PAGE_SIZE)
    : [];

  return {
    range: state.range,
    filters: state.filters,
    rows,
    timezone: currentTimezone(),
    load_state: state.load_state,
    page_state: state.page_state,
    page: state.page,
    data_revision: snapshot?.data_revision,
    total_items: snapshot?.items.length ?? 0,
    total_pages: pages,
    sort_by: state.sort_by,
    sort_order: state.sort_order,
    error_code: state.error_code,
    page_error_code: undefined,
    retry_load: retryLoad,
    go_to_page: goToPage,
    previous_page: previousPage,
    next_page: nextPage,
    select_sort: selectSort,
    retry_page: retryLoad,
  };
}
