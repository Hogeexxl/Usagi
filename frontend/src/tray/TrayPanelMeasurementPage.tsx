import { useEffect, useLayoutEffect, useRef, useState } from "react";

import type {
  CodexQuotaResponse,
  CodexQuotaWindowDto,
  DashboardRange,
  SummaryUsageDto,
} from "../data/types";
import type { DashboardQuotaControllerView } from "../dashboard/useDashboardQuotaController";
import { TrayPanelView, type TrayPanelViewModel } from "./TrayPanelPage";

type Scenario = "M01" | "M02" | "M03" | "M04" | "M05" | "M06" | "M07" | "M08" | "M09";

type WryWindow = Window & {
  ipc: { postMessage(message: string): void };
};

type MeasureSuccess = {
  status: "ok";
  scenario: Scenario;
  required_height: number;
  root_height: number;
  max_visible_bottom: number;
  min_visible_left: number;
  max_visible_right: number;
};

type MeasureFatal = {
  status: "fatal";
  scenario: Scenario | null;
  error: string;
};

const SCENARIOS: readonly Scenario[] = ["M01", "M02", "M03", "M04", "M05", "M06", "M07", "M08", "M09"];

const ZERO_USAGE: SummaryUsageDto = {
  input_tokens: 0,
  cached_tokens: 0,
  cache_write_tokens: 0,
  uncached_input_tokens: 0,
  output_tokens: 0,
  reasoning_tokens: 0,
  other_output_tokens: 0,
  total_tokens: 0,
  cache_hit_rate: null,
  estimated_cost: null,
  estimated_cost_status: "unknown",
  session_count: 0,
  cost_incomplete_session_count: 0,
  complete_session_cost_per_million_tokens: null,
  session_health: {
    total_sessions: 0,
    complete_sessions: 0,
    incomplete_sessions: 0,
    error_sessions: 0,
  },
};

const LARGE_USAGE: SummaryUsageDto = {
  ...ZERO_USAGE,
  input_tokens: 8_765_432_109_876,
  cached_tokens: 7_654_321_098_765,
  output_tokens: 987_654_321_098,
  reasoning_tokens: 876_543_210_987,
  other_output_tokens: 111_111_110_111,
  total_tokens: 9_753_086_430_974,
  cache_hit_rate: 0.873,
  estimated_cost: 123_456.78,
  estimated_cost_status: "complete",
  session_count: 999_999,
  complete_session_cost_per_million_tokens: 12.3456,
  session_health: {
    total_sessions: 999_999,
    complete_sessions: 999_999,
    incomplete_sessions: 0,
    error_sessions: 0,
  },
};

const WEEKLY: CodexQuotaWindowDto = {
  used_percent: 2,
  remaining_percent: 98,
  limit_window_seconds: 604_800,
  reset_at_ms: 1_800_000_000_000,
};

const SESSION: CodexQuotaWindowDto = {
  used_percent: 11,
  remaining_percent: 89,
  limit_window_seconds: 18_000,
  reset_at_ms: 1_800_000_000_000,
};

const LONG_ACCOUNT_EMAIL = "tray-height-measurement-account-with-intentionally-long-name-0123456789@example-subdomain.example.com";
const MEASUREMENT_ACCOUNT_EMAIL = "tray-height-measurement@example.com";

const LOADING_CODEX_QUOTA: CodexQuotaResponse = {
  status: "loading",
  account_email: null,
  plan_type: null,
  session: null,
  weekly: null,
  reset_credits_available: null,
  fetched_at_ms: null,
};

const UNAVAILABLE_CODEX_QUOTA: CodexQuotaResponse = {
  status: "unavailable",
  account_email: null,
  plan_type: null,
  session: null,
  weekly: null,
  reset_credits_available: null,
  fetched_at_ms: null,
};

const UNAVAILABLE_ANTIGRAVITY_QUOTA: DashboardQuotaControllerView["antigravity"] = {
  status: "unavailable",
  account_email: null,
  plan_type: null,
  session: null,
  weekly: null,
  fetched_at_ms: null,
};

