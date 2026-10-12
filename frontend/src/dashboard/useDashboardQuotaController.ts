import { useCallback, useEffect, useRef, useState } from "react";

import { usagiClient, type UsagiClient } from "../data/usagiClient";
import type { QuotaResponse } from "../data/types";

const QUOTA_POLL_INTERVAL_MS = 60_000;

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
  const loadRef = useRef<(() => void) | null>(null);

  const loadSnapshot = useCallback(() => {
    if (!mountedRef.current || requestRef.current) return;
    const controller = new AbortController();
    requestRef.current = controller;
    void client.quota(controller.signal).then(
      (response) => {
        if (!mountedRef.current || controller.signal.aborted) return;
        setQuota(response);
        setRefreshError(false);
      },
      (error: unknown) => {
        if (!mountedRef.current || controller.signal.aborted || (error instanceof DOMException && error.name === "AbortError")) return;
        setRefreshError(true);
      },
    ).finally(() => {
      if (requestRef.current === controller) requestRef.current = null;
    });
  }, [client]);

  loadRef.current = loadSnapshot;

  useEffect(() => {
    mountedRef.current = true;
    setQuota(loadingQuota());
    loadSnapshot();
    const poll = window.setInterval(() => loadRef.current?.(), QUOTA_POLL_INTERVAL_MS);
    return () => {
      mountedRef.current = false;
      window.clearInterval(poll);
      requestRef.current?.abort();
      requestRef.current = null;
    };
  }, [client, loadSnapshot]);

  const refresh = useCallback(() => {
    if (!mountedRef.current || refreshInFlightRef.current) return;
    refreshInFlightRef.current = true;
    setRefreshing(true);
    setRefreshError(false);
    requestRef.current?.abort();
    const controller = new AbortController();
    requestRef.current = controller;
    void client.refreshQuota(controller.signal).then(
      (response) => {
        if (mountedRef.current && !controller.signal.aborted) setQuota(response);
      },
      (error: unknown) => {
        if (!mountedRef.current || controller.signal.aborted || (error instanceof DOMException && error.name === "AbortError")) return;
        setRefreshError(true);
      },
    ).finally(() => {
      if (requestRef.current === controller) requestRef.current = null;
      if (mountedRef.current) setRefreshing(false);
      refreshInFlightRef.current = false;
    });
  }, [client]);

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
