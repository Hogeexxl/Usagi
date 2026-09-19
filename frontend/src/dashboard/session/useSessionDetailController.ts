import { useCallback, useEffect, useRef, useState } from "react";

import { createRevisionFeed, type RevisionFeed } from "../../data/revisionFeed";
import { canonicalDashboardFilters, dashboardQueryKey, type UsagiClient, usagiClient } from "../../data/usagiClient";
import {
  UsagiClientError,
  type DashboardFilters,
  type DashboardRange,
  type RevisionTuple,
  type SessionDetailResponse,
  type SessionItemDto,
} from "../../data/types";

export type SessionDetailLoadState = "closed" | "loading" | "ready" | "refreshing" | "error";

export type SessionDetailControllerOptions = {
  client?: UsagiClient;
  revisionFeed?: RevisionFeed;
  /** The revision backing the currently visible Session snapshot. */
  dataRevision?: number;
  /** Refresh the Session snapshot after a detail request reports stale data. */
  onStaleRevision?: () => void;
};

export type SessionDetailControllerViewModel = {
  open: boolean;
  selected_root_session_id: string | null;
  selected_row: SessionItemDto | null;
  detail: SessionDetailResponse | null;
  data_revision?: number;
  load_state: SessionDetailLoadState;
  error_code?: string;
  refresh_error_code?: string;
  open_detail: (row: SessionItemDto) => void;
  select_session: (row: SessionItemDto) => void;
  close_detail: () => void;
  retry_detail: () => void;
};

type DetailCacheEntry = {
  key: string;
  revision: number;
  value: SessionDetailResponse;
};

type DetailState = {
  open: boolean;
  selected_root_session_id: string | null;
  selected_row: SessionItemDto | null;
  detail: SessionDetailResponse | null;
  data_revision?: number;
  load_state: SessionDetailLoadState;
  error_code?: string;
  refresh_error_code?: string;
};

type Scope = {
  range: DashboardRange;
  filters: DashboardFilters;
  revision: number;
};

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function errorCode(error: unknown): string {
  return error instanceof UsagiClientError ? error.code : "HTTP_ERROR";
}

function detailCacheKey(scope: Scope, rootSessionId: string, revision = scope.revision): string {
  return JSON.stringify([dashboardQueryKey(scope.range, scope.filters), rootSessionId, revision]);
}

function revisionFromFeed(feed: RevisionFeed | undefined): number {
  return feed?.get_snapshot()?.data_revision ?? 0;
}

