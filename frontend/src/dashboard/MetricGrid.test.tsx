import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ComponentProps } from "react";

import type { SummaryUsageDto } from "../data/types";
import { chartMuted, chartSeriesColor } from "./charts/chartPalette";
import type { AntigravityQuotaResponse, CodexQuotaResponse } from "../data/types";
import { Popover, PopoverContent, PopoverTrigger } from "../ui/beui/popover";
import { CacheHitMetric, codexQuotaColor, EstimatedCostMetric, MetricGrid } from "./MetricGrid";
import { formatCodexPlanType, formatCodexResetTime } from "./format";

const usage: SummaryUsageDto = {
  input_tokens: 1_500,
  cached_tokens: 600,
  cache_write_tokens: null,
  uncached_input_tokens: null,
  output_tokens: 500,
  reasoning_tokens: 125,
  other_output_tokens: 375,
  total_tokens: 2_000,
  cache_hit_rate: 0.4,
  estimated_cost: 1_240,
  estimated_cost_status: "partial",
  session_count: 4,
  cost_incomplete_session_count: 1,
  complete_session_cost_per_million_tokens: 6.2354,
  session_health: {
    total_sessions: 5,
    complete_sessions: 4,
    incomplete_sessions: 1,
    error_sessions: 0,
  },
};

const compactUsage: SummaryUsageDto = {
  ...usage,
  input_tokens: 12_200_000,
  cached_tokens: 8_700_000,
  output_tokens: 6_200_000,
  reasoning_tokens: 2_100_000,
  other_output_tokens: 4_100_000,
  total_tokens: 18_400_000,
  cache_hit_rate: 8_700_000 / 12_200_000,
};

const TOKEN_BAR_LABEL = "输入与输出 Token 构成；推理 Token 包含在输出 Token 中";

const readyQuota: CodexQuotaResponse = {
  status: "ready",
  account_email: "hoge@example.com",
  plan_type: "prolite",
  session: null,
  weekly: {
    used_percent: 55,
    remaining_percent: 45,
    limit_window_seconds: 604800,
    reset_at_ms: Date.UTC(2026, 7, 12, 4, 23),
  },
  reset_credits_available: 2,
  fetched_at_ms: Date.UTC(2026, 7, 1),
};

const dualQuota: CodexQuotaResponse = {
  ...readyQuota,
  session: {
    used_percent: 12,
    remaining_percent: 88,
    limit_window_seconds: 18000,
    reset_at_ms: Date.UTC(2026, 7, 2, 9, 10),
  },
};

const geminiQuota: AntigravityQuotaResponse = {
  status: "ready",
  account_email: "gemini@example.com",
  plan_type: "google_ai_pro",
  session: {
    used_percent: 20,
    remaining_percent: 80,
    limit_window_seconds: 18000,
    reset_at_ms: Date.UTC(2026, 7, 2, 9, 10),
  },
  weekly: {
    used_percent: 65,
    remaining_percent: 35,
    limit_window_seconds: 604800,
    reset_at_ms: Date.UTC(2026, 7, 12, 4, 23),
  },
  fetched_at_ms: Date.UTC(2026, 7, 1),
};

const unavailableCodexQuota: CodexQuotaResponse = {
  status: "unavailable",
  account_email: null,
  plan_type: null,
  session: null,
  weekly: null,
  reset_credits_available: null,
  fetched_at_ms: null,
};

const unavailableAntigravityQuota: AntigravityQuotaResponse = {
  status: "unavailable",
  account_email: null,
  plan_type: null,
  session: null,
  weekly: null,
  fetched_at_ms: null,
};

