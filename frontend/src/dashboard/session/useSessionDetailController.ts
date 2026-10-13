import { useCallback, useEffect, useRef, useState } from "react";

import { createRevisionFeed, type RevisionFeed } from "../../data/revisionFeed";
import { canonicalDashboardFilters, dashboardQueryKey, threadKeyId, usagiClient, type UsagiClient } from "../../data/usagiClient";
import {
  UsagiClientError,
  type DashboardFilters,
  type DashboardRange,
  type SessionDetailResponse,
  type SessionListItemDto,
  type ThreadKey,
} from "../../data/types";

export type SessionDetailLoadState = "closed" | "loading" | "refreshing" | "ready" | "error";

export type SessionDetailControllerViewModel = {
  open: boolean;
  selected_thread_key_id: string | null;
  selected_row: SessionListItemDto | null;
  detail: SessionDetailResponse | null;
  data_revision?: number;
  load_state: SessionDetailLoadState;
  error_code?: string;
  refresh_error_code?: string;
  open_detail: (row: SessionListItemDto) => void;
  select_session: (row: SessionListItemDto) => void;
  close_detail: () => void;
  retry_detail: () => void;
};

type CacheEntry = { revision: number; value: SessionDetailResponse };

type State = {
  open: boolean;
  selected_thread_key_id: string | null;
  selected_key: ThreadKey | null;
  selected_row: SessionListItemDto | null;
  detail: SessionDetailResponse | null;
  data_revision?: number;
  load_state: SessionDetailLoadState;
  error_code?: string;
  refresh_error_code?: string;
};

export type SessionDetailControllerOptions = {
  client?: UsagiClient;
  revisionFeed?: RevisionFeed;
  dataRevision?: number;
  sessionsReady?: boolean;
  onStaleRevision?: () => void;
};

