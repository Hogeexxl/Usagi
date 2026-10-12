import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ComponentProps } from "react";

import type { QuotaProviderDto, QuotaResponse, QuotaWindowDto, UsageDto } from "../data/types";
import { chartMuted, chartSeriesColor } from "./charts/chartPalette";
import { Popover, PopoverContent, PopoverTrigger } from "../ui/beui/popover";
import { CacheHitMetric, CostMetric, MetricGrid, quotaColor } from "./MetricGrid";
import { formatQuotaResetTime } from "./format";

const usage: UsageDto = {
  calls: 5,
  errors: 1,
  input: 600,
  output: 700,
  cache_read: 400,
  cache_write: 300,
  cache_write_1h: 100,
  reasoning: 125,
  total: 2_000,
  cache_hit_rate: 400 / 1_300,
  cost: 0.0124,
  unpriced: 1,
};

const compactUsage: UsageDto = {
  ...usage,
  input: 12_200_000,
  cache_read: 8_700_000,
  cache_write: 0,
  output: 6_200_000,
  cache_write_1h: 0,
  reasoning: 2_100_000,
  total: 27_100_000,
  cost: 1_240,
  unpriced: 0,
};

const tokenBarLabel = "Token 分布：输入、缓存读和输出；推理 Token 包含在输出 Token 中";
const resetAt = Date.UTC(2026, 7, 12, 4, 23);

function quotaWindow(name: string, used: number, resetsAt = resetAt, unlimited = false): QuotaWindowDto {
  return {
    unlimited,
    name,
    used,
    resets_at_ms: resetsAt,
    reset_secs: 604_800,
    display: "",
    amount: 0,
    limit: 0,
    unit: "",
    family: "",
    pool: "",
  };
}

function provider(
  providerId: string,
  name: string,
  windows: QuotaWindowDto[],
  user = `${providerId}@example.com`,
): QuotaProviderDto {
  return {
    provider: providerId,
    name,
    plan: `${name} Plan`,
    user,
    windows,
    balance: "$12.00 balance",
    error: "",
    as_of_ms: resetAt,
    read_at_ms: resetAt,
  };
}

const quota: QuotaResponse = {
  reading: false,
  items: [provider("codex", "Codex", [quotaWindow("Weekly", 55)], "hoge@example.com")],
};

const manyProviders: QuotaResponse = {
  reading: false,
  items: [
    provider("codex", "Codex", [quotaWindow("5H", 12), quotaWindow("Weekly", 55)], "hoge@example.com"),
    provider("gemini", "Gemini", [quotaWindow("5H", 20), quotaWindow("Weekly", 65)], "gemini@example.com"),
  ],
};

