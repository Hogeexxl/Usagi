import { useCallback, useEffect, useRef, useState } from "react";

import { createRevisionFeed, type RevisionFeed } from "../../data/revisionFeed";
import { canonicalDashboardFilters, dashboardQueryKey, usagiClient, type UsagiClient } from "../../data/usagiClient";
import {
  UsagiClientError,
  type DashboardFilters,
  type DashboardRange,
  type RevisionTuple,
  type SessionItemDto,
  type SessionRowsResponse,
  type SessionSortField,
  type SessionSortIndexItem,
  type SessionSortOrder,
} from "../../data/types";
import type { SessionControllerOptions, SessionLoadState, SessionPageState, SessionTableViewModel } from "./sessionTypes";

export const FRONTEND_PAGE_SIZE = 10;
export const ROW_BATCH_LIMIT = 40;

type Snapshot = {
  query_key: string;
  range: DashboardRange;
  filters: DashboardFilters;
  timezone: string;
  data_revision: number;
  total_items: number;
  sort_index: SessionSortIndexItem[];
  row_cache: Map<string, SessionItemDto>;
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
  page_error_code?: string;
};

type InflightRows = {
  controller: AbortController;
  priority: "foreground" | "prefetch";
  promise: Promise<SessionRowsResponse>;
};

type StaleSnapshotRefresh = {
  revision: number;
  retries: number;
};

const DEFAULT_SORT_BY: SessionSortField = "last_activity";
const DEFAULT_SORT_ORDER: SessionSortOrder = "desc";
const SORT_DEFAULTS: Record<SessionSortField, SessionSortOrder> = {
  last_activity: "desc",
  project: "asc",
  model: "asc",
  total_tokens: "desc",
  combined_total_tokens: "desc",
  combined_estimated_cost: "desc",
  cache_hit_rate: "desc",
};

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function errorCode(error: unknown): string {
  return error instanceof UsagiClientError ? error.code : "HTTP_ERROR";
}

