import { lazy, Suspense, useEffect, useMemo, useRef } from "react";
import { Moon, RefreshCw, Sun } from "lucide-react";

import { createRevisionFeed, type RevisionFeed } from "../data/revisionFeed";
import type { ServiceClient } from "../data/serviceClient";
import { ActionSwapIcon, ActionSwapText } from "../ui/beui/action-swap";
import { Button } from "../ui/beui/button";
import { useThemeToggle } from "../ui/beui/theme-toggle";
import { ChartSection } from "./charts/ChartSection";
import { useDashboardChartsController } from "./charts/useDashboardChartsController";
import { FilterControls } from "./FilterControls";
import { formatLastSyncTime } from "./format";
import { MetricGrid } from "./MetricGrid";
import { RangeSelector } from "./RangeSelector";
import { DASHBOARD_SCOPE_POLICIES, resolveDashboardScope } from "./scope";
import { ServiceButton } from "./ServiceButton";
import { SessionSection } from "./session/SessionSection";
import { createSourceDisplayLookup } from "./shared/sourceDisplay";
import { useSessionDetailController } from "./session/useSessionDetailController";
import { useSessionTableController } from "./session/useSessionTableController";
import { UpdateButton } from "./UpdateButton";
import { useDashboardQuotaController } from "./useDashboardQuotaController";
import { useDashboardController, type DashboardControllerOptions } from "./useDashboardController";

const LazySessionDetailDrawer = lazy(() =>
  import("./session/SessionDetailDrawer").then(({ SessionDetailDrawer }) => ({ default: SessionDetailDrawer })),
);

function loadErrorMessage(loadState: string): string | null {
  return loadState === "error" ? "数据加载失败" : null;
}

function refreshErrorMessage(refreshState: string, errorCode?: string): string | null {
  if (refreshState === "source_changed") return "数据源已变化";
  if (refreshState === "tracking_error") return "同步状态获取失败";
  if (refreshState !== "failed") return null;
  if (errorCode === "FORBIDDEN" || errorCode === "FORBIDDEN_HOST" || errorCode === "FORBIDDEN_ORIGIN") return "无法发起同步";
  return "同步失败";
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
  const sessions = useSessionTableController(sessionScope.range, sessionScope.filters, { client: options?.client, revisionFeed: feedRef.current });
  const detail = useSessionDetailController(sessionScope.range, sessionScope.filters, {
    client: options?.client,
    revisionFeed: feedRef.current,
    dataRevision: sessions.data_revision,
    onStaleRevision: sessions.retry_load,
  });
  const charts = useDashboardChartsController({ range: view.range, filters: view.filters, dataRevision: view.data_revision, client: options?.client });
  const sourceDisplay = useMemo(
    () => createSourceDisplayLookup(view.filter_options?.sources),
    [view.filter_options?.sources],
  );
  useEffect(() => () => feedRef.current?.dispose(), []);

  const loading = view.load_state === "loading";
  const loadError = loadErrorMessage(view.load_state);
  const refreshError = refreshErrorMessage(view.refresh_state, view.error_code);
  const refreshEnabled = view.error_code !== "STATUS_NOT_READY" && (view.refresh_state === "idle" || view.refresh_state === "failed");
  const refreshAnimating = view.refresh_state === "requesting" || view.refresh_state === "running";
  const syncText = formatLastSyncTime(view.last_scan_completed_at_ms);

  return (
    <div className="dashboard-shell bg-background text-foreground">
      <main className="dashboard-content">
        <div className="flex flex-col gap-8">
          <header className="dashboard-header">
            <div className="flex min-w-0 items-center gap-3">
              <h1 className="text-foreground">Usagi</h1>
              <UpdateButton client={options?.client} />
            </div>
            <div className="dashboard-sync-group">
              <span className="flex items-center whitespace-nowrap text-sm text-muted-foreground">
                上次同步：
                <ActionSwapText key={syncText} value={syncText} animation="blur">{syncText}</ActionSwapText>
              </span>
              <Button
                variant="ghost"
                size="icon"
                aria-label="同步数据"
                title="同步数据"
                disabled={!refreshEnabled}
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
                optionsStale={view.filter_options_stale}
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
            {refreshError ? (
              <div className="mt-3 flex items-center gap-2 text-xs text-muted-foreground" aria-live="polite">
                <span>{refreshError}</span>
                {view.refresh_state === "tracking_error" ? <Button variant="ghost" size="sm" onClick={view.retry_refresh_status}>重试</Button> : null}
              </div>
            ) : null}
          </section>

          <section className="metrics-section" aria-label="关键指标" aria-busy={loading}>
            <MetricGrid
              usage={view.metrics}
              modelFilterActive={view.modelFilterActive}
              codexQuota={quota.codex}
              antigravityQuota={quota.antigravity}
              onRefreshQuota={quota.refresh}
              quotaRefreshing={quota.refreshing}
              quotaRefreshError={quota.refresh_error}
              quotaRefreshAvailable={quota.refresh_available}
            />
          </section>
          <SessionSection view={sessions} detail={detail} sourceDisplay={sourceDisplay} />
          <ChartSection view={charts} />
        </div>
        <div className="sr-only" aria-live="polite">{loading ? "数据加载中…" : loadError ?? refreshError}</div>
      </main>
      {detail.open || detail.selected_row ? (
        <Suspense fallback={null}>
          <LazySessionDetailDrawer view={detail} timezone={sessions.timezone} sourceDisplay={sourceDisplay} />
        </Suspense>
      ) : null}
    </div>
  );
}

export default DashboardPage;
