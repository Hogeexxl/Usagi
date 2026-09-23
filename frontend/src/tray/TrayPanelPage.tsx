import { ArrowUpRight, Moon, Power, RefreshCw, Sun } from "lucide-react";
import { useState } from "react";

import { serviceClient, type ServiceClient } from "../data/serviceClient";
import { ActionSwapIcon } from "../ui/beui/action-swap";
import { AnimatedToastStack, useAnimatedToastStack } from "../ui/beui/animated-toast-stack";
import { Button } from "../ui/beui/button";
import { useThemeToggle } from "../ui/beui/theme-toggle";
import { formatLastSyncTime } from "../dashboard/format";
import { AccountQuotaCard, CacheHitMetric, EstimatedCostMetric, SkeletonCard, TotalTokenMetric } from "../dashboard/MetricGrid";
import { RangeSelector } from "../dashboard/RangeSelector";
import {
  useDashboardController,
  type DashboardControllerOptions,
  type DashboardViewModel,
} from "../dashboard/useDashboardController";
import { useDashboardQuotaController, type DashboardQuotaControllerView } from "../dashboard/useDashboardQuotaController";

type WryWindow = Window & {
  ipc: { postMessage(message: string): void };
};

export type TrayPanelPageOptions = DashboardControllerOptions & {
  serviceClient?: ServiceClient;
};

export type TrayPanelViewModel = Pick<
  DashboardViewModel,
  | "range"
  | "metrics"
  | "load_state"
  | "last_scan_completed_at_ms"
  | "refresh_state"
  | "error_code"
  | "select_range"
  | "request_refresh"
  | "retry_load"
  | "retry_refresh_status"
>;

export type TrayPanelViewProps = {
  view: TrayPanelViewModel;
  quota: DashboardQuotaControllerView;
  stopping: boolean;
  onOpenDashboard: () => void;
  onStop: () => void;
};

function loadErrorMessage(loadState: TrayPanelViewModel["load_state"]): string | null {
  return loadState === "error" ? "数据加载失败" : null;
}

function refreshErrorMessage(refreshState: TrayPanelViewModel["refresh_state"], errorCode?: string): string | null {
  if (refreshState === "source_changed") return "数据源已变化";
  if (refreshState === "tracking_error") return "同步状态获取失败";
  if (refreshState !== "failed") return null;
  if (errorCode === "FORBIDDEN" || errorCode === "FORBIDDEN_HOST" || errorCode === "FORBIDDEN_ORIGIN") return "无法发起同步";
  return "同步失败";
}

export function TrayPanelView({ view, quota, stopping, onOpenDashboard, onStop }: TrayPanelViewProps) {
  const themeToggle = useThemeToggle({ variant: "circle-blur", start: "bottom-up" });
  const loadError = loadErrorMessage(view.load_state);
  const refreshError = refreshErrorMessage(view.refresh_state, view.error_code);
  const refreshEnabled =
    view.error_code !== "STATUS_NOT_READY" &&
    (view.refresh_state === "idle" || view.refresh_state === "failed");
  const refreshAnimating = view.refresh_state === "requesting" || view.refresh_state === "running";

  return (
    <div className="flex w-full flex-col gap-4 p-4">
      <div className="flex items-center gap-4">
        <Button
          variant="secondary"
          size="sm"
          onClick={onOpenDashboard}
          aria-label="打开 Dashboard"
          title="打开 Dashboard"
        >
          Dashboard
          <ArrowUpRight className="h-4 w-4" />
        </Button>
        <div className="ml-auto text-xs text-muted-foreground">上次同步：{formatLastSyncTime(view.last_scan_completed_at_ms)}</div>
        <Button
          variant="ghost"
          size="icon"
          aria-label="刷新"
          title="刷新"
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
        <Button
          variant="ghost"
          size="icon"
          aria-label="停止服务"
          title="停止服务"
          className="text-destructive hover:bg-destructive/10 hover:text-destructive"
          disabled={stopping}
          onClick={onStop}
        >
          <Power className="h-4 w-4" />
        </Button>
      </div>

      <RangeSelector
        value={view.range}
        onChange={view.select_range}
        ranges={["today", "yesterday", "7d"]}
        showCustom={false}
      />

      {loadError ? (
        <div className="flex items-center gap-4 text-xs text-destructive" role="alert" aria-live="polite">
          <span>{loadError}</span>
          <Button variant="ghost" size="sm" onClick={view.retry_load}>重试</Button>
        </div>
      ) : null}
      {refreshError ? (
        <div className="flex items-center gap-4 text-xs text-muted-foreground" aria-live="polite">
          <span>{refreshError}</span>
          {view.refresh_state === "tracking_error" ? <Button variant="ghost" size="sm" onClick={view.retry_refresh_status}>重试</Button> : null}
        </div>
      ) : null}

      <div className="grid grid-cols-[304px_304px] gap-4 [&>*]:col-span-1" aria-label={view.metrics ? "KPI 指标" : "KPI 加载中"}>
        {view.metrics === null ? (
          <SkeletonCard />
        ) : (
          <TotalTokenMetric usage={view.metrics} />
        )}
        <AccountQuotaCard
          codex={quota.codex}
          antigravity={quota.antigravity}
          onRefresh={quota.refresh}
          refreshing={quota.refreshing}
          refreshError={quota.refresh_error}
          refreshAvailable={quota.refresh_available}
          wide
          showGemini={false}
        />
        {view.metrics === null ? (
          <>
            <SkeletonCard />
            <SkeletonCard />
          </>
        ) : (
          <>
            <CacheHitMetric usage={view.metrics} glare={false} />
            <EstimatedCostMetric usage={view.metrics} glare={false} />
          </>
        )}
      </div>
    </div>
  );
}

export function TrayPanelPage({ options }: { options?: TrayPanelPageOptions }) {
  const view = useDashboardController(options);
  const quota = useDashboardQuotaController({ client: options?.client, codexOnly: true });
  const stopClient = options?.serviceClient ?? serviceClient;
  const [stopping, setStopping] = useState(false);
  const toast = useAnimatedToastStack();

  const openDashboard = () => {
    (window as unknown as WryWindow).ipc.postMessage("open-dashboard");
  };

  const stop = () => {
    if (stopping) return;
    setStopping(true);
    void stopClient.stop().catch(() => {
      setStopping(false);
      toast.showToast({ status: "error", title: "停止服务失败" });
    });
  };

  return (
    <div className="h-screen w-full overflow-hidden bg-background text-foreground">
      <TrayPanelView
        view={view}
        quota={quota}
        stopping={stopping}
        onOpenDashboard={openDashboard}
        onStop={stop}
      />
      <AnimatedToastStack
        toasts={toast.toasts}
        onDismiss={toast.dismissToast}
        placement="fixed"
      />
    </div>
  );
}

export default TrayPanelPage;
