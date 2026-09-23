import { useCallback, useEffect, useRef, useState } from "react";

import { usagiClient, type UsagiClient } from "../data/usagiClient";
import type { AntigravityQuotaResponse, CodexQuotaResponse } from "../data/types";

const LOADING_RETRY_DELAY_MS = 1_000;
const QUOTA_POLL_INTERVAL_MS = 300_000;

function loadingCodexSnapshot(): CodexQuotaResponse {
  return {
    status: "loading",
    account_email: null,
    plan_type: null,
    session: null,
    weekly: null,
    reset_credits_available: null,
    fetched_at_ms: null,
  };
}

function unavailableCodexSnapshot(): CodexQuotaResponse {
  return { ...loadingCodexSnapshot(), status: "unavailable" };
}

function loadingAntigravitySnapshot(): AntigravityQuotaResponse {
  return {
    status: "loading",
    account_email: null,
    plan_type: null,
    session: null,
    weekly: null,
    fetched_at_ms: null,
  };
}

function unavailableAntigravitySnapshot(): AntigravityQuotaResponse {
  return { ...loadingAntigravitySnapshot(), status: "unavailable" };
}

export type DashboardQuotaControllerOptions = {
  client?: UsagiClient;
  codexOnly?: boolean;
};

export type DashboardQuotaControllerView = {
  codex: CodexQuotaResponse;
  antigravity: AntigravityQuotaResponse;
  refreshing: boolean;
  refresh_error: boolean;
  refresh_available: boolean;
  refresh: () => void;
};