function errorCode(error: unknown): string {
  return error instanceof UsagiClientError ? error.code : "HTTP_ERROR";
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function detailCacheKey(scopeKey: string, key: ThreadKey, revision: number): string {
  return `${scopeKey}|${threadKeyId(key)}|${revision}`;
}

export function useSessionDetailController(
  range: DashboardRange,
  filters: DashboardFilters,
  options: SessionDetailControllerOptions = {},
): SessionDetailControllerViewModel {
  const client = options.client ?? usagiClient;
  const ownedFeedRef = useRef<RevisionFeed | null>(null);
  const feedRef = useRef<RevisionFeed | null>(null);
  if (!feedRef.current) {
    feedRef.current = options.revisionFeed ?? createRevisionFeed({ client });
    if (!options.revisionFeed) ownedFeedRef.current = feedRef.current;
  }
  const scopeKey = dashboardQueryKey(range, filters);
  const latestScopeRef = useRef({ range, filters: canonicalDashboardFilters(filters) });
  latestScopeRef.current = { range, filters: canonicalDashboardFilters(filters) };
  const scopeRef = useRef({ range, filters: canonicalDashboardFilters(filters), revision: options.dataRevision ?? 0 });
  const cacheRef = useRef(new Map<string, CacheEntry>());
  const requestAbortRef = useRef<AbortController | null>(null);
  const requestGenerationRef = useRef(0);
  const mountedRef = useRef(false);
  const awaitingEpochSnapshotRef = useRef(false);
  const staleRetryRef = useRef(false);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const stateRef = useRef<State>({
    open: false,
    selected_thread_key_id: null,
    selected_key: null,
    selected_row: null,
    detail: null,
    data_revision: options.dataRevision,
    load_state: "closed",
  });
  const [state, setState] = useState<State>(stateRef.current);

  const commit = useCallback((next: State | ((current: State) => State)) => {
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

  const loadDetail = useCallback((row: SessionListItemDto, requestedRevision: number, bypassCache: boolean) => {
    const scope = scopeRef.current;
    const key = row.thread.key;
    const keyId = threadKeyId(key);
    const scopeKey = dashboardQueryKey(scope.range, scope.filters);
    const cacheKey = detailCacheKey(scopeKey, key, requestedRevision);
    const cached = !bypassCache ? cacheRef.current.get(cacheKey) : undefined;
    requestAbortRef.current?.abort();
    const generation = ++requestGenerationRef.current;

    commit((current) => ({
      ...current,
      open: true,
      selected_thread_key_id: keyId,
      selected_key: key,
      selected_row: row,
      data_revision: requestedRevision,
      detail: cached?.value ?? (current.selected_thread_key_id === keyId ? current.detail : null),
      load_state: cached ? "ready" : current.detail && current.selected_thread_key_id === keyId ? "refreshing" : "loading",
      error_code: undefined,
      refresh_error_code: undefined,
    }));
    if (cached) return;

    const controller = new AbortController();
    requestAbortRef.current = controller;
    void client.getSessionDetail({ range: scope.range, filters: scope.filters, key, signal: controller.signal }).then(
      (response) => {
        if (
          controller.signal.aborted ||
          generation !== requestGenerationRef.current ||
          !mountedRef.current ||
          !stateRef.current.open ||
          stateRef.current.selected_thread_key_id !== keyId
        ) return;
        const currentRevision = scopeRef.current.revision;
        if (response.data_revision < currentRevision) {
          options.onStaleRevision?.();
          if (!staleRetryRef.current) {
            staleRetryRef.current = true;
            loadDetail(row, currentRevision, true);
          }
          return;
        }
        staleRetryRef.current = false;
        const responseKey = detailCacheKey(dashboardQueryKey(scopeRef.current.range, scopeRef.current.filters), key, response.data_revision);
        cacheRef.current.set(responseKey, { revision: response.data_revision, value: response });
        for (const [entryKey, entry] of cacheRef.current) {
          if (entry.revision < response.data_revision) cacheRef.current.delete(entryKey);
        }
        scopeRef.current = { ...scopeRef.current, revision: response.data_revision };
        commit((current) => ({
          ...current,
          detail: response,
          data_revision: response.data_revision,
          load_state: "ready",
          error_code: undefined,
          refresh_error_code: undefined,
        }));
      },
      (error: unknown) => {
        if (controller.signal.aborted || generation !== requestGenerationRef.current || !mountedRef.current || isAbortError(error)) return;
        const code = errorCode(error);
        if (code === "DATA_LOADING") {
          commit((current) => ({
            ...current,
            load_state: current.detail ? "ready" : "loading",
            error_code: undefined,
            refresh_error_code: undefined,
          }));
          return;
        }
        if (code === "STALE_DATA_REVISION") options.onStaleRevision?.();
        commit((current) => ({
          ...current,
          load_state: current.detail ? "ready" : "error",
          error_code: current.detail ? undefined : code,
          refresh_error_code: current.detail ? code : undefined,
        }));
      },
    ).finally(() => {
      if (requestAbortRef.current === controller) requestAbortRef.current = null;
    });
  }, [client, commit, options.onStaleRevision]);

  const openDetail = useCallback((row: SessionListItemDto) => {
    if (!stateRef.current.open) {
      const active = document.activeElement;
      previousFocusRef.current = active instanceof HTMLElement ? active : null;
    }
    const requestedRevision = scopeRef.current.revision;
    staleRetryRef.current = false;
    loadDetail(row, requestedRevision, false);
  }, [loadDetail]);

  const closeDetail = useCallback(() => {
    requestAbortRef.current?.abort();
    requestAbortRef.current = null;
    requestGenerationRef.current += 1;
    commit({
      open: false,
      selected_thread_key_id: null,
      selected_key: null,
      selected_row: null,
      detail: null,
      data_revision: scopeRef.current.revision,
      load_state: "closed",
    });
    restoreFocus();
  }, [commit, restoreFocus]);

  const refreshDetail = useCallback(() => {
    const current = stateRef.current;
    if (!current.open || !current.selected_row) return;
    staleRetryRef.current = false;
    loadDetail(current.selected_row, scopeRef.current.revision, true);
  }, [loadDetail]);

  useEffect(() => {
    scopeRef.current = { ...latestScopeRef.current, revision: scopeRef.current.revision };
    cacheRef.current.clear();
    if (stateRef.current.open && stateRef.current.selected_row) loadDetail(stateRef.current.selected_row, scopeRef.current.revision, true);
  }, [loadDetail, scopeKey]);

  useEffect(() => {
    const unsubscribe = feedRef.current!.subscribe((event) => {
      const current = stateRef.current;
      if (event.kind === "epoch_reset") {
        scopeRef.current = { ...scopeRef.current, revision: event.tuple.data_revision };
        awaitingEpochSnapshotRef.current = true;
        staleRetryRef.current = false;
        cacheRef.current.clear();
        requestAbortRef.current?.abort();
        requestGenerationRef.current += 1;
        if (current.open && current.selected_row) {
          commit((value) => ({
            ...value,
            detail: null,
            data_revision: event.tuple.data_revision,
            load_state: "loading",
            error_code: undefined,
            refresh_error_code: undefined,
          }));
          loadDetail(current.selected_row, event.tuple.data_revision, true);
        } else commit((value) => ({ ...value, data_revision: event.tuple.data_revision }));
        return;
      }
      if (event.tuple.data_revision <= scopeRef.current.revision) return;
      scopeRef.current = { ...scopeRef.current, revision: event.tuple.data_revision };
      cacheRef.current.clear();
      staleRetryRef.current = false;
      if (current.open && current.selected_row) loadDetail(current.selected_row, event.tuple.data_revision, true);
      else commit((value) => ({ ...value, data_revision: event.tuple.data_revision }));
    });
    return () => {
      unsubscribe();
      requestAbortRef.current?.abort();
      requestAbortRef.current = null;
      ownedFeedRef.current?.dispose();
    };
  }, [commit, loadDetail]);

  useEffect(() => {
    const suppliedRevision = options.dataRevision;
    if (suppliedRevision === undefined) return;
    if (awaitingEpochSnapshotRef.current) {
      awaitingEpochSnapshotRef.current = false;
      if (!options.sessionsReady) {
        awaitingEpochSnapshotRef.current = true;
        return;
      }
      if (suppliedRevision !== scopeRef.current.revision) {
        scopeRef.current = { ...scopeRef.current, revision: suppliedRevision };
        cacheRef.current.clear();
        const current = stateRef.current;
        if (current.open && current.selected_row) loadDetail(current.selected_row, suppliedRevision, true);
        else commit((value) => ({ ...value, data_revision: suppliedRevision }));
      }
      return;
    }
    if (suppliedRevision <= scopeRef.current.revision) return;
    scopeRef.current = { ...scopeRef.current, revision: suppliedRevision };
    cacheRef.current.clear();
    staleRetryRef.current = false;
    const current = stateRef.current;
    if (current.open && current.selected_row) loadDetail(current.selected_row, suppliedRevision, true);
  }, [commit, loadDetail, options.dataRevision, options.sessionsReady]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      requestAbortRef.current?.abort();
      requestAbortRef.current = null;
    };
  }, []);

  return {
    open: state.open,
    selected_thread_key_id: state.selected_thread_key_id,
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