function MetricGridFixture({
  usage: currentUsage = usage,
  modelFilterActive = false,
  codexQuota = unavailableCodexQuota,
  antigravityQuota = unavailableAntigravityQuota,
  onRefreshQuota = () => undefined,
  quotaRefreshing = false,
  quotaRefreshError = false,
  quotaRefreshAvailable = false,
}: Partial<ComponentProps<typeof MetricGrid>>) {
  return (
    <MetricGrid
      usage={currentUsage}
      modelFilterActive={modelFilterActive}
      codexQuota={codexQuota}
      antigravityQuota={antigravityQuota}
      onRefreshQuota={onRefreshQuota}
      quotaRefreshing={quotaRefreshing}
      quotaRefreshError={quotaRefreshError}
      quotaRefreshAvailable={quotaRefreshAvailable}
    />
  );
}

function cardByTitle(title: string): HTMLElement {
  if (title === "账户额度") return screen.getByRole("group", { name: "账户额度" });
  const card = screen.getByText(title).closest(".h-36");
  if (!card) throw new Error(`Metric card not found: ${title}`);
  return card as HTMLElement;
}

function segmentByClass(bar: HTMLElement, className: string): HTMLElement {
  const segment = Array.from(bar.children).find((child) => child.classList.contains(className));
  if (!segment) throw new Error(`Bar segment not found: ${className}`);
  return segment as HTMLElement;
}

function widths(segments: HTMLElement[]): string[] {
  return segments.map((segment) => segment.style.width);
}

function enableTiltEffects() {
  vi.spyOn(window, "matchMedia").mockImplementation((query) => ({
    matches: query.includes("(hover: hover) and (pointer: fine)"),
    media: query,
    onchange: null,
    addListener: () => undefined,
    removeListener: () => undefined,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    dispatchEvent: () => false,
  }));
}

function glareOverlayCount(card: HTMLElement): number {
  return Array.from(card.children).filter((child) =>
    child.classList.contains("pointer-events-none") &&
    child.classList.contains("absolute") &&
    child.classList.contains("inset-0") &&
    child.classList.contains("opacity-15"),
  ).length;
}