function readyQuota(session: CodexQuotaWindowDto | null, accountEmail = MEASUREMENT_ACCOUNT_EMAIL): CodexQuotaResponse {
  return {
    status: "ready",
    account_email: accountEmail,
    plan_type: "pro",
    session,
    weekly: WEEKLY,
    reset_credits_available: 999,
    fetched_at_ms: 1_800_000_000_000,
  };
}

function quotaFor(codex: CodexQuotaResponse): DashboardQuotaControllerView {
  return {
    codex,
    antigravity: UNAVAILABLE_ANTIGRAVITY_QUOTA,
    refreshing: false,
    refresh_error: false,
    refresh_available: true,
    refresh: noop,
  };
}

const noopRange = (_range: DashboardRange) => undefined;
const noop = () => undefined;

function viewFor(
  metrics: SummaryUsageDto | null,
  loadState: TrayPanelViewModel["load_state"],
  refreshState: TrayPanelViewModel["refresh_state"],
  errorCode?: string,
): TrayPanelViewModel {
  return {
    range: { key: "today" },
    metrics,
    load_state: loadState,
    last_scan_completed_at_ms: null,
    refresh_state: refreshState,
    error_code: errorCode,
    select_range: noopRange,
    request_refresh: noop,
    retry_load: noop,
    retry_refresh_status: noop,
  };
}

const SPECIMENS: Record<Scenario, { view: TrayPanelViewModel; quota: DashboardQuotaControllerView }> = {
  M01: {
    view: viewFor(null, "loading", "idle"),
    quota: quotaFor(LOADING_CODEX_QUOTA),
  },
  M02: {
    view: viewFor(ZERO_USAGE, "ready", "idle"),
    quota: quotaFor(UNAVAILABLE_CODEX_QUOTA),
  },
  M03: {
    view: viewFor(null, "error", "idle", "HTTP_ERROR"),
    quota: quotaFor(readyQuota(null)),
  },
  M04: {
    view: viewFor(ZERO_USAGE, "ready", "tracking_error", "HTTP_ERROR"),
    quota: quotaFor(readyQuota(null)),
  },
  M05: {
    view: viewFor(null, "error", "tracking_error", "HTTP_ERROR"),
    quota: quotaFor(readyQuota(SESSION)),
  },
  M06: {
    view: viewFor(ZERO_USAGE, "ready", "source_changed", "SOURCE_CHANGED"),
    quota: quotaFor(readyQuota(SESSION)),
  },
  M07: {
    view: viewFor(ZERO_USAGE, "ready", "failed", "HTTP_ERROR"),
    quota: quotaFor(readyQuota(SESSION)),
  },
  M08: {
    view: viewFor(LARGE_USAGE, "ready", "idle"),
    quota: quotaFor(readyQuota(SESSION)),
  },
  M09: {
    view: viewFor(ZERO_USAGE, "ready", "idle"),
    quota: quotaFor(readyQuota(SESSION, LONG_ACCOUNT_EMAIL)),
  },
};

function nextFrame(): Promise<void> {
  return new Promise((resolve) => window.requestAnimationFrame(() => resolve()));
}

function isFiniteAnimation(animation: Animation): boolean {
  const timing = animation.effect?.getComputedTiming();
  return timing !== undefined && typeof timing.endTime === "number" && Number.isFinite(timing.endTime);
}

async function waitForAnimations(): Promise<void> {
  const deadline = performance.now() + 3_000;
  for (;;) {
    const active = document
      .getAnimations()
      .filter(isFiniteAnimation)
      .filter((animation) => animation.playState !== "finished" && animation.playState !== "idle");
    if (active.length === 0) return;

    const remaining = deadline - performance.now();
    if (remaining <= 0) throw new Error("animations did not stabilize within 3 seconds");

    let timeout: number | undefined;
    const timeoutPromise = new Promise<void>((resolve) => {
      timeout = window.setTimeout(resolve, remaining);
    });
    await Promise.race([Promise.all(active.map((animation) => animation.finished)).then(() => undefined), timeoutPromise]);
    if (timeout !== undefined) window.clearTimeout(timeout);
    if (performance.now() >= deadline) {
      const unsettled = document
        .getAnimations()
        .filter(isFiniteAnimation)
        .some((animation) => animation.playState !== "finished" && animation.playState !== "idle");
      if (unsettled) throw new Error("animations did not stabilize within 3 seconds");
    }
  }
}