export function useSessionDetailController(
  range: DashboardRange,
  filters: DashboardFilters,
  options: SessionDetailControllerOptions = {},
): SessionDetailControllerViewModel {
  const client = options.client ?? usagiClient;
  const canonicalFilters = canonicalDashboardFilters(filters);
  const feedRef = useRef<RevisionFeed | null>(null);
  const ownedFeedRef = useRef<RevisionFeed | null>(null);
  if (!feedRef.current) {
    feedRef.current = options.revisionFeed ?? createRevisionFeed({ client });
    if (!options.revisionFeed) ownedFeedRef.current = feedRef.current;
  }

  const initialRevision = options.dataRevision ?? revisionFromFeed(feedRef.current);
  const scopeRef = useRef<Scope>({ range, filters: canonicalFilters, revision: initialRevision });
  const suppliedRevision = options.dataRevision ?? revisionFromFeed(feedRef.current);
  scopeRef.current = { range, filters: canonicalFilters, revision: Math.max(scopeRef.current.revision, suppliedRevision) };
  const stateRef = useRef<DetailState>({
    open: false,
    selected_root_session_id: null,
    selected_row: null,
    detail: null,
    data_revision: initialRevision || undefined,
    load_state: "closed",
  });
  const [state, setState] = useState<DetailState>(stateRef.current);
  const mountedRef = useRef(true);
  const requestGenerationRef = useRef(0);
  const requestAbortRef = useRef<AbortController | null>(null);
  const cacheRef = useRef(new Map<string, DetailCacheEntry>());
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const staleCallbackRef = useRef(options.onStaleRevision);
  staleCallbackRef.current = options.onStaleRevision;

  const commit = useCallback((next: DetailState | ((current: DetailState) => DetailState)) => {
    if (!mountedRef.current) return;
    const value = typeof next === "function" ? next(stateRef.current) : next;
    stateRef.current = value;
    setState(value);
  }, []);

  const restoreFocus = useCallback(() => {
    const previous = previousFocusRef.current;
    previousFocusRef.current = null;
    if (previous && document.contains(previous)) {
      previous.focus();
      return;
    }
    document.querySelector<HTMLElement>(".session-table-scroll")?.focus();
  }, []);

  const loadDetail = useCallback(
    (row: SessionItemDto, requestedRevision: number, bypassCache: boolean) => {
      const scope = scopeRef.current;
      const rootSessionId = row.root_session_id;
      const cacheKey = detailCacheKey(scope, rootSessionId, requestedRevision);
      const cached = !bypassCache ? cacheRef.current.get(cacheKey) : undefined;
      requestAbortRef.current?.abort();
      const generation = ++requestGenerationRef.current;

      commit((current) => ({
        ...current,
        open: true,
        selected_root_session_id: rootSessionId,
        selected_row: row,
        data_revision: requestedRevision || current.data_revision,
        detail: cached?.value ?? (current.selected_root_session_id === rootSessionId ? current.detail : null),
        load_state: cached ? "ready" : current.detail && current.selected_root_session_id === rootSessionId ? "refreshing" : "loading",
        error_code: undefined,
        refresh_error_code: undefined,
      }));
      if (cached) return;

      const controller = new AbortController();
      requestAbortRef.current = controller;
      const expectedRevision = requestedRevision > 0 ? requestedRevision : undefined;
      void client
        .getSessionDetail({
          range: scope.range,
          filters: scope.filters,
          root_session_id: rootSessionId,
          expected_data_revision: expectedRevision,
          signal: controller.signal,
        })
        .then((response) => {
          if (
            controller.signal.aborted ||
            generation !== requestGenerationRef.current ||
            !mountedRef.current ||
            !stateRef.current.open ||
            stateRef.current.selected_root_session_id !== rootSessionId
          ) {
            return;
          }
          const currentRevision = scopeRef.current.revision;
          if (currentRevision > 0 && response.data_revision !== currentRevision) {
            if (response.data_revision < currentRevision) {
              loadDetail(row, currentRevision, true);
            }
            return;
          }
          const responseKey = detailCacheKey(scopeRef.current, rootSessionId, response.data_revision);
          cacheRef.current.set(responseKey, { key: responseKey, revision: response.data_revision, value: response });
          for (const [key, entry] of cacheRef.current) {
            if (entry.revision < response.data_revision) cacheRef.current.delete(key);
          }
          commit((current) => ({
            ...current,
            detail: response,
            data_revision: response.data_revision,
            load_state: "ready",
            error_code: undefined,
            refresh_error_code: undefined,
          }));
        })
        .catch((error: unknown) => {
          if (controller.signal.aborted || generation !== requestGenerationRef.current || !mountedRef.current) return;
          const code = errorCode(error);
          if (code === "STALE_DATA_REVISION") {
            staleCallbackRef.current?.();
            const currentRevision = scopeRef.current.revision;
            if (currentRevision > requestedRevision) {
              loadDetail(row, currentRevision, true);
              return;
            }
          }
          commit((current) => ({
            ...current,
            load_state: current.detail ? "ready" : "error",
            error_code: current.detail ? undefined : code,
            refresh_error_code: current.detail ? code : undefined,
          }));
        })
        .finally(() => {
          if (requestAbortRef.current === controller) requestAbortRef.current = null;
        });
    },
    [client, commit],
  );

  const openDetail = useCallback(
    (row: SessionItemDto) => {
      if (!stateRef.current.open) {
        const active = document.activeElement;
        previousFocusRef.current = active instanceof HTMLElement ? active : null;
      }
      const requestedRevision = scopeRef.current.revision;
      loadDetail(row, requestedRevision, false);
    },
    [loadDetail],
  );

  const closeDetail = useCallback(() => {
    requestAbortRef.current?.abort();
    requestAbortRef.current = null;
    requestGenerationRef.current += 1;
    commit({
      open: false,
      selected_root_session_id: null,
      selected_row: null,
      detail: null,
      data_revision: scopeRef.current.revision || undefined,
      load_state: "closed",
    });
    restoreFocus();
  }, [commit, restoreFocus]);

  const refreshDetail = useCallback(() => {
    const current = stateRef.current;
    const row = current.selected_row;
    if (!current.open || !row) return;
    loadDetail(row, scopeRef.current.revision, true);
  }, [loadDetail]);

  useEffect(() => {
    const feed = feedRef.current;
    if (!feed) return undefined;
    const unsubscribe = feed.subscribe((tuple: RevisionTuple) => {
      const current = stateRef.current;
      if (tuple.data_revision <= (scopeRef.current.revision || 0)) return;
      scopeRef.current = { ...scopeRef.current, revision: tuple.data_revision };
      cacheRef.current.clear();
      if (current.open && current.selected_row) loadDetail(current.selected_row, tuple.data_revision, true);
      else commit((value) => ({ ...value, data_revision: tuple.data_revision }));
    });
    return unsubscribe;
  }, [commit, loadDetail]);

  useEffect(() => {
    if (stateRef.current.open && stateRef.current.selected_row && options.dataRevision !== undefined && options.dataRevision > (stateRef.current.data_revision ?? 0)) {
      scopeRef.current = { ...scopeRef.current, revision: options.dataRevision };
      cacheRef.current.clear();
      loadDetail(stateRef.current.selected_row, options.dataRevision, true);
    }
  }, [loadDetail, options.dataRevision]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      requestAbortRef.current?.abort();
      requestAbortRef.current = null;
      feedRef.current && ownedFeedRef.current?.dispose();
    };
  }, []);

  return {
    open: state.open,
    selected_root_session_id: state.selected_root_session_id,
    selected_row: state.selected_row,
    detail: state.detail,
    data_revision: state.data_revision,
    load_state: state.load_state,
    error_code: state.error_code,
    refresh_error_code: state.refresh_error_code,
    open_detail: openDetail,
    select_session: openDetail,
    close_detail: closeDetail,
    retry_detail: refreshDetail,
  };
}
