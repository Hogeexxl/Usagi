import { lazy, Suspense, useEffect, useMemo, useRef } from "react";
import { Moon, RefreshCw, Sun } from "lucide-react";

import { createRevisionFeed, type RevisionFeed } from "../data/revisionFeed";
import type { ServiceClient } from "../data/serviceClient";
import { ActionSwapIcon } from "../ui/beui/action-swap";
import { Button } from "../ui/beui/button";
import { useThemeToggle } from "../ui/beui/theme-toggle";
import { ChartSection } from "./charts/ChartSection";
import { useDashboardChartsController } from "./charts/useDashboardChartsController";
import { FilterControls } from "./FilterControls";
import { MetricGrid } from "./MetricGrid";
import { RangeSelector } from "./RangeSelector";
import { DASHBOARD_SCOPE_POLICIES, resolveDashboardScope } from "./scope";
import { ServiceButton } from "./ServiceButton";
import { SessionSection } from "./session/SessionSection";
import { createAgentDisplayLookup } from "./shared/agentDisplay";
import { useSessionDetailController } from "./session/useSessionDetailController";
import { useSessionTableController } from "./session/useSessionTableController";
import { useDashboardQuotaController } from "./useDashboardQuotaController";
import { useDashboardController, type DashboardControllerOptions } from "./useDashboardController";

const LazySessionDetailDrawer = lazy(() =>
  import("./session/SessionDetailDrawer").then(({ SessionDetailDrawer }) => ({ default: SessionDetailDrawer })),
);

function loadErrorMessage(loadState: string): string | null {
  return loadState === "error" ? "数据加载失败" : null;
}

type DashboardPageOptions = DashboardControllerOptions & { serviceClient?: ServiceClient };

export function DashboardPage({ options }: { options?: DashboardPageOptions }) {
  const feedRef = useRef<RevisionFeed | null>(null);
  if (!feedRef.current) {
    feedRef.current = createRevisionFeed({
      client: options?.client,
      eventSourceFactory: options?.eventSourceFactory,
      pollIntervalMs: options?.pollIntervalMs,
    });
  }
  const view = useDashboardController({ ...options, revisionFeed: feedRef.current });
  const quota = useDashboardQuotaController({ client: options?.client });
  const themeToggle = useThemeToggle({ variant: "circle-blur", start: "bottom-up" });
  const sessionScope = resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.sessions, view.range, view.filters);
  const sessions = useSessionTableController(sessionScope.range, sessionScope.filters, {
    client: options?.client,
    revisionFeed: feedRef.current,
    reloadKey: view.reload_key,
  });
  const detail = useSessionDetailController(sessionScope.range, sessionScope.filters, {
    client: options?.client,
    revisionFeed: feedRef.current,
    dataRevision: sessions.data_revision,
    sessionsReady: sessions.load_state === "ready",
    onStaleRevision: sessions.retry_load,
  });
  const charts = useDashboardChartsController({
    range: view.range,
    filters: view.filters,
    dataRevision: view.data_revision,
    reloadKey: view.reload_key,
    epochResetKey: view.epoch_reset_key,
    client: options?.client,
  });
  const agentDisplay = useMemo(
    () => createAgentDisplayLookup(view.filter_options?.agents),
    [view.filter_options?.agents],
  );
  const lastReloadKeyRef = useRef(view.reload_key);
  useEffect(() => {
    if (lastReloadKeyRef.current === view.reload_key) return;
    lastReloadKeyRef.current = view.reload_key;
    if (detail.open) detail.retry_detail();
  }, [detail.open, detail.retry_detail, view.reload_key]);
  useEffect(() => () => feedRef.current?.dispose(), []);

  const loading = view.load_state === "loading";
  const loadError = loadErrorMessage(view.load_state);
  const refreshAnimating = view.refreshing;

  return (
    <div className="dashboard-shell bg-background text-foreground">
      <main className="dashboard-content">
        <div className="flex flex-col gap-8">
          <header className="dashboard-header">
            <div className="flex min-w-0 items-center gap-3">
              <h1 className="text-foreground">Usagi</h1>
            </div>
            <div className="dashboard-sync-group">
              <Button
                variant="ghost"
                size="icon"
                aria-label="刷新数据"
                title="刷新数据"
                disabled={view.refreshing}
                onClick={view.request_refresh}
              >
                <RefreshCw className={`h-4 w-4${refreshAnimating ? " animate-spin" : ""}`} />
              </Button>
              <Button
                variant="ghost"
                size="icon"
                aria-label={themeToggle.mounted && themeToggle.isDark ? "Switch to light mode" : "Switch to dark mode"}
                onClick={themeToggle.toggle}
              >
                {themeToggle.mounted ? (
                  <ActionSwapIcon value={themeToggle.isDark ? "dark" : "light"} animation="blur" className="h-4 w-4">
                    {themeToggle.isDark ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
                  </ActionSwapIcon>
                ) : (
                  <span className="h-4 w-4" aria-hidden="true" />
                )}
              </Button>
              <ServiceButton client={options?.serviceClient} />
            </div>
          </header>

          <section className="dashboard-controls" aria-label="Dashboard 控制">
            <div className="dashboard-controls-row">
              <RangeSelector value={view.range} onChange={view.select_range} />
              <FilterControls
                filters={view.filters}
                options={view.filter_options}
                optionsLoading={view.filter_options_loading}
                optionsErrorCode={view.filter_options_error_code}
                anyFilterActive={view.anyFilterActive}
                onChange={view.select_filters}
                onClear={view.clear_filters}
                onRetryOptions={view.retry_filter_options}
              />
            </div>
            {loadError ? (
              <div className="mt-3 flex items-center gap-2 text-xs text-destructive" role="alert" aria-live="polite">
                <span>{loadError}</span>
                <Button variant="ghost" size="sm" onClick={view.retry_load}>重试</Button>
              </div>
            ) : null}
          </section>

          <section className="metrics-section" aria-label="关键指标" aria-busy={loading}>
            <MetricGrid
              usage={view.metrics}
              quota={quota.quota}
              onRefreshQuota={quota.refresh}
              quotaRefreshing={quota.refreshing}
              quotaRefreshError={quota.refresh_error}
              quotaRefreshAvailable={quota.refresh_available}
            />
          </section>
          <SessionSection view={sessions} detail={detail} agentDisplay={agentDisplay} />
          <ChartSection view={charts} />
        </div>
        <div className="sr-only" aria-live="polite">{loading ? "数据加载中…" : loadError}</div>
      </main>
      {detail.open || detail.selected_row ? (
        <Suspense fallback={null}>
          <LazySessionDetailDrawer view={detail} timezone={sessions.timezone} agentDisplay={agentDisplay} />
        </Suspense>
      ) : null}
    </div>
  );
}

export default DashboardPage;
