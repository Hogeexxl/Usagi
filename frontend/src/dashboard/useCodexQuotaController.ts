import { useEffect, useRef, useState } from "react";

import { usagiClient, type UsagiClient } from "../data/usagiClient";
import type { CodexQuotaResponse } from "../data/types";

const LOADING_RETRY_DELAY_MS = 1_000;
const QUOTA_POLL_INTERVAL_MS = 300_000;

function loadingSnapshot(): CodexQuotaResponse {
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

function unavailableSnapshot(): CodexQuotaResponse {
  return {
    status: "unavailable",
    account_email: null,
    plan_type: null,
    session: null,
    weekly: null,
    reset_credits_available: null,
    fetched_at_ms: null,
  };
}

export type CodexQuotaControllerOptions = {
  client?: UsagiClient;
};

export function useCodexQuotaController(options: CodexQuotaControllerOptions = {}): CodexQuotaResponse {
  const client = options.client ?? usagiClient;
  const [snapshot, setSnapshot] = useState<CodexQuotaResponse>(loadingSnapshot);
  const snapshotRef = useRef(snapshot);
  const requestRef = useRef<AbortController | null>(null);
  const loadingRetryRef = useRef<number | null>(null);
  const pollRef = useRef<number | null>(null);
  const settledRef = useRef(false);
  const mountedRef = useRef(false);

  useEffect(() => {
    mountedRef.current = true;
    settledRef.current = false;
    snapshotRef.current = loadingSnapshot();
    setSnapshot(snapshotRef.current);

    const clearLoadingRetry = () => {
      if (loadingRetryRef.current !== null) {
        window.clearTimeout(loadingRetryRef.current);
        loadingRetryRef.current = null;
      }
    };

    const clearPoll = () => {
      if (pollRef.current !== null) {
        window.clearInterval(pollRef.current);
        pollRef.current = null;
      }
    };

    const commit = (next: CodexQuotaResponse) => {
      if (!mountedRef.current) return;
      snapshotRef.current = next;
      setSnapshot(next);
    };

    const scheduleLoadingRetry = () => {
      if (loadingRetryRef.current !== null || !mountedRef.current) return;
      loadingRetryRef.current = window.setTimeout(() => {
        loadingRetryRef.current = null;
        requestQuota();
      }, LOADING_RETRY_DELAY_MS);
    };

    const startPolling = () => {
      if (pollRef.current !== null || !mountedRef.current) return;
      pollRef.current = window.setInterval(requestQuota, QUOTA_POLL_INTERVAL_MS);
    };

    const applyResponse = (response: CodexQuotaResponse) => {
      if (!mountedRef.current) return;
      if (response.status === "loading") {
        if (!settledRef.current) scheduleLoadingRetry();
        return;
      }
      settledRef.current = true;
      clearLoadingRetry();
      startPolling();
      if (response.status === "ready") {
        commit(response);
        return;
      }
      if (snapshotRef.current.status === "ready") return;
      commit(response);
    };

    function requestQuota() {
      if (!mountedRef.current || requestRef.current) return;
      const controller = new AbortController();
      requestRef.current = controller;
      void client.codexQuota(controller.signal).then(applyResponse, () => {
        if (!mountedRef.current || controller.signal.aborted) return;
        settledRef.current = true;
        clearLoadingRetry();
        startPolling();
        if (snapshotRef.current.status !== "ready") commit(unavailableSnapshot());
      }).finally(() => {
        if (requestRef.current === controller) requestRef.current = null;
      });
    }

    requestQuota();

    return () => {
      mountedRef.current = false;
      clearLoadingRetry();
      clearPoll();
      requestRef.current?.abort();
      requestRef.current = null;
    };
  }, [client]);

  return snapshot;
}