function MetricGridFixture({
  usage: currentUsage = usage,
  sessionCount = 4,
  modelFilterActive = false,
  quota: currentQuota = { reading: false, items: [] },
  onRefreshQuota = () => undefined,
  quotaRefreshing = false,
  quotaRefreshError = false,
  quotaRefreshAvailable = true,
}: Partial<ComponentProps<typeof MetricGrid>>) {
  return (
    <MetricGrid
      usage={currentUsage}
      sessionCount={sessionCount}
      modelFilterActive={modelFilterActive}
      quota={currentQuota}
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

describe("MetricGrid Spec04", () => {
  it("keeps glare only on the total token card and preserves the count column behavior", async () => {
    enableTiltEffects();
    const { rerender } = render(<MetricGridFixture usage={usage} sessionCount={7} quota={quota} />);
    const grid = screen.getByLabelText("KPI 指标");
    await waitFor(() => expect(Array.from(grid.children).map((card) => glareOverlayCount(card as HTMLElement))).toEqual([1, 0, 0, 0, 0]));
    expect(within(grid).getByText("会话数量")).toBeInTheDocument();
    expect(within(grid).getByTitle("7")).toHaveTextContent("7");

    rerender(<MetricGridFixture usage={usage} sessionCount={7} modelFilterActive quota={quota} />);
    expect(screen.getByLabelText("KPI 指标").children).toHaveLength(4);
    expect(screen.queryByText("会话数量")).not.toBeInTheDocument();
  });

  it("shows visible token fields as a normalized bar and keeps reasoning nested within output", () => {
    render(<MetricGridFixture usage={usage} quota={quota} />);
    const card = cardByTitle("总 Token");
    const bar = screen.getByLabelText(tokenBarLabel) as HTMLElement;
    const input = segmentByClass(bar, "bg-[#68c0e8]");
    const cacheRead = segmentByClass(bar, "bg-[#be506e]");
    const output = segmentByClass(bar, "bg-[#be753e]");
    const reasoning = segmentByClass(bar, "bg-[#a6333d]");
    const visibleTotal = usage.input + usage.cache_read + usage.output;

    expect(within(card).getByTitle("2,000")).toHaveTextContent("2K");
    expect(within(card).queryByText("缓存写")).not.toBeInTheDocument();
    expect(within(card).queryByTitle("300")).not.toBeInTheDocument();
    expect(within(card).queryByLabelText(/缓存写/)).not.toBeInTheDocument();
    expect(bar.children).toHaveLength(4);
    expect(input).toHaveStyle({ left: "", width: `${(usage.input / visibleTotal) * 100}%` });
    expect(cacheRead).toHaveStyle({ left: `${(usage.input / visibleTotal) * 100}%`, width: `${(usage.cache_read / visibleTotal) * 100}%` });
    expect(output).toHaveStyle({ left: `${((usage.input + usage.cache_read) / visibleTotal) * 100}%`, width: `${(usage.output / visibleTotal) * 100}%` });
    expect(Number.parseFloat(reasoning.style.left)).toBeCloseTo(((visibleTotal - usage.reasoning) / visibleTotal) * 100);
    expect(Number.parseFloat(reasoning.style.width)).toBeCloseTo((usage.reasoning / visibleTotal) * 100);
    expect(Number.parseFloat(input.style.width) + Number.parseFloat(cacheRead.style.width) + Number.parseFloat(output.style.width)).toBeCloseTo(100);
    expect(screen.getByRole("button", { name: /推理 125，包含在输出 Token 中/ })).toBeInTheDocument();

    const widths = Array.from(bar.children, (segment) => (segment as HTMLElement).style.width);
    for (const legend of screen.getAllByRole("button").filter((button) => button.closest(".h-36") === cardByTitle("总 Token"))) {
      fireEvent.focus(legend);
      expect(Array.from(bar.children, (segment) => (segment as HTMLElement).style.width)).toEqual(widths);
      fireEvent.blur(legend);
    }
  });

  it("keeps the cache hit headline and shows a normalized bar for visible cache fields", () => {
    render(<CacheHitMetric usage={usage} glare={false} />);
    const card = screen.getByText("缓存命中").closest(".h-36") as HTMLElement;
    const display = within(card).getByTitle("30.8%");
    expect(display).toHaveTextContent("30.8%");
    expect(within(card).queryByText("缓存写")).not.toBeInTheDocument();
    expect(within(card).queryByTitle("300")).not.toBeInTheDocument();
    expect(within(card).queryByLabelText(/缓存写/)).not.toBeInTheDocument();

    const bar = screen.getByLabelText("缓存读与输入 Token 分布");
    const cached = segmentByClass(bar, "bg-[#be506e]");
    const input = segmentByClass(bar, "bg-[#4057a5]");
    expect(Array.from(bar.children)).toHaveLength(2);
    expect(cached).toHaveStyle({ width: "40%" });
    expect(input).toHaveStyle({ left: "40%", width: "60%" });
    expect(Number.parseFloat(cached.style.width) + Number.parseFloat(input.style.width)).toBeCloseTo(100);
  });

  it("shows cost per million from cost / total and warns using the unpriced count", async () => {
    render(<CostMetric usage={usage} glare={false} />);
    const card = cardByTitle("费用");
    expect(within(card).getByText("$6.200 / MToken")).toBeInTheDocument();
    expect(screen.getByTitle("$0.01")).toHaveTextContent("$0.01");
    const trigger = within(card).getByRole("button", { name: "未计价使用记录提示" });
    expect(trigger).toHaveClass("text-warning");
    fireEvent.pointerEnter(trigger.parentElement!, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    expect(await screen.findByRole("dialog")).toHaveTextContent("1 条使用记录未计价");
  });

  it("shows complete-cost messaging and uses total for the ratio", async () => {
    render(<CostMetric usage={{ ...usage, unpriced: 0 }} glare={false} />);
    const card = cardByTitle("费用");
    expect(within(card).getByText("$6.200 / MToken")).toBeInTheDocument();
    const trigger = within(card).getByRole("button", { name: "未计价使用记录提示" });
    expect(trigger).toHaveClass("text-foreground");
    fireEvent.pointerEnter(trigger.parentElement!, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    expect(await screen.findByRole("dialog")).toHaveTextContent("所有使用记录均已计价");
  });

  it("uses the provider and window order from the unified quota snapshot", () => {
    render(<MetricGridFixture usage={usage} quota={manyProviders} />);
    const stack = screen.getByRole("group", { name: "账户额度卡片" });
    const labels = Array.from(stack.children, (entry) => entry.firstElementChild?.firstElementChild?.firstElementChild?.textContent);
    expect(labels).toEqual(["Codex·5H", "Codex·Weekly", "Gemini·5H", "Gemini·Weekly"]);
    expect(stack.children).toHaveLength(4);
  });

  it("shows snapshot window percentages, reset data, and provider metadata", async () => {
    render(<MetricGridFixture usage={usage} quota={quota} />);
    const card = cardByTitle("账户额度");
    const windowCard = within(screen.getByRole("group", { name: "账户额度卡片" })).getByText("Codex·Weekly").closest(".absolute") as HTMLElement;
    expect(card).toHaveClass("h-[144px]", "w-[236px]");
    expect(within(windowCard).getByLabelText("45%")).toBeInTheDocument();
    expect(within(windowCard).getByText(`下次重置：${formatQuotaResetTime(resetAt)}`)).toBeInTheDocument();
    const remainingBar = within(windowCard).getByLabelText("剩余与已使用配额");
    expect(remainingBar.children).toHaveLength(2);
    expect((remainingBar.children[0] as HTMLElement).style.width).toBe("45%");
    expect((remainingBar.children[0] as HTMLElement).style.backgroundColor).toBe(chartSeriesColor(5));
    expect((remainingBar.children[1] as HTMLElement).style.backgroundColor).toBe(chartMuted);
    expect(quotaColor(60)).toBe(chartSeriesColor(8));
    expect(quotaColor(45)).toBe(chartSeriesColor(5));
    expect(quotaColor(20)).toBe(chartSeriesColor(5));
    expect(quotaColor(19)).toBe(chartSeriesColor(9));

    const planTrigger = within(windowCard).getByRole("button", { name: "Codex 账户信息" });
    fireEvent.pointerEnter(planTrigger.parentElement!, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    expect(await screen.findByText("hoge@example.com")).toBeInTheDocument();
    expect(screen.getByText("$12.00 balance")).toBeInTheDocument();
  });

  it("converts provider used percentages to remaining quota and keeps unlimited windows unlimited", () => {
    const { rerender } = render(<MetricGridFixture usage={usage} quota={quota} />);

    for (const { used, remaining } of [{ used: 25, remaining: 75 }, { used: 22, remaining: 78 }, { used: 100, remaining: 0 }]) {
      rerender(<MetricGridFixture usage={usage} quota={{ reading: false, items: [provider("codex", "Codex", [quotaWindow("5H", used)])] }} />);
      const windowCard = screen.getByText("Codex·5H").closest(".absolute") as HTMLElement;
      expect(within(windowCard).getByLabelText(`${remaining}%`)).toBeInTheDocument();

      const remainingBar = within(windowCard).getByLabelText("剩余与已使用配额");
      expect((remainingBar.children[0] as HTMLElement).style.width).toBe(`${remaining}%`);
      expect((remainingBar.children[1] as HTMLElement).style.width).toBe(`${used}%`);
    }

    rerender(<MetricGridFixture usage={usage} quota={{ reading: false, items: [provider("codex", "Codex", [quotaWindow("5H", 25, resetAt, true)])] }} />);
    const unlimitedCard = screen.getByText("Codex·5H").closest(".absolute") as HTMLElement;
    expect(within(unlimitedCard).getByText("∞")).toBeInTheDocument();
  });

  it("keeps the quota stack open while moving into overflow and supports keyboard controls", () => {
    render(<MetricGridFixture usage={usage} quota={manyProviders} />);
    const card = cardByTitle("账户额度");
    const stack = screen.getByRole("group", { name: "账户额度卡片" });
    const entries = Array.from(stack.children) as HTMLElement[];

    fireEvent.pointerEnter(card, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    expect(stack).toHaveAttribute("aria-expanded", "true");
    expect(stack).toHaveStyle({ height: "380px" });
    fireEvent.pointerLeave(card, { pointerId: 1, pointerType: "mouse", buttons: 0, relatedTarget: entries[3] });
    expect(stack).toHaveAttribute("aria-expanded", "true");
    fireEvent.pointerLeave(card, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    expect(stack).toHaveAttribute("aria-expanded", "false");
    expect(entries.slice(0, 2).every((entry) => entry.style.visibility !== "hidden")).toBe(true);
    expect(entries.slice(2).every((entry) => entry.style.visibility === "hidden")).toBe(true);

    fireEvent.click(stack);
    expect(stack).toHaveAttribute("aria-expanded", "true");
    fireEvent.keyDown(stack, { key: "Escape" });
    expect(stack).toHaveAttribute("aria-expanded", "false");
  });

  it("shows reading and provider error states without fabricating quota percentages", () => {
    const { rerender } = render(<MetricGridFixture usage={usage} quota={{ reading: true, items: [] }} />);
    const card = cardByTitle("账户额度");
    expect(within(card).getByText("正在读取账户额度…")).toBeInTheDocument();
    expect(within(card).queryByLabelText(/%$/)).not.toBeInTheDocument();
    rerender(<MetricGridFixture usage={usage} quota={{ reading: false, items: [provider("codex", "Codex", [], "",)] .map((item) => ({ ...item, error: "Quota unavailable" })) }} />);
    expect(within(card).getByText("Quota unavailable")).toBeInTheDocument();
    expect(within(card).queryByLabelText(/%$/)).not.toBeInTheDocument();
  });

  it("preserves default Popover theme behavior and renders structural skeletons", () => {
    const { rerender } = render(
      <Popover defaultOpen>
        <PopoverTrigger><button type="button">default</button></PopoverTrigger>
        <PopoverContent>Default content</PopoverContent>
      </Popover>,
    );
    const dialog = screen.getByRole("dialog");
    const portal = dialog.closest("[data-popover-portal]");
    expect(portal?.querySelectorAll(".bg-popover")).toHaveLength(2);
    expect(portal?.querySelectorAll(".bg-primary")).toHaveLength(0);
    expect(dialog).toHaveClass("text-popover-foreground");
    rerender(<MetricGridFixture usage={null} />);
    expect(screen.getByLabelText("KPI 加载中").children).toHaveLength(5);
    expect(screen.getByLabelText("KPI 加载中")).toHaveTextContent("");
    rerender(<MetricGridFixture usage={null} modelFilterActive />);
    expect(screen.getByLabelText("KPI 加载中").children).toHaveLength(4);
  });

  it("keeps compact values accessible with full values in title and aria labels", () => {
    render(<MetricGridFixture usage={compactUsage} quota={quota} />);
    const token = screen.getByTitle("27,100,000");
    expect(token).toHaveAttribute("aria-label", "27,100,000");
    expect(token).toHaveTextContent("27.1M");
    const cost = screen.getByTitle("$1,240.00");
    expect(cost).toHaveAttribute("aria-label", "$1,240.00");
    expect(cost).toHaveTextContent("$1.24K");
  });
});
