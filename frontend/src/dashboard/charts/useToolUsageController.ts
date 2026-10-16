import { useCallback, useEffect, useMemo, useState } from "react";

import { usagiClient, type UsagiClient } from "../../data/usagiClient";
import type { DashboardFilters, ToolUsageResponse } from "../../data/types";
import { DASHBOARD_SCOPE_POLICIES, resolveDashboardScope } from "../scope";

export type ToolUsageRange = "today" | "7d" | "30d";

export type ToolUsageControllerView = {
  range: ToolUsageRange;
  response: ToolUsageResponse | null;
  loading: boolean;
  error: boolean;
  select_range: (range: ToolUsageRange) => void;
};

const RANGE_DAYS: Record<ToolUsageRange, 1 | 7 | 30> = { today: 1, "7d": 7, "30d": 30 };
const TOOL_POLL_INTERVAL_MS = 60_000;
const INITIAL_TOOL_RETRY_INTERVAL_MS = 5_000;
const EMPTY_FILTERS: DashboardFilters = { agents: [], models: [], projects: [] };

export function useToolUsageController(options: { client?: UsagiClient } = {}): ToolUsageControllerView {
  const client = options.client ?? usagiClient;
  const [range, setRange] = useState<ToolUsageRange>("today");
  const [view, setView] = useState<Omit<ToolUsageControllerView, "range" | "select_range">>({
    response: null,
    loading: true,
    error: false,
  });
  const scopePolicy = useMemo(() => ({
    ...DASHBOARD_SCOPE_POLICIES.toolUsage,
    range: { kind: "fixed" as const, range: { key: range } },
  }), [range]);
  const days = RANGE_DAYS[resolveDashboardScope(scopePolicy, { key: "today" }, EMPTY_FILTERS).range.key as ToolUsageRange];

  useEffect(() => {
    let active = true;
    let request: AbortController | null = null;
    let timer: number | null = null;
    let hasLoaded = false;
    let lastFailed = false;

    const load = () => {
      if (!active || request) return;
      const controller = new AbortController();
      request = controller;
      setView((current) => ({
        response: current.response?.days === days ? current.response : null,
        loading: !lastFailed,
        error: lastFailed,
      }));
      void client.toolUsage(days, controller.signal).then(
        (response) => {
          if (!active || controller.signal.aborted) return;
          hasLoaded = true;
          lastFailed = false;
          setView({ response, loading: false, error: false });
        },
        (error: unknown) => {
          if (!active || controller.signal.aborted || error instanceof DOMException && error.name === "AbortError") return;
          lastFailed = true;
          setView((current) => ({ ...current, loading: false, error: true }));
        },
      ).finally(() => {
        if (request !== controller) return;
        request = null;
        if (!active) return;
        timer = window.setTimeout(() => {
          timer = null;
          load();
        }, hasLoaded ? TOOL_POLL_INTERVAL_MS : INITIAL_TOOL_RETRY_INTERVAL_MS);
      });
    };

    load();
    return () => {
      active = false;
      if (timer !== null) window.clearTimeout(timer);
      timer = null;
      request?.abort();
      request = null;
    };
  }, [client, days]);

  const selectRange = useCallback((next: ToolUsageRange) => setRange(next), []);
  return { range, ...view, select_range: selectRange };
}