async function waitForStableLayout(): Promise<void> {
  if (document.fonts?.ready) await document.fonts.ready;
  await nextFrame();
  await nextFrame();
  await waitForAnimations();
}

async function waitFor(predicate: () => boolean, message: string): Promise<void> {
  const deadline = performance.now() + 3_000;
  while (!predicate()) {
    if (performance.now() >= deadline) throw new Error(message);
    await nextFrame();
  }
}

function m09ExpandedStack(): HTMLElement {
  const stack = document.querySelector<HTMLElement>('[role="group"][aria-label="账户额度卡片"]');
  if (!stack || stack.getAttribute("aria-expanded") !== "true") {
    throw new Error("M09 account quota stack is not expanded");
  }

  const labels = ["Codex·5H", "Codex·Weekly"];
  for (const label of labels) {
    const labelElement = Array.from(stack.querySelectorAll<HTMLElement>("*")).find(
      (element) => element.textContent?.trim() === label,
    );
    const card = labelElement?.closest<HTMLElement>(".absolute");
    if (!card || card.getAttribute("aria-hidden") !== "false" || getComputedStyle(card).visibility === "hidden") {
      throw new Error(`M09 quota card is not visible: ${label}`);
    }
  }
  if (stack.querySelector('[aria-label*="Gemini"]')) throw new Error("M09 unexpectedly rendered a Gemini card");
  return stack;
}

function m09LastCodexTrigger(): HTMLButtonElement {
  const stack = m09ExpandedStack();
  const triggers = Array.from(
    stack.querySelectorAll<HTMLButtonElement>('button[aria-label="Codex 计划：Pro 20x"]'),
  );
  if (triggers.length !== 2) throw new Error(`M09 expected two Codex plan badges, got ${triggers.length}`);

  const lastTrigger = triggers[triggers.length - 1];
  const weeklyLabel = Array.from(stack.querySelectorAll<HTMLElement>("*")).find(
    (element) => element.textContent?.trim() === "Codex·Weekly",
  );
  if (!weeklyLabel || lastTrigger.closest(".absolute") !== weeklyLabel.closest(".absolute")) {
    throw new Error("M09 last Codex badge is not on the Codex weekly card");
  }
  return lastTrigger;
}

async function openM09Popover(): Promise<void> {
  await waitFor(
    () => document.querySelector<HTMLElement>('[role="group"][aria-label="账户额度卡片"]') !== null,
    "M09 account quota stack missing",
  );
  const stack = document.querySelector<HTMLElement>('[role="group"][aria-label="账户额度卡片"]');
  if (!stack) throw new Error("M09 account quota stack missing");
  stack.dispatchEvent(
    new PointerEvent("pointerover", {
      bubbles: true,
      pointerId: 1,
      pointerType: "mouse",
      buttons: 0,
    }),
  );
  await waitFor(() => {
    try {
      m09ExpandedStack();
      return true;
    } catch {
      return false;
    }
  }, "M09 two-card Codex quota stack did not expand");
  await waitForStableLayout();

  const trigger = m09LastCodexTrigger();
  const hoverRoot = trigger?.parentElement;
  if (!trigger || !hoverRoot) throw new Error("M09 quota hover root missing");
  hoverRoot.dispatchEvent(
    new PointerEvent("pointerover", {
      bubbles: true,
      pointerId: 1,
      pointerType: "mouse",
      buttons: 0,
    }),
  );
  await waitFor(() => {
    const expanded = trigger.getAttribute("aria-expanded") === "true";
    const emailVisible = Array.from(document.querySelectorAll<HTMLElement>("[data-popover-portal]"))
      .some((portal) => portal.textContent?.includes(LONG_ACCOUNT_EMAIL));
    return expanded && emailVisible;
  }, "M09 Codex weekly popover did not open");
}

