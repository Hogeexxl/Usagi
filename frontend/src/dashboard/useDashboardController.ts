import { useCallback, useEffect, useRef, useState, type MutableRefObject } from "react";

import {
  usagiClient,
  canonicalDashboardFilters,
  dashboardRangeKey,
  dashboardRangesEqual,
  dashboardQueryKey,
  type UsagiClient,
} from "../data/usagiClient";
import { createRevisionFeed, type RevisionEventSource, type RevisionFeed } from "../data/revisionFeed";
import { DASHBOARD_SCOPE_POLICIES, resolveDashboardScope } from "./scope";
import {
  type DashboardRange,
  type RefreshAccepted,
  type RevisionTuple,
  type StatusResponse,
  type SummaryResponse,
  type DashboardFilters,
  type FilterOptionsResponse,
  UsagiClientError,
} from "../data/types";

export type LoadState = "initial" | "loading" | "ready" | "error";
export type RefreshState =
  | "idle"
  | "requesting"
  | "running"
  | "failed"
  | "tracking_error"
  | "source_changed";

export type DashboardViewModel = {
  range: DashboardRange;
  filters: DashboardFilters;
  metrics: SummaryResponse["usage"] | null;
  data_revision: number;
  last_scan_completed_at_ms: number | null;
  filter_options: FilterOptionsResponse | null;
  filter_options_loading: boolean;
  filter_options_stale: boolean;
  filter_options_error_code?: string;
  modelFilterActive: boolean;
  projectFilterActive: boolean;
  anyFilterActive: boolean;
  load_state: LoadState;
  refresh_state: RefreshState;
  error_code?: string;
  select_range: (range: DashboardRange) => void;
  select_filters: (filters: DashboardFilters) => void;
  clear_filters: () => void;
  retry_filter_options: () => void;
  retry_load: () => void;
  request_refresh: () => void;
  retry_refresh_status: () => void;
};

type Snapshot = SummaryResponse;
type FailureFlags = {
  summary: boolean;
  status: boolean;
  revision: boolean;
};

type InternalState = {
  range: DashboardRange;
  filters: DashboardFilters;
  snapshot: Snapshot | null;
  last_scan_completed_at_ms: number | null;
  load_state: LoadState;
  summary_loading: boolean;
  refresh_state: RefreshState;
  error_code?: string;
  status_ready: boolean;
  filter_options: FilterOptionsResponse | null;
  filter_options_loading: boolean;
  filter_options_stale: boolean;
  filter_options_error_code?: string;
};

export type DashboardControllerOptions = {
  client?: UsagiClient;
  eventSourceFactory?: (url: string) => RevisionEventSource;
  pollIntervalMs?: number;
  revisionFeed?: RevisionFeed;
};

const DEFAULT_POLL_INTERVAL = 60_000;
const STATUS_NOT_READY = "STATUS_NOT_READY";
const TARGET_QUEUED = "TARGET_QUEUED";

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function clientErrorCode(error: unknown): string {
  return error instanceof UsagiClientError ? error.code : "HTTP_ERROR";
}

function clientErrorStatus(error: unknown): number {
  return error instanceof UsagiClientError ? error.status : 0;
}

function setFailure(ref: MutableRefObject<FailureFlags>, key: keyof FailureFlags, value: boolean) {
  ref.current = { ...ref.current, [key]: value };
}

