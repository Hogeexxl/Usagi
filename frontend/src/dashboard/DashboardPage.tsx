import { lazy, Suspense, useEffect, useRef } from "react";

import { createRevisionFeed, type RevisionFeed } from "../data/revisionFeed";
import type { ServiceClient } from "../data/serviceClient";
import { ActionSwapText } from "../ui/beui/action-swap";
import { Button } from "../ui/beui/button";
import { ThemeToggle } from "../ui/beui/theme-toggle";
import { ChartSection } from "./charts/ChartSection";
import { useDashboardChartsController } from "./charts/useDashboardChartsController";
import { FilterControls } from "./FilterControls";
import { formatLastSyncTime } from "./format";
import { MetricGrid } from "./MetricGrid";
import { RangeSelector } from "./RangeSelector";
import { DASHBOARD_SCOPE_POLICIES, resolveDashboardScope } from "./scope";
import { ServiceButton } from "./ServiceButton";
import { SessionSection } from "./session/SessionSection";
import { useSessionDetailController } from "./session/useSessionDetailController";
import { useSessionTableController } from "./session/useSessionTableController";
import { SyncButton } from "./SyncButton";
import { UpdateButton } from "./UpdateButton";
import { useCodexQuotaController } from "./useCodexQuotaController";
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
  const quota = useCodexQuotaController({ client: options?.client });
  const sessionScope = resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.sessions, view.range, view.filters);
  const sessions = useSessionTableController(sessionScope.range, sessionScope.filters, { client: options?.client, revisionFeed: feedRef.current });
  const detail = useSessionDetailController(sessionScope.range, sessionScope.filters, {
    client: options?.client,
    revisionFeed: feedRef.current,
    dataRevision: sessions.data_revision,
    onStaleRevision: sessions.retry_load,
  });
  const charts = useDashboardChartsController({ range: view.range, filters: view.filters, dataRevision: view.data_revision, client: options?.client });
  useEffect(() => () => feedRef.current?.dispose(), []);

  const loading = view.load_state === "loading";
  const loadError = loadErrorMessage(view.load_state);
  const refreshError = refreshErrorMessage(view.refresh_state, view.error_code);
  const refreshEnabled = view.error_code !== "STATUS_NOT_READY" && (view.refresh_state === "idle" || view.refresh_state === "failed");
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
              <SyncButton disabled={!refreshEnabled} refreshState={view.refresh_state} lastSyncAtMs={view.last_scan_completed_at_ms} onClick={view.request_refresh} />
              <ServiceButton client={options?.serviceClient} />
              <ThemeToggle
                variant="circle-blur"
                start="bottom-up"
                className="rounded-xl border border-border bg-background p-2.5"
                iconClassName="h-5 w-5"
              />
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
            <MetricGrid usage={view.metrics} modelFilterActive={view.modelFilterActive} quota={quota} />
          </section>
          <SessionSection view={sessions} detail={detail} />
          <ChartSection view={charts} />
        </div>
        <div className="sr-only" aria-live="polite">{loading ? "数据加载中…" : loadError ?? refreshError}</div>
      </main>
      {detail.open || detail.selected_row ? (
        <Suspense fallback={null}>
          <LazySessionDetailDrawer view={detail} timezone={sessions.timezone} />
        </Suspense>
      ) : null}
    </div>
  );
}

export default DashboardPage;
