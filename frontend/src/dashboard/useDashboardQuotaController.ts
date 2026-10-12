import { useCallback, useEffect, useRef, useState } from "react";

import { usagiClient, type UsagiClient } from "../data/usagiClient";
import type { QuotaResponse } from "../data/types";

const QUOTA_POLL_INTERVAL_MS = 60_000;
const QUOTA_READING_POLL_INTERVAL_MS = 1_000;

const loadingQuota = (): QuotaResponse => ({ reading: true, items: [] });

export type DashboardQuotaControllerOptions = {
  client?: UsagiClient;
  codexOnly?: boolean;
};

export type DashboardQuotaControllerView = {
  quota: QuotaResponse;
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
  const [quota, setQuota] = useState<QuotaResponse>(loadingQuota);
  const [refreshing, setRefreshing] = useState(false);
  const [refreshError, setRefreshError] = useState(false);
  const requestRef = useRef<AbortController | null>(null);
  const mountedRef = useRef(false);
  const refreshInFlightRef = useRef(false);
  const quotaRef = useRef<QuotaResponse>(loadingQuota());
  const pollTimerRef = useRef<number | null>(null);
  const loadRef = useRef<(() => void) | null>(null);

  const schedulePoll = useCallback(() => {
    if (!mountedRef.current || refreshInFlightRef.current) return;
    if (pollTimerRef.current !== null) window.clearTimeout(pollTimerRef.current);
    const interval = quotaRef.current.reading
      ? QUOTA_READING_POLL_INTERVAL_MS
      : QUOTA_POLL_INTERVAL_MS;
    pollTimerRef.current = window.setTimeout(() => {
      pollTimerRef.current = null;
      loadRef.current?.();
    }, interval);
  }, []);

  const loadSnapshot = useCallback(() => {
    if (!mountedRef.current || requestRef.current || refreshInFlightRef.current) return;
    const controller = new AbortController();
    requestRef.current = controller;
    void client.quota(controller.signal).then(
      (response) => {
        if (!mountedRef.current || controller.signal.aborted || requestRef.current !== controller) return;
        quotaRef.current = response;
        setQuota(response);
        setRefreshError(false);
      },
      (error: unknown) => {
        if (!mountedRef.current || controller.signal.aborted || requestRef.current !== controller || (error instanceof DOMException && error.name === "AbortError")) return;
        setRefreshError(true);
      },
    ).finally(() => {
      if (requestRef.current !== controller) return;
      requestRef.current = null;
      schedulePoll();
    });
  }, [client, schedulePoll]);

  loadRef.current = loadSnapshot;

  useEffect(() => {
    mountedRef.current = true;
    const initialQuota = loadingQuota();
    quotaRef.current = initialQuota;
    setQuota(initialQuota);
    setRefreshing(false);
    loadSnapshot();
    return () => {
      mountedRef.current = false;
      if (pollTimerRef.current !== null) window.clearTimeout(pollTimerRef.current);
      pollTimerRef.current = null;
      requestRef.current?.abort();
      requestRef.current = null;
      refreshInFlightRef.current = false;
    };
  }, [client, loadSnapshot]);

  const refresh = useCallback(() => {
    if (!mountedRef.current || refreshInFlightRef.current) return;
    refreshInFlightRef.current = true;
    if (pollTimerRef.current !== null) window.clearTimeout(pollTimerRef.current);
    pollTimerRef.current = null;
    setRefreshing(true);
    setRefreshError(false);
    requestRef.current?.abort();
    const controller = new AbortController();
    requestRef.current = controller;
    void client.refreshQuota(controller.signal).then(
      (response) => {
        if (mountedRef.current && !controller.signal.aborted && requestRef.current === controller) {
          quotaRef.current = response;
          setQuota(response);
        }
      },
      (error: unknown) => {
        if (!mountedRef.current || controller.signal.aborted || requestRef.current !== controller || (error instanceof DOMException && error.name === "AbortError")) return;
        setRefreshError(true);
      },
    ).finally(() => {
      if (requestRef.current !== controller) return;
      requestRef.current = null;
      refreshInFlightRef.current = false;
      if (mountedRef.current) {
        setRefreshing(false);
        schedulePoll();
      }
    });
  }, [client, schedulePoll]);

  const visibleQuota = codexOnly
    ? { ...quota, items: quota.items.filter((item) => item.provider === "codex") }
    : quota;

  return {
    quota: visibleQuota,
    refreshing,
    refresh_error: refreshError,
    refresh_available: true,
    refresh,
  };
}