function compareRootIds(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function compareNullableText(left: string | null, right: string | null, order: SessionSortOrder): number {
  const leftValid = left !== null && left.length > 0;
  const rightValid = right !== null && right.length > 0;
  if (!leftValid || !rightValid) {
    if (leftValid === rightValid) return 0;
    return leftValid ? -1 : 1;
  }
  const comparison = left < right ? -1 : left > right ? 1 : 0;
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

function compareIndex(left: SessionSortIndexItem, right: SessionSortIndexItem, sortBy: SessionSortField, order: SessionSortOrder): number {
  let comparison = 0;
  if (sortBy === "project") comparison = compareNullableText(left.project_sort_key, right.project_sort_key, order);
  if (sortBy === "model") comparison = compareNullableText(left.model_sort_key, right.model_sort_key, order);
  if (sortBy === "cache_hit_rate") comparison = compareNullableNumber(left.cache_hit_rate, right.cache_hit_rate, order);
  if (sortBy === "last_activity") comparison = compareNullableNumber(left.last_activity_at_ms, right.last_activity_at_ms, order);
  if (sortBy === "total_tokens") comparison = compareNullableNumber(left.total_tokens, right.total_tokens, order);
  if (sortBy === "combined_total_tokens") comparison = compareNullableNumber(left.combined_total_tokens, right.combined_total_tokens, order);
  if (sortBy === "combined_estimated_cost") comparison = compareNullableNumber(left.combined_estimated_cost, right.combined_estimated_cost, order);
  return comparison || compareRootIds(left.root_session_id, right.root_session_id);
}

function sortedIds(snapshot: Snapshot, sortBy: SessionSortField, sortOrder: SessionSortOrder): string[] {
  return [...snapshot.sort_index].sort((left, right) => compareIndex(left, right, sortBy, sortOrder)).map((item) => item.root_session_id);
}

function totalPages(totalItems: number): number {
  return Math.ceil(totalItems / FRONTEND_PAGE_SIZE);
}

function windowIds(snapshot: Snapshot, page: number, sortBy: SessionSortField, sortOrder: SessionSortOrder): string[] {
  const ids = sortedIds(snapshot, sortBy, sortOrder);
  const firstRank = Math.floor(Math.max(0, page - 1) / 4) * ROW_BATCH_LIMIT;
  return ids.slice(firstRank, firstRank + ROW_BATCH_LIMIT);
}

function makeSnapshot(
  queryKey: string,
  range: DashboardRange,
  filters: DashboardFilters,
  response: Awaited<ReturnType<UsagiClient["getSessionSnapshot"]>>,
  previous: Snapshot | null,
): Snapshot {
  const rowCache = new Map<string, SessionItemDto>();
  if (previous && previous.data_revision === response.data_revision && previous.query_key === queryKey) {
    for (const [id, row] of previous.row_cache) rowCache.set(id, row);
  }
  for (const row of response.items) rowCache.set(row.root_session_id, row);
  return {
    query_key: queryKey,
    range,
    filters,
    timezone: response.range.timezone,
    data_revision: response.data_revision,
    total_items: response.total_items,
    sort_index: response.sort_index,
    row_cache: rowCache,
  };
}

export function useSessionTableController(
  range: DashboardRange,
  filters: DashboardFilters,
  options: SessionControllerOptions = {},
): SessionTableViewModel {
  const client = options.client ?? usagiClient;
  const canonicalFilters = canonicalDashboardFilters(filters);
  const queryKey = dashboardQueryKey(range, canonicalFilters);
  const ownedFeedRef = useRef<RevisionFeed | null>(null);
  const feedRef = useRef<RevisionFeed | null>(null);
  if (!feedRef.current) {
    feedRef.current = options.revisionFeed ?? createRevisionFeed({ client });
    if (!options.revisionFeed) ownedFeedRef.current = feedRef.current;
  }
  const snapshotsRef = useRef(new Map<string, Snapshot>());
  const snapshotAbortRef = useRef<AbortController | null>(null);
  const snapshotGenerationRef = useRef(0);
  const rowsInFlightRef = useRef(new Map<string, InflightRows>());
  const prefetchInFlightRef = useRef<string | null>(null);
  const staleSnapshotRefreshRef = useRef(new Map<string, StaleSnapshotRefresh>());
  const queryKeyRef = useRef<string | null>(null);
  const stateRef = useRef<State>({
    range,
    filters: canonicalFilters,
    snapshot: snapshotsRef.current.get(queryKey) ?? null,
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

  const abortRows = useCallback(() => {
    for (const request of rowsInFlightRef.current.values()) request.controller.abort();
    rowsInFlightRef.current.clear();
    prefetchInFlightRef.current = null;
  }, []);

  const loadSnapshot = useCallback(
    (targetRange: DashboardRange, targetFilters: DashboardFilters, force = false) => {
      const canonical = canonicalDashboardFilters(targetFilters);
      const targetKey = dashboardQueryKey(targetRange, canonical);
      snapshotAbortRef.current?.abort();
      abortRows();
      const controller = new AbortController();
      snapshotAbortRef.current = controller;
      const generation = ++snapshotGenerationRef.current;
      const previous = snapshotsRef.current.get(targetKey) ?? null;
      if (targetKey === dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) {
        commit((current) => ({
          ...current,
          snapshot: previous,
          load_state: previous ? "refreshing" : "loading",
          page_state: "idle",
          error_code: undefined,
          page_error_code: undefined,
        }));
      }
      void client
        .getSessionSnapshot({
          range: targetRange,
          filters: canonical,
          seed_sort_by: stateRef.current.sort_by,
          seed_sort_order: stateRef.current.sort_order,
          signal: controller.signal,
        })
        .then((response) => {
          if (controller.signal.aborted || generation !== snapshotGenerationRef.current) return;
          if (dashboardQueryKey(stateRef.current.range, stateRef.current.filters) !== targetKey) return;
          const feedRevision = feedRef.current?.get_snapshot()?.data_revision ?? 0;
          const requiredRevision = Math.max(previous?.data_revision ?? 0, feedRevision);
          if (response.data_revision < requiredRevision) {
            const stale = staleSnapshotRefreshRef.current.get(targetKey);
            const currentStale = stale && stale.revision === requiredRevision ? stale : { revision: requiredRevision, retries: 0 };
            if (currentStale.retries < 1) {
              staleSnapshotRefreshRef.current.set(targetKey, { ...currentStale, retries: currentStale.retries + 1 });
              loadSnapshot(targetRange, canonical, true);
            } else {
              // A lagging local API must not leave the table in an endless
              // refreshing state. Keep the last complete snapshot visible and
              // wait for the next revision notification before trying again.
              staleSnapshotRefreshRef.current.set(targetKey, currentStale);
              const stableSnapshot = snapshotsRef.current.get(targetKey) ?? previous;
              commit((current) => ({
                ...current,
                snapshot: stableSnapshot,
                load_state: stableSnapshot ? "ready" : "error",
                page_state: "idle",
                error_code: stableSnapshot ? undefined : "UPDATE_FAILED",
              }));
            }
            return;
          }
          staleSnapshotRefreshRef.current.delete(targetKey);
          const snapshot = makeSnapshot(targetKey, targetRange, canonical, response, force ? null : previous);
          snapshotsRef.current.set(targetKey, snapshot);
          const pages = totalPages(snapshot.total_items);
          commit((current) => ({
            ...current,
            snapshot,
            page: pages > 0 ? Math.min(current.page, pages) : 1,
            load_state: "ready",
            page_state: "idle",
            error_code: undefined,
            page_error_code: undefined,
          }));
        })
        .catch((error: unknown) => {
          if (controller.signal.aborted || generation !== snapshotGenerationRef.current || isAbortError(error)) return;
          if (dashboardQueryKey(stateRef.current.range, stateRef.current.filters) !== targetKey) return;
          commit((current) => ({
            ...current,
            load_state: "error",
            page_state: "idle",
            error_code: previous ? "UPDATE_FAILED" : errorCode(error),
          }));
        });
    },
    [abortRows, client, commit, feedRef],
  );

  const loadRows = useCallback(
    (ids: string[], priority: "foreground" | "prefetch") => {
      const current = stateRef.current;
      const snapshot = current.snapshot;
      if (!snapshot || ids.length === 0) return;
      const idsToLoad = [...new Set(ids)].filter((id) => !snapshot.row_cache.has(id));
      if (idsToLoad.length === 0) return;
      const batch = idsToLoad.slice(0, ROW_BATCH_LIMIT);
      const requestKey = `${snapshot.query_key}|${snapshot.data_revision}|${batch.join("\u0001")}`;
      const existing = rowsInFlightRef.current.get(requestKey);
      if (existing) {
        if (priority === "foreground") {
          existing.priority = "foreground";
          commit((value) => ({ ...value, page_state: "loading", page_error_code: undefined }));
        }
        return;
      }
      if (priority === "prefetch" && prefetchInFlightRef.current !== null) return;
      const controller = new AbortController();
      const request = client.getSessionRows({
        range: snapshot.range,
        filters: snapshot.filters,
        root_session_ids: batch,
        expected_data_revision: snapshot.data_revision,
        signal: controller.signal,
      });
      const entry: InflightRows = { controller, priority, promise: request };
      rowsInFlightRef.current.set(requestKey, entry);
      if (priority === "prefetch") prefetchInFlightRef.current = requestKey;
      if (priority === "foreground") commit((value) => ({ ...value, page_state: "loading", page_error_code: undefined }));
      void request
        .then((response) => {
          const latest = stateRef.current;
          const latestSnapshot = latest.snapshot;
          const effectivePriority = entry.priority;
          if (
            controller.signal.aborted ||
            !latestSnapshot ||
            latestSnapshot.query_key !== snapshot.query_key ||
            latestSnapshot.data_revision !== snapshot.data_revision ||
            response.data_revision !== snapshot.data_revision
          ) return;
          const rowCache = new Map(latestSnapshot.row_cache);
          for (const row of response.items) {
            if (batch.includes(row.root_session_id)) rowCache.set(row.root_session_id, row);
          }
          const nextSnapshot = { ...latestSnapshot, row_cache: rowCache };
          snapshotsRef.current.set(snapshot.query_key, nextSnapshot);
          commit((value) => ({
            ...value,
            snapshot: nextSnapshot,
            page_state: effectivePriority === "foreground" ? "idle" : value.page_state,
            page_error_code: effectivePriority === "foreground" ? undefined : value.page_error_code,
          }));
        })
        .catch((error: unknown) => {
          if (controller.signal.aborted || isAbortError(error)) return;
          const latest = stateRef.current;
          if (!latest.snapshot || latest.snapshot.query_key !== snapshot.query_key || latest.snapshot.data_revision !== snapshot.data_revision) return;
          if (errorCode(error) === "STALE_DATA_REVISION") {
            loadSnapshot(latest.range, latest.filters, true);
            return;
          }
          if (entry.priority === "foreground") commit((value) => ({ ...value, page_state: "error", page_error_code: errorCode(error) }));
        })
        .finally(() => {
          rowsInFlightRef.current.delete(requestKey);
          if (prefetchInFlightRef.current === requestKey) prefetchInFlightRef.current = null;
        });
    },
    [client, commit, loadSnapshot],
  );

  const ensureCurrentWindow = useCallback(() => {
    const current = stateRef.current;
    const snapshot = current.snapshot;
    if (!snapshot || current.load_state === "loading" || current.load_state === "error") return;
    const ids = windowIds(snapshot, current.page, current.sort_by, current.sort_order);
    const missing = ids.filter((id) => !snapshot.row_cache.has(id));
    if (missing.length > 0) {
      loadRows(missing, "foreground");
      return;
    }
    if ((current.page - 1) % 4 === 2) {
      const sorted = sortedIds(snapshot, current.sort_by, current.sort_order);
      const nextWindowStart = (Math.floor((current.page - 1) / 4) + 1) * ROW_BATCH_LIMIT;
      const nextWindow = sorted.slice(nextWindowStart, nextWindowStart + ROW_BATCH_LIMIT);
      const missingNext = nextWindow.filter((id) => !snapshot.row_cache.has(id));
      if (missingNext.length > 0) loadRows(missingNext, "prefetch");
    }
  }, [loadRows]);

  const goToPage = useCallback((targetPage: number) => {
    const pages = totalPages(stateRef.current.snapshot?.total_items ?? 0);
    if (!Number.isSafeInteger(targetPage) || targetPage < 1 || targetPage > pages || targetPage === stateRef.current.page) return;
    commit((current) => ({ ...current, page: targetPage, page_state: "idle", page_error_code: undefined }));
  }, [commit]);

  const previousPage = useCallback(() => goToPage(stateRef.current.page - 1), [goToPage]);
  const nextPage = useCallback(() => goToPage(stateRef.current.page + 1), [goToPage]);

  const selectSort = useCallback((sortBy: SessionSortField) => {
    if (sortBy === stateRef.current.sort_by) {
      commit((current) => ({ ...current, sort_order: current.sort_order === "asc" ? "desc" : "asc" }));
      return;
    }
    commit((current) => ({ ...current, sort_by: sortBy, sort_order: SORT_DEFAULTS[sortBy], page_error_code: undefined }));
  }, [commit]);

  const retryPage = useCallback(() => {
    const current = stateRef.current;
    if (current.page_state !== "error") return;
    ensureCurrentWindow();
  }, [ensureCurrentWindow]);

  const retryLoad = useCallback(() => {
    const current = stateRef.current;
    if (current.error_code === "REVISION_FAILED") feedRef.current?.retry_now();
    loadSnapshot(current.range, current.filters, true);
  }, [loadSnapshot]);

  useEffect(() => {
    if (queryKeyRef.current === null) {
      queryKeyRef.current = queryKey;
      if (stateRef.current.range !== range || dashboardQueryKey(stateRef.current.range, stateRef.current.filters) !== queryKey) {
        commit((current) => ({ ...current, range, filters: canonicalFilters, page: 1, snapshot: snapshotsRef.current.get(queryKey) ?? null }));
      }
      loadSnapshot(range, canonicalFilters);
      return;
    }
    if (queryKeyRef.current === queryKey) {
      if (!stateRef.current.snapshot && snapshotAbortRef.current?.signal.aborted) loadSnapshot(range, canonicalFilters);
      return;
    }
    queryKeyRef.current = queryKey;
    commit((current) => ({
      ...current,
      range,
      filters: canonicalFilters,
      page: 1,
      snapshot: snapshotsRef.current.get(queryKey) ?? null,
      load_state: snapshotsRef.current.has(queryKey) ? "ready" : "loading",
      page_state: "idle",
      error_code: undefined,
      page_error_code: undefined,
    }));
    loadSnapshot(range, canonicalFilters);
  }, [canonicalFilters, commit, loadSnapshot, queryKey, range]);

  useEffect(() => {
    ensureCurrentWindow();
  }, [ensureCurrentWindow, state.page, state.sort_by, state.sort_order, state.snapshot, state.load_state]);

  useEffect(() => {
    const unsubscribe = feedRef.current!.subscribe(
      (tuple: RevisionTuple) => {
        const current = stateRef.current;
        if (!current.snapshot || tuple.data_revision <= current.snapshot.data_revision) return;
        loadSnapshot(current.range, current.filters, true);
      },
      (error: unknown) => {
        if (isAbortError(error)) return;
        commit((current) => ({ ...current, load_state: "error", error_code: "REVISION_FAILED" }));
      },
    );
    return () => {
      unsubscribe();
      snapshotAbortRef.current?.abort();
      abortRows();
      if (ownedFeedRef.current) ownedFeedRef.current.dispose();
    };
  }, [abortRows, commit, loadSnapshot]);

  const snapshot = state.snapshot;
  const pages = totalPages(snapshot?.total_items ?? 0);
  const currentRows = snapshot
    ? sortedIds(snapshot, state.sort_by, state.sort_order)
        .slice((state.page - 1) * FRONTEND_PAGE_SIZE, state.page * FRONTEND_PAGE_SIZE)
        .map((id) => snapshot.row_cache.get(id))
        .filter((row): row is SessionItemDto => row !== undefined)
    : [];
  return {
    range: state.range,
    filters: state.filters,
    rows: currentRows,
    timezone: snapshot?.timezone ?? "Asia/Shanghai",
    load_state: state.load_state,
    page_state: state.page_state,
    page: state.page,
    data_revision: snapshot?.data_revision,
    total_items: snapshot?.total_items ?? 0,
    total_pages: pages,
    sort_by: state.sort_by,
    sort_order: state.sort_order,
    error_code: state.error_code,
    page_error_code: state.page_error_code,
    retry_load: retryLoad,
    go_to_page: goToPage,
    previous_page: previousPage,
    next_page: nextPage,
    select_sort: selectSort,
    retry_page: retryPage,
  };
}