function measurementElements(root: HTMLElement, scenario: Scenario): HTMLElement[] {
  const elements = [root, ...Array.from(root.querySelectorAll<HTMLElement>("*"))];
  if (scenario !== "M09") return elements;

  const trigger = m09LastCodexTrigger();
  if (trigger.getAttribute("aria-expanded") !== "true") throw new Error("M09 Codex weekly trigger is not expanded");
  const contentId = trigger.getAttribute("aria-controls");
  if (!contentId) throw new Error("M09 quota popover controls missing");
  const content = document.getElementById(contentId);
  if (!content) throw new Error("M09 quota popover content missing");
  const portal = content.closest<HTMLElement>("[data-popover-portal]");
  if (!portal || portal.hasAttribute("inert") || portal.closest("[inert]")) throw new Error("M09 quota popover portal is inert or missing");
  if (!portal.textContent?.includes(LONG_ACCOUNT_EMAIL)) throw new Error("M09 Codex account email is missing from the portal");
  elements.push(portal, ...Array.from(portal.querySelectorAll<HTMLElement>("*")));
  return elements;
}

function measure(scenario: Scenario): MeasureSuccess {
  const roots = document.querySelectorAll<HTMLElement>("[data-tray-measure-root]");
  if (roots.length !== 1) throw new Error("measurement root missing or not unique");
  const root = roots[0];
  const rootRect = root.getBoundingClientRect();
  if (rootRect.width !== 656) throw new Error(`measurement root width is ${rootRect.width}, expected 656`);

  let minVisibleLeft = 0;
  let maxVisibleRight = 656;
  let maxVisibleBottom = rootRect.height;
  for (const element of measurementElements(root, scenario)) {
    const style = getComputedStyle(element);
    if (style.display === "none" || style.visibility === "hidden" || style.visibility === "collapse") continue;
    if (element.closest("[inert]")) continue;
    const rect = element.getBoundingClientRect();
    if (![rect.left, rect.right, rect.bottom, rect.width, rect.height].every(Number.isFinite)) {
      throw new Error("measurement element has a non-finite rect");
    }
    if (rect.width === 0 && rect.height === 0) continue;
    if (rect.top < 0 || rect.bottom > window.innerHeight) {
      throw new Error("measurement element overflows the measurement window vertically");
    }
    const left = rect.left - rootRect.left;
    const right = rect.right - rootRect.left;
    const bottom = rect.bottom - rootRect.top;
    if (left < 0 || right > 656) throw new Error("measurement element overflows the 656px viewport");
    minVisibleLeft = Math.min(minVisibleLeft, left);
    maxVisibleRight = Math.max(maxVisibleRight, right);
    maxVisibleBottom = Math.max(maxVisibleBottom, bottom);
  }

  return {
    status: "ok",
    scenario,
    required_height: Math.ceil(Math.max(rootRect.height, maxVisibleBottom)),
    root_height: rootRect.height,
    max_visible_bottom: maxVisibleBottom,
    min_visible_left: minVisibleLeft,
    max_visible_right: maxVisibleRight,
  };
}

function postMeasurement(result: MeasureSuccess | MeasureFatal): void {
  (window as unknown as WryWindow).ipc.postMessage(`tray-measure-result:${JSON.stringify(result)}`);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function TrayPanelMeasurementPage() {
  const [scenario, setScenario] = useState<Scenario>(SCENARIOS[0]);
  const committedScenarioRef = useRef<Scenario>(SCENARIOS[0]);

  useLayoutEffect(() => {
    committedScenarioRef.current = scenario;
  }, [scenario]);

  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      for (const current of SCENARIOS) {
        if (cancelled) return;
        setScenario(current);
        try {
          await waitFor(
            () => committedScenarioRef.current === current,
            `${current} DOM commit did not complete`,
          );
          if (current === "M09") await openM09Popover();
          await waitForStableLayout();
          const result = measure(current);
          postMeasurement(result);
        } catch (error) {
          if (!cancelled) {
            postMeasurement({ status: "fatal", scenario: current, error: errorMessage(error) });
          }
          return;
        }
      }
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, []);

  const specimen = SPECIMENS[scenario];
  return (
    <div data-tray-measure-root style={{ width: "656px" }}>
      <TrayPanelView
        view={specimen.view}
        quota={specimen.quota}
        stopping={false}
        onOpenDashboard={noop}
        onStop={noop}
      />
    </div>
  );
}

export default TrayPanelMeasurementPage;