describe("MetricGrid v0.2.1", () => {
  it("exports the shared tray metric components without changing dashboard behavior", () => {
    expect(CacheHitMetric).toEqual(expect.any(Function));
    expect(EstimatedCostMetric).toEqual(expect.any(Function));
  });

  it("keeps glare only on the total token card in the dashboard grid", async () => {
    enableTiltEffects();
    render(<MetricGridFixture usage={usage} modelFilterActive={false} codexQuota={readyQuota} />);

    const grid = screen.getByLabelText("KPI 指标");
    await waitFor(() => {
      expect(Array.from(grid.children).map((card) => glareOverlayCount(card as HTMLElement))).toEqual([1, 0, 0, 0, 0]);
    });
  });

  it("keeps the default Popover theme contract when inverseTheme is omitted", async () => {
    render(
      <Popover defaultOpen>
        <PopoverTrigger>
          <button type="button">default</button>
        </PopoverTrigger>
        <PopoverContent>Default content</PopoverContent>
      </Popover>,
    );

    const dialog = await screen.findByRole("dialog");
    const portal = dialog.closest("[data-popover-portal]");
    expect(portal).not.toBeNull();
    expect(portal?.querySelectorAll(".bg-popover")).toHaveLength(2);
    expect(portal?.querySelectorAll(".bg-primary")).toHaveLength(0);
    expect(dialog).toHaveClass("text-popover-foreground");
    expect(dialog).not.toHaveClass("text-primary-foreground");
  });

  it("[T-S03-001] renders five KPI cards and all required titles without a model filter", () => {
    render(<MetricGridFixture usage={usage} modelFilterActive={false} codexQuota={readyQuota} />);

    const grid = screen.getByLabelText("KPI 指标");
    expect(grid.children).toHaveLength(5);
    for (const title of ["总 Token", "缓存命中", "会话数量", "预估费用", "账户额度"]) {
      expect(within(grid).getByText(title)).toBeInTheDocument();
    }
  });

  it("[T-S03-001] hides only Session Count while a model filter is active", () => {
    render(<MetricGridFixture usage={usage} modelFilterActive codexQuota={readyQuota} />);

    const grid = screen.getByLabelText("KPI 指标");
    expect(grid.children).toHaveLength(4);
    expect(within(grid).queryByText("会话数量")).not.toBeInTheDocument();
    for (const title of ["总 Token", "缓存命中", "预估费用", "账户额度"]) {
      expect(within(grid).getByText(title)).toBeInTheDocument();
    }
  });

  it("[T-S03-002] keeps reasoning nested in output with fixed token-bar geometry", () => {
    render(<MetricGridFixture usage={usage} modelFilterActive={false} />);

    const bar = screen.getByLabelText(TOKEN_BAR_LABEL) as HTMLElement;
    const input = segmentByClass(bar, "bg-[#68c0e8]");
    const output = segmentByClass(bar, "bg-[#be753e]");
    const reasoning = segmentByClass(bar, "bg-[#a6333d]");
    expect(bar.children).toHaveLength(3);

    expect(input).toHaveStyle({ width: "75%" });
    expect(output).toHaveStyle({ left: "75%", width: "25%" });
    expect(reasoning).toHaveStyle({ width: "6.25%" });
    expect(reasoning).toHaveClass("absolute", "right-0");
    expect(reasoning).not.toHaveClass("left-0");
    expect(screen.getByRole("button", { name: /推理 125，包含在输出 Token 中/ })).toBeInTheDocument();

    const inputPct = Number.parseFloat(input.style.width);
    const outputPct = Number.parseFloat(output.style.width);
    const reasoningPct = Number.parseFloat(reasoning.style.width);
    expect(inputPct + outputPct).toBeCloseTo(100);
    expect(inputPct + outputPct + reasoningPct).not.toBeCloseTo(100);

    const before = widths([input, output, reasoning]);
    for (const legend of within(bar.parentElement as HTMLElement).getAllByRole("button")) {
      fireEvent.focus(legend);
      expect(widths([input, output, reasoning])).toEqual(before);
      fireEvent.blur(legend);
    }
  });

  it("[T-S03-003] renders cache and remaining geometry with two interactive dotted legends", () => {
    render(<MetricGridFixture usage={usage} modelFilterActive={false} />);

    const card = cardByTitle("缓存命中");
    const cached = card.querySelector(".bg-\\[\\#be506e\\]")?.parentElement as HTMLElement;
    const remaining = cached.children[1] as HTMLElement;
    expect(cached.children).toHaveLength(2);
    expect((cached.children[0] as HTMLElement).style.width).toBe("40%");
    expect(remaining.style.width).toBe("60%");
    expect(remaining).toHaveClass("bg-[#4057a5]");
    expect(screen.getByTitle("40.0%")).toBeInTheDocument();

    const legends = within(card).getAllByRole("button");
    expect(legends).toHaveLength(2);
    expect(legends[0]).toHaveAttribute("type", "button");
    expect(legends[0]).toHaveTextContent("缓存读");
    expect(legends[1]).toHaveTextContent("输入");
    expect(legends[0].firstElementChild).toHaveAttribute("aria-hidden", "true");
    expect(legends[1].firstElementChild).toHaveAttribute("aria-hidden", "true");
    expect(legends[0]).toHaveClass("h-4", "gap-1");
    expect(legends[1]).toHaveClass("h-4", "gap-1");
    expect(legends[0].firstElementChild).toHaveClass("h-4", "shrink-0", "items-center", "justify-center");
    expect(legends[1].firstElementChild).toHaveClass("h-4", "shrink-0", "items-center", "justify-center");
    expect(legends[0].firstElementChild).not.toHaveClass("w-4");
    expect(legends[1].firstElementChild).not.toHaveClass("w-4");
    expect(legends[0].firstElementChild?.firstElementChild).toHaveClass("h-1.5", "w-1.5", "bg-[#be506e]");
    expect(legends[1].firstElementChild?.firstElementChild).toHaveClass("h-1.5", "w-1.5", "bg-[#4057a5]");
    expect(within(card).getByText("缓存读")).toHaveClass("h-4", "items-center");
    expect(within(card).getByText("输入")).toHaveClass("h-4", "items-center");

    const before = widths(Array.from(cached.children) as HTMLElement[]);
    for (const legend of legends) {
      fireEvent.focus(legend);
      expect(widths(Array.from(cached.children) as HTMLElement[])).toEqual(before);
      fireEvent.blur(legend);
    }
  });

  it("[T-S03-005] always shows neutral completeness info and MToken cost for complete pricing", async () => {
    render(
      <MetricGridFixture
        usage={{
          ...usage,
          estimated_cost_status: "complete",
          cost_incomplete_session_count: 0,
          complete_session_cost_per_million_tokens: 12.3456,
        }}
        modelFilterActive={false}
      />,
    );

    const card = cardByTitle("预估费用");
    expect(within(card).getByText("$12.35 / MToken")).toBeInTheDocument();
    const trigger = within(card).getByRole("button", { name: "预估费用完整性提示" });
    expect(trigger).toHaveClass("text-foreground");
    expect(trigger).not.toHaveClass("text-warning");
    fireEvent.pointerEnter(trigger.parentElement!, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("5/5 个会话完整计价");
    expect(dialog).not.toHaveTextContent("计价不完整");
    const portal = dialog.closest("[data-popover-portal]");
    expect(portal).not.toBeNull();
    expect(portal?.querySelectorAll(".bg-primary")).toHaveLength(2);
    expect(portal?.querySelectorAll(".bg-popover")).toHaveLength(0);
    expect(dialog).toHaveClass("text-primary-foreground");
    expect(dialog).not.toHaveClass("text-popover-foreground");
  });

  it("[T-S03-005] keeps known partial cost, warns, and reports unified completeness copy", async () => {
    render(<MetricGridFixture usage={usage} modelFilterActive={false} />);

    const card = cardByTitle("预估费用");
    const cost = screen.getByTitle("$1,240.00");
    expect(cost).toHaveTextContent("$1.24K");
    expect(within(card).getByText("$6.235 / MToken")).toBeInTheDocument();

    const trigger = within(card).getByRole("button", { name: "预估费用完整性提示" });
    expect(trigger).toHaveClass("text-warning");
    expect(trigger).not.toHaveClass("text-destructive");
    fireEvent.pointerEnter(trigger.parentElement!, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("4/5 个会话完整计价");
    expect(dialog).toHaveTextContent("1 个会话计价不完整");
  });

  it("[T-S03-005] shows an unknown-cost dash and no MToken value when no session is fully priced", async () => {
    render(
      <MetricGridFixture
        usage={{
          ...usage,
          estimated_cost: null,
          estimated_cost_status: "unknown",
          cost_incomplete_session_count: 5,
          complete_session_cost_per_million_tokens: null,
        }}
        modelFilterActive={false}
      />,
    );

    const card = cardByTitle("预估费用");
    expect(within(card).getByText("—")).toBeInTheDocument();
    expect(within(card).getByText("— / MToken")).toBeInTheDocument();
    const trigger = within(card).getByRole("button", { name: "预估费用完整性提示" });
    expect(trigger).toHaveClass("text-warning");
    fireEvent.pointerEnter(trigger.parentElement!, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("0/5 个会话完整计价");
    expect(dialog).toHaveTextContent("5 个会话计价不完整");
  });

  it("[T-S03-006] exposes compact token and cost values with complete original aria/title values", () => {
    render(<MetricGridFixture usage={compactUsage} modelFilterActive={false} />);

    const token = screen.getByTitle("18,400,000");
    expect(token).toHaveAttribute("aria-label", "18,400,000");
    expect(token).toHaveTextContent("18.4M");

    const cost = screen.getByTitle("$1,240.00");
    expect(cost).toHaveAttribute("aria-label", "$1,240.00");
    expect(cost).toHaveTextContent("$1.24K");
  });

  it("T-Q-005 formats known and unknown plan types", () => {
    expect(formatCodexPlanType("prolite")).toBe("Pro 5x");
    expect(formatCodexPlanType("pro")).toBe("Pro 20x");
    expect(formatCodexPlanType("plus")).toBe("Plus");
    expect(formatCodexPlanType("team_plan")).toBe("Team Plan");
    expect(formatCodexPlanType(null)).toBe("—");
  });

  it("renders a compact quota card with reset details, plan Popover, and the shared palette", async () => {
    render(<MetricGridFixture usage={usage} modelFilterActive={false} codexQuota={readyQuota} />);

    const card = cardByTitle("账户额度");
    const stack = within(card).getByRole("group", { name: "账户额度卡片" });
    const panel = card.firstElementChild as HTMLElement;
    expect(card).toHaveClass("h-[144px]", "w-[236px]");
    expect(panel).toHaveClass("min-h-[144px]", "p-[10px]", "rounded-3xl");
    expect(within(card).getByText("账户额度")).toBeInTheDocument();
    expect(within(card).getByText("账户额度")).toHaveClass("pl-[2px]");
    expect(within(card).getByRole("button", { name: "刷新账户额度" })).toBeDisabled();
    expect(stack).toHaveClass("h-[100px]", "w-[216px]");
    expect(stack.children).toHaveLength(1);
    const inner = stack.firstElementChild as HTMLElement;
    expect(inner).toHaveClass("h-[92px]", "w-full", "p-[10px]");
    expect(within(inner).getByText("Codex·Weekly")).toBeInTheDocument();
    expect(within(inner).getByLabelText("45%")).toBeInTheDocument();
    const reset = within(inner).getByText(`下次重置：${formatCodexResetTime(readyQuota.weekly!.reset_at_ms)}`);
    expect(reset).toBeInTheDocument();
    expect(reset).toHaveClass("text-xs", "leading-4");

    const bar = within(inner).getByLabelText("剩余与已使用配额");
    expect(bar).toHaveClass("h-[5px]");
    expect(bar.children).toHaveLength(2);
    expect((bar.children[0] as HTMLElement).style.width).toBe("45%");
    expect((bar.children[0] as HTMLElement).style.backgroundColor).toBe(chartSeriesColor(5));
    expect((bar.children[1] as HTMLElement).style.backgroundColor).toBe(chartMuted);
    const planTrigger = within(inner).getByRole("button", { name: "Codex 计划：Pro 5x" });
    expect(planTrigger.parentElement?.parentElement).toHaveTextContent(`下次重置：${formatCodexResetTime(readyQuota.weekly!.reset_at_ms)}`);
    expect(planTrigger).toHaveClass("h-4", "rounded-full", "border", "border-foreground/40", "whitespace-nowrap");
    expect(card.querySelector(".size-7")).toBeNull();

    expect(codexQuotaColor(60)).toBe(chartSeriesColor(8));
    expect(codexQuotaColor(45)).toBe(chartSeriesColor(5));
    expect(codexQuotaColor(20)).toBe(chartSeriesColor(5));
    expect(codexQuotaColor(19)).toBe(chartSeriesColor(9));

    fireEvent.pointerEnter(planTrigger.parentElement!, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    const dialog = (await screen.findByText("hoge@example.com")).closest('[role="dialog"]');
    expect(dialog).toHaveTextContent("hoge@example.com");
    expect(dialog).toHaveTextContent("重置卡：2 次");
    const portal = dialog?.closest("[data-popover-portal]");
    expect(portal).not.toBeNull();
    expect(portal?.querySelectorAll(".bg-primary")).toHaveLength(2);
    expect(portal?.querySelectorAll(".bg-popover")).toHaveLength(0);
    expect(dialog).toHaveClass("text-primary-foreground");
    expect(dialog).not.toHaveClass("text-popover-foreground");
  });

  it("stacks quota windows in provider order and gives every Codex and Gemini card a plan Popover", async () => {
    render(<MetricGridFixture usage={usage} modelFilterActive={false} codexQuota={dualQuota} antigravityQuota={geminiQuota} />);

    const card = cardByTitle("账户额度");
    const stack = within(card).getByRole("group", { name: "账户额度卡片" });
    const labels = Array.from(stack.children, (entry) => {
      const title = entry.firstElementChild?.firstElementChild?.firstElementChild;
      return title?.textContent;
    });
    expect(labels).toEqual(["Codex·5H", "Codex·Weekly", "Gemini·5H", "Gemini·Weekly"]);
    const entries = Array.from(stack.children) as HTMLElement[];
    for (const entry of entries) expect(entry).toHaveClass("h-[92px]", "w-full", "p-[10px]");

    fireEvent.pointerEnter(card, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    expect(stack).toHaveAttribute("aria-expanded", "true");
    fireEvent.pointerLeave(card, { pointerId: 1, pointerType: "mouse", buttons: 0, relatedTarget: entries[3] });
    expect(stack).toHaveAttribute("aria-expanded", "true");
    fireEvent.pointerLeave(card, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    expect(stack).toHaveAttribute("aria-expanded", "false");
    expect(entries.slice(0, 2).every((entry) => entry.style.visibility !== "hidden")).toBe(true);
    expect(entries.slice(2).every((entry) => entry.style.visibility === "hidden")).toBe(true);

    fireEvent.pointerEnter(card, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    expect(stack).toHaveAttribute("aria-expanded", "true");
    expect(entries.every((entry) => entry.style.visibility !== "hidden")).toBe(true);
    expect(within(stack).getAllByRole("button", { name: "Codex 计划：Pro 5x" })).toHaveLength(2);
    const geminiBadges = within(stack).getAllByRole("button", { name: "Gemini 计划：Google AI Pro" });
    expect(geminiBadges).toHaveLength(2);
    const bars = within(stack).getAllByLabelText("剩余与已使用配额");
    expect(bars).toHaveLength(4);
    for (const bar of bars) expect(bar).toHaveClass("h-[5px]");

    fireEvent.pointerEnter(geminiBadges[0].parentElement!, { pointerId: 2, pointerType: "mouse", buttons: 0 });
    const dialogs = await screen.findAllByRole("dialog");
    const geminiDialog = dialogs.find((dialog) => dialog.textContent?.includes("gemini@example.com"));
    expect(geminiDialog).toBeDefined();
    expect(geminiDialog).toHaveTextContent("gemini@example.com");
    expect(geminiDialog).not.toHaveTextContent("Gemini 计划");
    expect(geminiDialog).not.toHaveTextContent("数据更新时间");
  });

  it("keeps the expanded quota stack open while the pointer enters an overflowing card", async () => {
    render(<MetricGridFixture usage={usage} modelFilterActive={false} codexQuota={dualQuota} antigravityQuota={geminiQuota} />);

    const card = cardByTitle("账户额度");
    const stack = within(card).getByRole("group", { name: "账户额度卡片" });
    const panel = card.firstElementChild as HTMLElement;
    const background = panel.firstElementChild as HTMLElement;
    const fourthCard = stack.children[3] as HTMLElement;

    expect(card).toHaveClass("h-[144px]");
    expect(panel).toHaveClass("absolute", "top-0", "min-h-[144px]", "p-[10px]");
    expect(panel).not.toHaveClass("h-[144px]");
    expect(background).toHaveClass("absolute", "inset-0", "rounded-3xl", "bg-muted");

    fireEvent.pointerEnter(card, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    const entries = Array.from(stack.children) as HTMLElement[];
    for (const entry of entries) expect(entry).toHaveClass("h-[92px]");
    expect(stack).toHaveStyle({ height: "380px" });
    expect(stack.parentElement).not.toHaveClass("h-[100px]");
    await waitFor(() => {
      expect(entries.map((entry) => Number(entry.style.transform.match(/translateY\((\d+)px\)/)?.[1] ?? 0))).toEqual([0, 96, 192, 288]);
    });
    fireEvent.pointerLeave(card, { pointerId: 1, pointerType: "mouse", buttons: 0, relatedTarget: background });
    expect(stack).toHaveAttribute("aria-expanded", "true");
    fireEvent.pointerLeave(card, { pointerId: 1, pointerType: "mouse", buttons: 0, relatedTarget: fourthCard });

    expect(stack).toHaveAttribute("aria-expanded", "true");
  });

  it("shows only two quota card layers while collapsed", () => {
    render(<MetricGridFixture usage={usage} modelFilterActive={false} codexQuota={dualQuota} antigravityQuota={geminiQuota} />);

    const card = cardByTitle("账户额度");
    const stack = within(card).getByRole("group", { name: "账户额度卡片" });
    const entries = Array.from(stack.children) as HTMLElement[];

    fireEvent.pointerEnter(card, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    fireEvent.pointerLeave(card, { pointerId: 1, pointerType: "mouse", buttons: 0 });

    expect(stack).toHaveAttribute("aria-expanded", "false");
    expect(entries.slice(0, 2).every((entry) => entry.style.visibility !== "hidden")).toBe(true);
    expect(entries.slice(2).every((entry) => entry.style.visibility === "hidden")).toBe(true);
    expect(entries.slice(2).every((entry) => entry.hasAttribute("inert") && entry.style.pointerEvents === "none")).toBe(true);

    fireEvent.click(stack);
    expect(stack).toHaveAttribute("aria-expanded", "true");
    expect(entries.every((entry) => !entry.hasAttribute("inert") && entry.style.pointerEvents === "auto")).toBe(true);
    fireEvent.keyDown(stack, { key: " " });
    expect(stack).toHaveAttribute("aria-expanded", "false");
    fireEvent.keyDown(stack, { key: "Enter" });
    expect(stack).toHaveAttribute("aria-expanded", "true");
    fireEvent.keyDown(stack, { key: "Escape" });
    expect(stack).toHaveAttribute("aria-expanded", "false");
  });

  it("omits windows without data and shows status text without inventing percentages", () => {
    const onlyWeeklyGemini = { ...geminiQuota, session: null };
    const { rerender } = render(<MetricGridFixture usage={usage} modelFilterActive={false} codexQuota={readyQuota} antigravityQuota={onlyWeeklyGemini} />);

    const card = cardByTitle("账户额度");
    const stack = within(card).getByRole("group", { name: "账户额度卡片" });
    expect(stack.children).toHaveLength(2);
    expect(within(stack).getByText("Codex·Weekly")).toBeInTheDocument();
    expect(within(stack).getByText("Gemini·Weekly")).toBeInTheDocument();
    expect(within(stack).queryByText("Codex·5H")).not.toBeInTheDocument();
    expect(within(stack).queryByText("Gemini·5H")).not.toBeInTheDocument();

    rerender(<MetricGridFixture usage={usage} modelFilterActive={false} codexQuota={{ ...readyQuota, status: "loading", weekly: null }} antigravityQuota={{ ...geminiQuota, status: "unavailable", session: null, weekly: null }} />);
    expect(within(card).getByText("正在读取账户额度…")).toBeInTheDocument();
    expect(within(card).queryByLabelText(/%$/)).not.toBeInTheDocument();
    rerender(<MetricGridFixture usage={usage} modelFilterActive={false} codexQuota={{ ...readyQuota, status: "unavailable", weekly: null }} antigravityQuota={{ ...geminiQuota, status: "unavailable", session: null, weekly: null }} />);
    expect(within(card).getByText("暂无可用额度")).toBeInTheDocument();
    expect(within(card).queryByLabelText(/%$/)).not.toBeInTheDocument();
  });

  it("renders structural skeletons without fabricated KPI values", () => {
    const { rerender } = render(<MetricGridFixture usage={null} modelFilterActive={false} />);
    expect(screen.getByLabelText("KPI 加载中").children).toHaveLength(5);
    expect(screen.getByLabelText("KPI 加载中")).toHaveTextContent("");

    rerender(<MetricGridFixture usage={null} modelFilterActive />);
    expect(screen.getByLabelText("KPI 加载中").children).toHaveLength(4);
    expect(screen.getByLabelText("KPI 加载中")).toHaveTextContent("");
  });
});