export function useDashboardController(options: DashboardControllerOptions = {}): DashboardViewModel {
  const client = options.client ?? usagiClient;
  const pollIntervalMs = options.pollIntervalMs ?? DEFAULT_POLL_INTERVAL;
  const ownedRevisionFeedRef = useRef<RevisionFeed | null>(null);
  const revisionFeedRef = useRef<RevisionFeed | null>(null);
  if (!revisionFeedRef.current) {
    revisionFeedRef.current = options.revisionFeed ?? createRevisionFeed({ client, eventSourceFactory: options.eventSourceFactory, pollIntervalMs });
    if (!options.revisionFeed) ownedRevisionFeedRef.current = revisionFeedRef.current;
  }
  const [state, setState] = useState<InternalState>({
    range: { key: "today" },
    filters: { models: [], projects: [] },
    snapshot: null,
    last_scan_completed_at_ms: null,
    load_state: "initial",
    summary_loading: true,
    refresh_state: "idle",
    status_ready: false,
    filter_options: null,
    filter_options_loading: true,
    filter_options_stale: false,
    filter_options_error_code: undefined,
  });
  const stateRef = useRef(state);
  const snapshotsRef = useRef(new Map<string, Snapshot>());
  const failuresRef = useRef<FailureFlags>({ summary: false, status: false, revision: false });
  const summaryAbortRef = useRef<AbortController | null>(null);
  const statusAbortRef = useRef<AbortController | null>(null);
  const refreshAbortRef = useRef<AbortController | null>(null);
  const summaryGenerationRef = useRef(0);
  const statusGenerationRef = useRef(0);
  const refreshGenerationRef = useRef(0);
  const statusRevisionRef = useRef(0);
  const acceptedTupleRef = useRef<RevisionTuple>({ data_revision: 0, status_revision: 0 });
  const statusRef = useRef<StatusResponse | null>(null);
  const targetRef = useRef<{ scan_id: string; kind: "started" | "followup" } | null>(null);
  const failuresDuringTrackingRef = useRef(false);
  const filterOptionsAbortRef = useRef<AbortController | null>(null);
  const filterOptionsGenerationRef = useRef(0);
  const filterOptionsStartedRef = useRef(false);
  const effectMountedRef = useRef(false);
  const filterOptionsLoadingRef = useRef(false);
  const filterOptionsRevisionRef = useRef(0);
  const pendingOptionsRevisionRef = useRef(0);
  const terminalOptionsRefreshScansRef = useRef(new Set<string>());

  const commit = useCallback((next: InternalState | ((current: InternalState) => InternalState)) => {
    const value = typeof next === "function" ? next(stateRef.current) : next;
    stateRef.current = value;
    setState(value);
  }, []);

  const setLoadState = useCallback(() => {
    const current = stateRef.current;
    const hasOrdinaryFailure =
      current.refresh_state !== "tracking_error" &&
      (failuresRef.current.summary || failuresRef.current.status || failuresRef.current.revision);
    let loadState: LoadState;
    if (current.summary_loading && !current.snapshot) {
      loadState = current.load_state === "initial" ? "initial" : "loading";
    } else if (hasOrdinaryFailure) {
      loadState = "error";
    } else if (current.snapshot) {
      loadState = current.summary_loading ? "loading" : "ready";
    } else {
      loadState = current.load_state === "initial" ? "initial" : "loading";
    }
    commit((value) => ({ ...value, load_state: loadState }));
  }, [commit]);

  const observeOptionsRevision = useCallback(
    (revision: number) => {
      if (revision <= filterOptionsRevisionRef.current) return;
      pendingOptionsRevisionRef.current = Math.max(pendingOptionsRevisionRef.current, revision);
      commit((value) => ({ ...value, filter_options_stale: true }));
    },
    [commit],
  );

  const loadFilterOptions = useCallback(() => {
    if (filterOptionsLoadingRef.current) return;
    filterOptionsAbortRef.current?.abort();
    const controller = new AbortController();
    filterOptionsAbortRef.current = controller;
    const generation = ++filterOptionsGenerationRef.current;
    filterOptionsLoadingRef.current = true;
    commit((value) => ({
      ...value,
      filter_options_loading: true,
      filter_options_error_code: undefined,
    }));
    void client.filterOptions(controller.signal).then(
      (response) => {
        if (controller.signal.aborted || generation !== filterOptionsGenerationRef.current) return;
        filterOptionsLoadingRef.current = false;
        const previousRevision = filterOptionsRevisionRef.current;
        if (response.data_revision >= previousRevision) {
          filterOptionsRevisionRef.current = response.data_revision;
          const pendingRevision = pendingOptionsRevisionRef.current;
          if (response.data_revision >= pendingRevision) pendingOptionsRevisionRef.current = 0;
          commit((value) => ({
            ...value,
            filter_options: response,
            filter_options_loading: false,
            filter_options_stale: response.data_revision < pendingRevision,
            filter_options_error_code: undefined,
          }));
        } else {
          commit((value) => ({ ...value, filter_options_loading: false, filter_options_stale: true }));
        }
      },
      (error: unknown) => {
        if (controller.signal.aborted || generation !== filterOptionsGenerationRef.current || isAbortError(error)) return;
        filterOptionsLoadingRef.current = false;
        commit((value) => ({
          ...value,
          filter_options_loading: false,
          filter_options_stale: true,
          filter_options_error_code: clientErrorCode(error),
        }));
      },
    );
  }, [client, commit]);

  const maybeRefreshFilterOptions = useCallback(
    (status: StatusResponse, scanId: string) => {
      observeOptionsRevision(status.data_revision);
      if (status.active_scan_id || status.followup?.state === "queued") return;
      if (pendingOptionsRevisionRef.current <= filterOptionsRevisionRef.current) return;
      if (terminalOptionsRefreshScansRef.current.has(scanId) || filterOptionsLoadingRef.current) return;
      terminalOptionsRefreshScansRef.current.add(scanId);
      loadFilterOptions();
    },
    [loadFilterOptions, observeOptionsRevision],
  );

  const loadSummary = useCallback(
    (range: DashboardRange, filters: DashboardFilters = stateRef.current.filters) => {
      const scope = resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.kpi, range, filters);
      const canonicalFilters = canonicalDashboardFilters(scope.filters);
      const queryKey = dashboardQueryKey(scope.range, canonicalFilters);
      summaryAbortRef.current?.abort();
      const controller = new AbortController();
      summaryAbortRef.current = controller;
      const generation = ++summaryGenerationRef.current;
      if (queryKey === dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) {
        commit((value) => ({
          ...value,
          summary_loading: true,
          load_state: value.snapshot ? "loading" : value.load_state === "initial" ? "initial" : "loading",
        }));
      }
      void client.summary(scope.range, canonicalFilters, controller.signal).then(
        (response) => {
          if (controller.signal.aborted || summaryGenerationRef.current !== generation) return;
          if (response.range.key !== dashboardRangeKey(scope.range) || queryKey !== dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) {
            setFailure(failuresRef, "summary", true);
            if (queryKey === dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) {
              commit((value) => ({
                ...value,
                summary_loading: false,
                load_state: "error",
                error_code: value.status_ready ? "HTTP_ERROR" : STATUS_NOT_READY,
              }));
            }
            return;
          }
          const previous = snapshotsRef.current.get(queryKey);
          const acceptedDataRevision = Math.max(previous?.data_revision ?? 0, acceptedTupleRef.current.data_revision);
          if (response.data_revision < acceptedDataRevision) {
            setFailure(failuresRef, "summary", false);
            if (queryKey === dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) {
              commit((value) => ({ ...value, summary_loading: false }));
              setLoadState();
            }
            return;
          }
          snapshotsRef.current.set(queryKey, response);
          acceptedTupleRef.current = {
            ...acceptedTupleRef.current,
            data_revision: response.data_revision,
          };
          setFailure(failuresRef, "summary", false);
          if (queryKey === dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) {
            commit((value) => ({
              ...value,
              snapshot: response,
              summary_loading: false,
              error_code: value.status_ready ? undefined : STATUS_NOT_READY,
            }));
            setLoadState();
          }
        },
        (error: unknown) => {
          if (controller.signal.aborted || summaryGenerationRef.current !== generation || isAbortError(error)) return;
          setFailure(failuresRef, "summary", true);
          if (queryKey === dashboardQueryKey(stateRef.current.range, stateRef.current.filters)) {
            commit((value) => ({
              ...value,
              summary_loading: false,
              load_state: "error",
              error_code: value.status_ready ? clientErrorCode(error) : STATUS_NOT_READY,
            }));
          }
        },
      );
    },
    [client, commit, setLoadState],
  );

  const statusFunctionRef = useRef<(targetScanId?: string, tracking?: boolean) => void>(() => undefined);

  const reduceStatus = useCallback(
    (status: StatusResponse, requestedTarget?: string) => {
      statusRef.current = status;
      const target = targetRef.current;
      if (target) {
        if (!status.target_scan || status.target_scan.scan_id !== target.scan_id || (requestedTarget && status.target_scan.scan_id !== requestedTarget)) {
          failuresDuringTrackingRef.current = true;
          commit((value) => ({
            ...value,
            refresh_state: "tracking_error",
            error_code: "SCAN_NOT_FOUND",
          }));
          return;
        }
        switch (status.target_scan.state) {
          case "queued":
            commit((value) => ({ ...value, refresh_state: "running", error_code: TARGET_QUEUED }));
            return;
          case "running":
            commit((value) => ({ ...value, refresh_state: "running", error_code: undefined }));
            return;
          case "completed":
          case "failed":
          case "start_failed": {
            const completed = status.target_scan.state === "completed";
            const oldRevision = stateRef.current.snapshot?.data_revision ?? -1;
            targetRef.current = null;
            failuresDuringTrackingRef.current = false;
            if (
              status.followup &&
              status.followup.scan_id !== target.scan_id &&
              (status.followup.state === "queued" || status.followup.state === "start_failed")
            ) {
              targetRef.current = { scan_id: status.followup.scan_id, kind: "followup" };
              commit((value) => ({
                ...value,
                refresh_state: "running",
                error_code: status.followup?.state === "queued" ? TARGET_QUEUED : undefined,
              }));
              statusFunctionRef.current(status.followup.scan_id, true);
            } else {
              maybeRefreshFilterOptions(status, target.scan_id);
              commit((value) => ({
                ...value,
                refresh_state: completed ? "idle" : "failed",
                error_code: completed ? undefined : status.target_scan?.error_code ?? "SCAN_FAILED",
              }));
            }
            if (completed && status.data_revision > oldRevision) loadSummary(stateRef.current.range, stateRef.current.filters);
            return;
          }
        }
      }

      if (stateRef.current.refresh_state === "requesting") return;
      if (status.source_binding_status === "source_changed" || status.scan_state === "source_changed") {
        commit((value) => ({ ...value, refresh_state: "source_changed", error_code: "SOURCE_CHANGED" }));
        return;
      }
      if (status.source_binding_status !== "ready") {
        commit((value) => ({ ...value, refresh_state: "idle", error_code: STATUS_NOT_READY }));
        return;
      }
      const followup = status.followup;
      if (followup && (followup.state === "queued" || followup.state === "start_failed")) {
        targetRef.current = { scan_id: followup.scan_id, kind: "followup" };
        commit((value) => ({
          ...value,
          refresh_state: "running",
          error_code: followup.state === "queued" ? TARGET_QUEUED : undefined,
        }));
        statusFunctionRef.current(followup.scan_id, true);
        return;
      }
      if (status.active_scan_id) {
        targetRef.current = { scan_id: status.active_scan_id, kind: "started" };
        commit((value) => ({ ...value, refresh_state: "running", error_code: undefined }));
        statusFunctionRef.current(status.active_scan_id, true);
        return;
      }
      if (status.scan_state === "startup" || status.scan_state === "running") {
        commit((value) => ({ ...value, refresh_state: "running", error_code: undefined }));
        return;
      }
      if (
        status.last_finished_scan_id &&
        (status.last_finished_scan_result === "completed" ||
          status.last_finished_scan_result === "failed" ||
          status.last_finished_scan_result === "start_failed")
      ) {
        maybeRefreshFilterOptions(status, status.last_finished_scan_id);
      }
      if (stateRef.current.refresh_state !== "failed" && stateRef.current.refresh_state !== "tracking_error") {
        commit((value) => ({ ...value, refresh_state: "idle", error_code: undefined }));
      }
    },
    [commit, loadSummary, maybeRefreshFilterOptions],
  );

  const requestStatus = useCallback(
    (targetScanId?: string, tracking = Boolean(targetScanId)) => {
      statusAbortRef.current?.abort();
      const controller = new AbortController();
      statusAbortRef.current = controller;
      const generation = ++statusGenerationRef.current;
      void client.getStatus(targetScanId, controller.signal).then(
        (status) => {
          if (controller.signal.aborted || generation !== statusGenerationRef.current) return;
          observeOptionsRevision(status.data_revision);
          const previousTuple = acceptedTupleRef.current;
          const staleStatus = status.status_revision < statusRevisionRef.current;
          if (status.status_revision > statusRevisionRef.current) statusRevisionRef.current = status.status_revision;
          const staleTuple = status.data_revision < previousTuple.data_revision || status.status_revision < previousTuple.status_revision;
          if (!staleTuple) acceptedTupleRef.current = status;
          if (staleStatus) {
            if (status.data_revision > previousTuple.data_revision) loadSummary(stateRef.current.range, stateRef.current.filters);
            if (status.data_revision <= previousTuple.data_revision) return;
          }
          statusRef.current = status;
          commit((value) => ({
            ...value,
            status_ready: true,
            last_scan_completed_at_ms: staleTuple ? value.last_scan_completed_at_ms : status.last_scan_completed_at_ms,
          }));
          setFailure(failuresRef, "status", false);
          if (tracking && targetRef.current) {
            failuresDuringTrackingRef.current = false;
            reduceStatus(status, targetScanId);
          } else {
            reduceStatus(status, targetScanId);
          }
          setLoadState();
        },
        (error: unknown) => {
          if (controller.signal.aborted || generation !== statusGenerationRef.current || isAbortError(error)) return;
          if (tracking || targetRef.current) {
            failuresDuringTrackingRef.current = true;
            setFailure(failuresRef, "status", false);
            commit((value) => ({
              ...value,
              refresh_state: "tracking_error",
              error_code: clientErrorCode(error),
            }));
          } else {
            setFailure(failuresRef, "status", true);
            commit((value) => ({ ...value, status_ready: false, load_state: "error", error_code: STATUS_NOT_READY }));
          }
          setLoadState();
        },
      );
    },
    [client, commit, loadSummary, observeOptionsRevision, reduceStatus, setLoadState],
  );
  statusFunctionRef.current = requestStatus;

  const handleRevision = useCallback(
    (tuple: RevisionTuple) => {
      const previous = acceptedTupleRef.current;
      if (tuple.data_revision < previous.data_revision || tuple.status_revision < previous.status_revision) return;
      observeOptionsRevision(tuple.data_revision);
      setFailure(failuresRef, "revision", false);
      const dataAdvanced = tuple.data_revision > previous.data_revision;
      const statusAdvanced = tuple.status_revision > previous.status_revision;
      if (!dataAdvanced && !statusAdvanced) return;
      acceptedTupleRef.current = tuple;
      if (dataAdvanced) loadSummary(stateRef.current.range, stateRef.current.filters);
      if (statusAdvanced) requestStatus(targetRef.current?.scan_id, Boolean(targetRef.current));
    },
    [loadSummary, observeOptionsRevision, requestStatus],
  );

  const handleRevisionError = useCallback(
    (error: unknown) => {
      if (isAbortError(error)) return;
      setFailure(failuresRef, "revision", true);
      commit((value) => ({
        ...value,
        load_state: "error",
        error_code: value.status_ready ? clientErrorCode(error) : STATUS_NOT_READY,
      }));
    },
    [commit],
  );

  const selectRange = useCallback(
    (range: DashboardRange) => {
      if (dashboardRangesEqual(range, stateRef.current.range)) return;
      const filters = stateRef.current.filters;
      const snapshot = snapshotsRef.current.get(dashboardQueryKey(range, filters)) ?? null;
      failuresRef.current = { ...failuresRef.current, summary: false };
      commit((value) => ({
        ...value,
        range,
        snapshot,
        summary_loading: true,
        load_state: "loading",
        error_code: undefined,
      }));
      loadSummary(range, filters);
    },
    [commit, loadSummary],
  );

  const selectFilters = useCallback(
    (filters: DashboardFilters) => {
      const canonicalFilters = canonicalDashboardFilters(filters);
      const range = stateRef.current.range;
      const nextKey = dashboardQueryKey(range, canonicalFilters);
      const currentKey = dashboardQueryKey(range, stateRef.current.filters);
      if (nextKey === currentKey) {
        commit((value) => ({ ...value, filters: canonicalFilters }));
        return;
      }
      const snapshot = snapshotsRef.current.get(nextKey) ?? null;
      failuresRef.current = { ...failuresRef.current, summary: false };
      commit((value) => ({
        ...value,
        filters: canonicalFilters,
        snapshot,
        summary_loading: true,
        load_state: "loading",
        error_code: undefined,
      }));
      loadSummary(range, canonicalFilters);
    },
    [commit, loadSummary],
  );

  const clearFilters = useCallback(() => {
    selectFilters({ models: [], projects: [] });
  }, [selectFilters]);

  const retryFilterOptions = useCallback(() => {
    loadFilterOptions();
  }, [loadFilterOptions]);

  const requestRefresh = useCallback(() => {
    const current = stateRef.current;
    const status = statusRef.current;
    if (
      current.refresh_state === "requesting" ||
      current.refresh_state === "running" ||
      current.refresh_state === "source_changed" ||
      targetRef.current ||
      !current.status_ready ||
      !status ||
      status.source_binding_status !== "ready" ||
      (status.scan_state !== "idle" && status.scan_state !== "failed") ||
      status.active_scan_id !== null ||
      status.followup?.state === "queued"
    ) {
      return;
    }
    refreshAbortRef.current?.abort();
    const controller = new AbortController();
    refreshAbortRef.current = controller;
    const generation = ++refreshGenerationRef.current;
    commit((value) => ({ ...value, refresh_state: "requesting", error_code: undefined }));
    void client.refresh(controller.signal).then(
      (accepted: RefreshAccepted) => {
        if (controller.signal.aborted || generation !== refreshGenerationRef.current) return;
        targetRef.current = {
          scan_id: accepted.scan_id,
          kind: accepted.disposition === "started" ? "started" : "followup",
        };
        commit((value) => ({ ...value, refresh_state: "running", error_code: undefined }));
        const cachedStatus = statusRef.current;
        if (cachedStatus?.target_scan?.scan_id === accepted.scan_id) {
          reduceStatus(cachedStatus, accepted.scan_id);
          if (targetRef.current?.scan_id === accepted.scan_id) {
            requestStatus(accepted.scan_id, true);
          }
        } else {
          requestStatus(accepted.scan_id, true);
        }
      },
      (error: unknown) => {
        if (controller.signal.aborted || generation !== refreshGenerationRef.current || isAbortError(error)) return;
        const code = clientErrorCode(error);
        const statusCode = clientErrorStatus(error);
        if (statusCode === 409 && code === "SOURCE_CHANGED") {
          commit((value) => ({ ...value, refresh_state: "source_changed", error_code: code }));
        } else if (statusCode === 403 || code === "FORBIDDEN" || code === "FORBIDDEN_HOST" || code === "FORBIDDEN_ORIGIN") {
          commit((value) => ({ ...value, refresh_state: "failed", error_code: "FORBIDDEN" }));
        } else {
          commit((value) => ({ ...value, refresh_state: "failed", error_code: code }));
        }
      },
    );
  }, [client, commit, reduceStatus, requestStatus]);

  const retryRefreshStatus = useCallback(() => {
    const target = targetRef.current;
    if (!target) return;
    failuresDuringTrackingRef.current = false;
    commit((value) => ({ ...value, refresh_state: "running", error_code: undefined }));
    requestStatus(target.scan_id, true);
  }, [commit, requestStatus]);

  const retryLoad = useCallback(() => {
    const current = stateRef.current;
    const failures = failuresRef.current;
    if (failures.summary) loadSummary(current.range, current.filters);
    if (failures.status && !failuresDuringTrackingRef.current) requestStatus(targetRef.current?.scan_id, Boolean(targetRef.current));
    if (failures.revision) revisionFeedRef.current?.retry_now();
    if (!failures.summary && !failures.status && !failures.revision && !current.snapshot) loadSummary(current.range, current.filters);
  }, [loadSummary, requestStatus]);

  useEffect(() => {
    effectMountedRef.current = true;
    loadSummary({ key: "today" }, stateRef.current.filters);
    if (!filterOptionsStartedRef.current) {
      filterOptionsStartedRef.current = true;
      loadFilterOptions();
    }
    requestStatus();
    const unsubscribe = revisionFeedRef.current?.subscribe(handleRevision, handleRevisionError);
    return () => {
      summaryAbortRef.current?.abort();
      statusAbortRef.current?.abort();
      refreshAbortRef.current?.abort();
      effectMountedRef.current = false;
      queueMicrotask(() => {
        if (effectMountedRef.current) return;
        filterOptionsAbortRef.current?.abort();
        filterOptionsStartedRef.current = false;
      });
      unsubscribe?.();
      ownedRevisionFeedRef.current?.dispose();
    };
    // The controller is intentionally mounted once for the page lifetime.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const current = state.snapshot;
  const modelFilterActive = state.filters.models.length > 0;
  const projectFilterActive = state.filters.projects.length > 0;
  return {
    range: state.range,
    filters: state.filters,
    metrics: current?.usage ?? null,
    data_revision: current?.data_revision ?? 0,
    last_scan_completed_at_ms: state.last_scan_completed_at_ms,
    filter_options: state.filter_options,
    filter_options_loading: state.filter_options_loading,
    filter_options_stale: state.filter_options_stale,
    filter_options_error_code: state.filter_options_error_code,
    modelFilterActive,
    projectFilterActive,
    anyFilterActive: modelFilterActive || projectFilterActive,
    load_state: state.load_state,
    refresh_state: state.refresh_state,
    error_code: state.error_code,
    select_range: selectRange,
    select_filters: selectFilters,
    clear_filters: clearFilters,
    retry_filter_options: retryFilterOptions,
    retry_load: retryLoad,
    request_refresh: requestRefresh,
    retry_refresh_status: retryRefreshStatus,
  };
}
