import { useEffect, useRef, useState } from "react";
import { usagiClient, dashboardQueryKey, type UsagiClient } from "../../data/usagiClient";
import { UsagiClientError, type DashboardFilters, type DashboardRange, type ModelDistributionResponse, type ProjectDistributionResponse } from "../../data/types";
import { DASHBOARD_SCOPE_POLICIES, resolveDashboardScope } from "../scope";

export type DashboardChartsView = {
  models: ModelDistributionResponse | null;
  projects: ProjectDistributionResponse | null;
  loading: boolean;
  error: boolean;
};

export function useDashboardChartsController(args: {
  range: DashboardRange;
  filters: DashboardFilters;
  dataRevision: number;
  reloadKey?: number;
  epochResetKey?: number;
  client?: UsagiClient;
}): DashboardChartsView {
  const client = args.client ?? usagiClient;
  const filterKey = dashboardQueryKey(args.range, args.filters);
  const latestScopeRef = useRef({ range: args.range, filters: args.filters });
  latestScopeRef.current = { range: args.range, filters: args.filters };
  const [view, setView] = useState<DashboardChartsView>({ models: null, projects: null, loading: true, error: false });
  const epochResetKeyRef = useRef(args.epochResetKey ?? 0);
  useEffect(() => {
    const epochReset = epochResetKeyRef.current !== (args.epochResetKey ?? 0);
    epochResetKeyRef.current = args.epochResetKey ?? 0;
    const controller = new AbortController();
    const { range, filters } = latestScopeRef.current;
    const modelScope = resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.modelDistribution, range, filters);
    const projectScope = resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.projectDistribution, range, filters);
    setView((current) => epochReset
      ? { models: null, projects: null, loading: true, error: false }
      : { ...current, loading: true, error: false });
    void Promise.all([
      client.modelDistribution(modelScope.range, modelScope.filters, controller.signal),
      client.projectDistribution(projectScope.range, projectScope.filters, controller.signal),
    ]).then(
      ([models, projects]) => {
        if (!controller.signal.aborted) setView({ models, projects, loading: false, error: false });
      },
      (error: unknown) => {
        if (!controller.signal.aborted && !(error instanceof DOMException && error.name === "AbortError")) {
          if (error instanceof UsagiClientError && error.code === "DATA_LOADING") {
            setView({ models: null, projects: null, loading: true, error: false });
            return;
          }
          setView((current) => ({ ...current, loading: false, error: true }));
        }
      },
    );
    return () => controller.abort();
  }, [client, args.dataRevision, args.epochResetKey, args.reloadKey, filterKey]);
  return view;
}