export function useDashboardQuotaController(
  options: DashboardQuotaControllerOptions = {},
): DashboardQuotaControllerView {
  const client = options.client ?? usagiClient;
  const codexOnly = options.codexOnly ?? false;
  const [codex, setCodex] = useState<CodexQuotaResponse>(loadingCodexSnapshot);
  const [antigravity, setAntigravity] = useState<AntigravityQuotaResponse>(loadingAntigravitySnapshot);
  const [refreshing, setRefreshing] = useState(false);
  const [refreshError, setRefreshError] = useState(false);
  const codexRef = useRef(codex);
  const antigravityRef = useRef(antigravity);
  const requestRef = useRef<AbortController | null>(null);
  const retryRef = useRef<number | null>(null);
  const pollRef = useRef<number | null>(null);
  const mountedRef = useRef(false);
  const refreshInFlightRef = useRef(false);
  const loadRef = useRef<(() => void) | null>(null);

  const commitCodex = useCallback((next: CodexQuotaResponse) => {
    codexRef.current = next;
    setCodex(next);
  }, []);
  const commitAntigravity = useCallback((next: AntigravityQuotaResponse) => {
    antigravityRef.current = next;
    setAntigravity(next);
  }, []);

  const loadSnapshots = useCallback(() => {
    if (!mountedRef.current || requestRef.current) return;
    const controller = new AbortController();
    requestRef.current = controller;

    const antigravityRequest = codexOnly
      ? Promise.resolve(unavailableAntigravitySnapshot())
      : client.antigravityQuota(controller.signal);

    void Promise.allSettled([
      client.codexQuota(controller.signal),
      antigravityRequest,
    ]).then(([codexResult, antigravityResult]) => {
      if (!mountedRef.current || controller.signal.aborted) return;
      let retryLoading = false;

      if (codexResult.status === "fulfilled") {
        retryLoading ||= codexResult.value.status === "loading";
        if (codexResult.value.status === "ready" || codexRef.current.status !== "ready") {
          commitCodex(codexResult.value);
        }
      } else if (codexRef.current.status !== "ready") {
        commitCodex(unavailableCodexSnapshot());
      }

      if (antigravityResult.status === "fulfilled") {
        retryLoading ||= antigravityResult.value.status === "loading";
        if (antigravityResult.value.status === "ready" || antigravityRef.current.status !== "ready") {
          commitAntigravity(antigravityResult.value);
        }
      } else if (antigravityRef.current.status !== "ready") {
        commitAntigravity(unavailableAntigravitySnapshot());
      }

      if (codexResult.status === "fulfilled" && antigravityResult.status === "fulfilled") setRefreshError(false);

      if (retryLoading) {
        if (retryRef.current === null) {
          retryRef.current = window.setTimeout(() => {
            retryRef.current = null;
            loadRef.current?.();
          }, LOADING_RETRY_DELAY_MS);
        }
      } else {
        if (retryRef.current !== null) window.clearTimeout(retryRef.current);
        retryRef.current = null;
        if (pollRef.current === null) {
          pollRef.current = window.setInterval(() => loadRef.current?.(), QUOTA_POLL_INTERVAL_MS);
        }
      }
    }).finally(() => {
      if (requestRef.current === controller) requestRef.current = null;
    });
  }, [client, codexOnly, commitAntigravity, commitCodex]);

  loadRef.current = loadSnapshots;

  useEffect(() => {
    mountedRef.current = true;
    commitCodex(loadingCodexSnapshot());
    commitAntigravity(loadingAntigravitySnapshot());
    loadSnapshots();

    return () => {
      mountedRef.current = false;
      if (retryRef.current !== null) window.clearTimeout(retryRef.current);
      if (pollRef.current !== null) window.clearInterval(pollRef.current);
      retryRef.current = null;
      pollRef.current = null;
      requestRef.current?.abort();
      requestRef.current = null;
    };
  }, [client, codexOnly, commitAntigravity, commitCodex, loadSnapshots]);

  const refresh = useCallback(() => {
    if (!mountedRef.current || refreshInFlightRef.current) return;

    refreshInFlightRef.current = true;
    setRefreshing(true);
    setRefreshError(false);
    if (retryRef.current !== null) window.clearTimeout(retryRef.current);
    retryRef.current = null;
    if (pollRef.current !== null) window.clearInterval(pollRef.current);
    pollRef.current = window.setInterval(() => loadRef.current?.(), QUOTA_POLL_INTERVAL_MS);

    requestRef.current?.abort();
    const controller = new AbortController();
    requestRef.current = controller;

    const antigravityRefresh = codexOnly
      ? Promise.resolve(unavailableAntigravitySnapshot())
      : client.refreshAntigravityQuota(controller.signal);

    void Promise.allSettled([
      client.refreshCodexQuota(controller.signal),
      antigravityRefresh,
    ]).then(([codexResult, antigravityResult]) => {
      if (!mountedRef.current || controller.signal.aborted) return;
      let retryLoading = false;
      let failed = false;

      if (codexResult.status === "fulfilled") {
        retryLoading ||= codexResult.value.status === "loading";
        if (codexResult.value.status === "ready" || codexRef.current.status !== "ready") {
          commitCodex(codexResult.value);
        }
      } else {
        failed = true;
        if (codexRef.current.status !== "ready") commitCodex(unavailableCodexSnapshot());
      }

      if (antigravityResult.status === "fulfilled") {
        retryLoading ||= antigravityResult.value.status === "loading";
        if (antigravityResult.value.status === "ready" || antigravityRef.current.status !== "ready") {
          commitAntigravity(antigravityResult.value);
        }
      } else {
        failed = true;
        if (antigravityRef.current.status !== "ready") commitAntigravity(unavailableAntigravitySnapshot());
      }

      setRefreshError(failed);
      if (retryLoading && retryRef.current === null) {
        retryRef.current = window.setTimeout(() => {
          retryRef.current = null;
          loadRef.current?.();
        }, LOADING_RETRY_DELAY_MS);
      }
    }).finally(() => {
      if (requestRef.current === controller) requestRef.current = null;
      if (mountedRef.current) setRefreshing(false);
      refreshInFlightRef.current = false;
    });
  }, [client, codexOnly, commitAntigravity, commitCodex]);

  return {
    codex,
    antigravity,
    refreshing,
    refresh_error: refreshError,
    refresh_available: true,
    refresh,
  };
}
