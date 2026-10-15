import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ComponentProps } from "react";

import type { QuotaProviderDto, QuotaResponse, QuotaWindowDto, RequestMetricsDto, UsageDto } from "../data/types";
import { chartMuted, chartSeriesColor } from "./charts/chartPalette";
import { Popover, PopoverContent, PopoverTrigger } from "../ui/beui/popover";
import { CacheHitMetric, CostMetric, MetricGrid, OutputSpeedMetric, RequestMetric, quotaColor } from "./MetricGrid";
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
  retried: 2,
  output_speed: 62.4,
  speed_samples: 3,
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
  retried: 0, output_speed: null, speed_samples: 0,
};

const requestMetrics: RequestMetricsDto = {
  calls: 5,
  errors: 1,
  retried: 2,
  output_speed: 62.4,
  speed_samples: 3,
  mean_ttft_seconds: 7.6,
  ttft_samples: 4,
};

const tokenBarLabel = "Token 分布：输入、缓存读、缓存写和输出";
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
  requestMetrics: currentRequestMetrics = requestMetrics,
  displayMode = "session",
  sessionCount = 8,
  sessionMedian = 1_000,
  sessionP90 = 3_000,
  quota: currentQuota = { reading: false, items: [] },
  onRefreshQuota = () => undefined,
  quotaRefreshing = false,
  quotaRefreshError = false,
  quotaRefreshAvailable = true,
}: Partial<ComponentProps<typeof MetricGrid>>) {
  return (
    <MetricGrid
      usage={currentUsage}
      requestMetrics={currentRequestMetrics}
      displayMode={displayMode}
      sessionCount={sessionCount}
      sessionMedian={sessionMedian}
      sessionP90={sessionP90}
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

describe("MetricGrid Spec05", () => {
  it("renders the five session cards by default", async () => {
    enableTiltEffects();
    render(<MetricGridFixture usage={usage} quota={quota} />);
    const grid = screen.getByLabelText("KPI 指标");
    await waitFor(() => expect(Array.from(grid.children).map((card) => glareOverlayCount(card as HTMLElement))).toEqual([1, 0, 0, 0, 0]));
    expect(Array.from(grid.children).map((card) => card.querySelector(".text-xs.font-medium")?.textContent)).toEqual(["总 Token", "缓存命中", "等价费用", "会话数", "账户额度"]);
    expect(within(grid).queryByText("请求数")).not.toBeInTheDocument();
    expect(cardByTitle("会话数")).toHaveTextContent("中位 1K · P90 3K");
  });

  it("shows visible token fields as a normalized bar without a separate reasoning part", () => {
    render(<MetricGridFixture usage={usage} quota={quota} />);
    const card = cardByTitle("总 Token");
    const bar = screen.getByLabelText(tokenBarLabel) as HTMLElement;
    const input = segmentByClass(bar, "bg-[#68c0e8]");
    const cacheRead = segmentByClass(bar, "bg-[#be506e]");
    const cacheWrite = Array.from(bar.children).find((child) => (child as HTMLElement).style.backgroundColor === chartSeriesColor(3)) as HTMLElement;
    const output = segmentByClass(bar, "bg-[#be753e]");
    const visibleTotal = usage.total;

    expect(within(card).getByTitle("2,000")).toHaveTextContent("2K");
    expect(within(card).getByText("缓存写")).toBeInTheDocument();
    expect(within(card).getByTitle("300")).toBeInTheDocument();
    expect(within(card).getByLabelText(/缓存写/)).toBeInTheDocument();
    expect(bar.children).toHaveLength(4);
    expect(within(card).queryByText("推理")).not.toBeInTheDocument();
    expect(within(card).queryByLabelText(/推理/)).not.toBeInTheDocument();
    expect(input).toHaveStyle({ left: "", width: `${(usage.input / visibleTotal) * 100}%` });
    expect(cacheRead).toHaveStyle({ left: `${(usage.input / visibleTotal) * 100}%`, width: `${(usage.cache_read / visibleTotal) * 100}%` });
    expect(cacheWrite).toHaveStyle({ left: `${((usage.input + usage.cache_read) / visibleTotal) * 100}%`, width: `${(usage.cache_write / visibleTotal) * 100}%` });
    expect(output).toHaveStyle({ left: `${((usage.input + usage.cache_read + usage.cache_write) / visibleTotal) * 100}%`, width: `${(usage.output / visibleTotal) * 100}%` });
    expect(Array.from(bar.children).reduce((sum, segment) => sum + Number.parseFloat((segment as HTMLElement).style.width), 0)).toBeCloseTo(100);

    const widths = Array.from(bar.children, (segment) => (segment as HTMLElement).style.width);
    for (const legend of screen.getAllByRole("button").filter((button) => button.closest(".h-36") === cardByTitle("总 Token"))) {
      fireEvent.focus(legend);
      expect(Array.from(bar.children, (segment) => (segment as HTMLElement).style.width)).toEqual(widths);
      fireEvent.blur(legend);
    }
  });

  it("hides zero token values from both card legends while preserving their bars", () => {
    const partialUsage = { ...usage, input: 0, cache_write: 0 };
    const rendered = render(<MetricGridFixture usage={partialUsage} quota={quota} />);

    const totalCard = cardByTitle("总 Token");
    expect(within(totalCard).queryByText("输入")).not.toBeInTheDocument();
    expect(within(totalCard).queryByText("缓存写")).not.toBeInTheDocument();
    expect(within(totalCard).getByText("缓存读")).toBeInTheDocument();
    expect(within(totalCard).getByText("输出")).toBeInTheDocument();
    const totalBar = within(totalCard).getByLabelText(tokenBarLabel);
    expect(totalBar.children).toHaveLength(4);
    expect(segmentByClass(totalBar as HTMLElement, "bg-[#68c0e8]")).toHaveStyle({ width: "0%" });

    const cacheCard = cardByTitle("缓存命中");
    expect(within(cacheCard).queryByText("输入")).not.toBeInTheDocument();
    expect(within(cacheCard).queryByText("缓存写")).not.toBeInTheDocument();
    expect(within(cacheCard).getByText("缓存读")).toBeInTheDocument();
    const cacheBar = within(cacheCard).getByLabelText("缓存读、输入与缓存写 Token 分布");
    expect(cacheBar.children).toHaveLength(3);
    expect(Array.from(cacheBar.children, (segment) => (segment as HTMLElement).style.width)).toEqual(["100%", "0%", "0%"]);

    rendered.rerender(<MetricGridFixture usage={{ ...partialUsage, cache_read: 0, output: 0 }} quota={quota} />);
    const zeroTotalCard = cardByTitle("总 Token");
    expect(within(zeroTotalCard).queryByRole("button")).not.toBeInTheDocument();
    const zeroTotalBar = within(zeroTotalCard).getByLabelText(tokenBarLabel);
    expect(zeroTotalBar.children).toHaveLength(4);
    expect(Array.from(zeroTotalBar.children, (segment) => (segment as HTMLElement).style.width)).toEqual(["0%", "0%", "0%", "0%"]);

    const zeroCacheCard = cardByTitle("缓存命中");
    expect(within(zeroCacheCard).queryByRole("button")).not.toBeInTheDocument();
    const zeroCacheBar = within(zeroCacheCard).getByLabelText("缓存读、输入与缓存写 Token 分布");
    expect(zeroCacheBar.children).toHaveLength(3);
    expect(Array.from(zeroCacheBar.children, (segment) => (segment as HTMLElement).style.width)).toEqual(["0%", "0%", "0%"]);
  });

  it("keeps the cache hit headline and normalizes cache read, input, and cache write", () => {
    render(<CacheHitMetric usage={usage} glare={false} />);
    const card = screen.getByText("缓存命中").closest(".h-36") as HTMLElement;
    const display = within(card).getByTitle("30.8%");
    expect(display).toHaveTextContent("30.8%");
    expect(within(card).getByText("缓存写")).toBeInTheDocument();
    expect(within(card).getByTitle("300")).toBeInTheDocument();
    expect(within(card).getByLabelText(/缓存写/)).toBeInTheDocument();

    const bar = screen.getByLabelText("缓存读、输入与缓存写 Token 分布");
    const cached = segmentByClass(bar, "bg-[#be506e]");
    const input = segmentByClass(bar, "bg-[#4057a5]");
    const cacheWrite = Array.from(bar.children).find((child) => (child as HTMLElement).style.backgroundColor === chartSeriesColor(3)) as HTMLElement;
    const denominator = usage.input + usage.cache_read + usage.cache_write;
    expect(Array.from(bar.children)).toHaveLength(3);
    expect(cached).toHaveStyle({ width: `${(usage.cache_read / denominator) * 100}%` });
    expect(input).toHaveStyle({ left: `${(usage.cache_read / denominator) * 100}%`, width: `${(usage.input / denominator) * 100}%` });
    expect(Number.parseFloat(cacheWrite.style.left)).toBeCloseTo(((usage.cache_read + usage.input) / denominator) * 100);
    expect(cacheWrite).toHaveStyle({ width: `${(usage.cache_write / denominator) * 100}%` });
    expect(Array.from(bar.children).reduce((sum, segment) => sum + Number.parseFloat((segment as HTMLElement).style.width), 0)).toBeCloseTo(100);
  });

  it("shows cost per million from equivalent cost / total without an unpriced prompt", () => {
    render(<CostMetric usage={usage} glare={false} />);
    const card = cardByTitle("等价费用");
    expect(within(card).getByText("$6.200 / MToken")).toBeInTheDocument();
    expect(screen.getByTitle("$0.01")).toHaveTextContent("$0.01");
    expect(within(card).queryByRole("button")).not.toBeInTheDocument();
    expect(within(card).queryByText(/未计价/)).not.toBeInTheDocument();
  });

  it("uses total for the equivalent cost ratio regardless of unpriced count", () => {
    render(<CostMetric usage={{ ...usage, unpriced: 0 }} glare={false} />);
    const card = cardByTitle("等价费用");
    expect(within(card).getByText("$6.200 / MToken")).toBeInTheDocument();
    expect(within(card).queryByText(/未计价/)).not.toBeInTheDocument();
  });

  it("renders null session percentiles as em dashes", () => {
    render(<MetricGridFixture sessionMedian={null} sessionP90={null} />);
    expect(within(cardByTitle("会话数")).getByText("中位 — · P90 —")).toBeInTheDocument();
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
  });

  it("renders request count with failure and retry details in the card footer", () => {
    render(<RequestMetric metrics={{ ...requestMetrics, calls: 10_091, errors: 277, retried: 36 }} />);
    const card = cardByTitle("请求数");
    expect(within(card).getByTitle("10,091")).toHaveTextContent("10091");
    expect(within(card).queryByText(/输出速度/)).not.toBeInTheDocument();
    expect(within(card).queryByRole("button", { name: "请求失败与重试提示" })).not.toBeInTheDocument();
    const failures = within(card).getByText("277 次失败");
    expect(failures).toHaveClass("text-destructive");
    expect(failures.parentElement).toHaveTextContent("277 次失败 · 成功 97.3%");
    expect(failures.parentElement?.lastElementChild).toHaveClass("text-muted-foreground");
    expect(failures.parentElement?.parentElement?.children).toHaveLength(2);
    const retries = within(card).getByText("36 次重试后成功");
    expect(retries).toHaveClass("text-warning");
    expect(retries.parentElement).toHaveTextContent("36 次重试后成功");
  });

  it("shows success and retry details when there are no failures", () => {
    render(<RequestMetric metrics={{ ...requestMetrics, calls: 0, errors: 0, retried: 0 }} />);
    const card = cardByTitle("请求数");
    const failures = within(card).getByText("0 次失败", { selector: ".text-destructive" });
    expect(failures.parentElement).toHaveTextContent("0 次失败 · 成功 —");
    const retries = within(card).getByText("0 次重试后成功", { selector: ".text-warning" });
    expect(retries.parentElement).toHaveTextContent("0 次重试后成功");
  });

  it("uses compact request counts for the headline, failures, and retries", () => {
    render(<RequestMetric metrics={{ ...requestMetrics, calls: 141_100, errors: 100_000, retried: 141_101 }} />);
    const card = cardByTitle("请求数");
    expect(within(card).getByTitle("141,100")).toHaveTextContent("141.1K");
    expect(within(card).getByText("100K 次失败")).toHaveClass("text-destructive");
    expect(within(card).getByText("100K 次失败")).toHaveAttribute("title", "100,000");
    expect(within(card).getByText("141.1K 次重试后成功")).toHaveClass("text-warning");
    expect(within(card).getByText("141.1K 次重试后成功")).toHaveAttribute("title", "141,101");
  });

  it("shows integer output speed and keeps speed and TTFT sample counts distinct", () => {
    render(<OutputSpeedMetric metrics={{ ...requestMetrics, output_speed: 61.988, speed_samples: 2_999, mean_ttft_seconds: 7.6, ttft_samples: 36 }} />);
    const card = cardByTitle("输出速度");
    expect(within(card).getByText("token/s")).toHaveClass("text-[28px]", "font-normal", "leading-none", "tracking-tight", "text-muted-foreground");
    expect(within(card).getByTitle("62")).toHaveTextContent("62");
    expect(within(card).getByText("2999 次计时")).toHaveAttribute("title", "2,999 次计时");
    expect(within(card).getByText("首字平均 7.6s · 36 次计时")).toBeInTheDocument();
  });

  it("uses request count formatting for both timing sample counts", () => {
    render(<OutputSpeedMetric metrics={{ ...requestMetrics, output_speed: 61.988, speed_samples: 141_100, mean_ttft_seconds: 7.6, ttft_samples: 99_999 }} />);
    const card = cardByTitle("输出速度");
    expect(within(card).getByText("141.1K 次计时")).toHaveAttribute("title", "141,100 次计时");
    expect(within(card).getByText("首字平均 7.6s · 99999 次计时")).toHaveAttribute("aria-label", "首字平均 7.6s · 99,999 次计时");
  });

  it("hides both timing lines when speed has no valid sample and preserves nullable TTFT", () => {
    const { rerender } = render(<OutputSpeedMetric metrics={{ ...requestMetrics, output_speed: null, speed_samples: 4, mean_ttft_seconds: 7.6, ttft_samples: 2 }} />);
    let card = cardByTitle("输出速度");
    expect(within(card).getByText("—")).toBeInTheDocument();
    expect(within(card).queryByText(/次计时/)).not.toBeInTheDocument();
    expect(within(card).queryByText(/首字平均/)).not.toBeInTheDocument();

    rerender(<OutputSpeedMetric metrics={{ ...requestMetrics, output_speed: 61.9, speed_samples: 1, mean_ttft_seconds: null, ttft_samples: 0 }} />);
    card = cardByTitle("输出速度");
    expect(within(card).getByText("首字平均 — · 0 次计时")).toBeInTheDocument();
  });

  it("replaces the last two cards with request metrics while keeping five KPI cards", () => {
    render(<MetricGridFixture displayMode="request" usage={usage} requestMetrics={requestMetrics} quota={quota} />);
    const grid = screen.getByLabelText("KPI 指标");
    expect(grid.children).toHaveLength(5);
    expect(Array.from(grid.children).map((card) => card.querySelector(".text-xs.font-medium")?.textContent)).toEqual([
      "总 Token", "缓存命中", "等价费用", "请求数", "输出速度",
    ]);
    expect(within(grid).queryByRole("group", { name: "账户额度" })).not.toBeInTheDocument();
  });

  it("shows only Copilot premium requests, titled Copilot·requests, with its reset time instead of the usage text", () => {
    const copilotQuota: QuotaResponse = {
      reading: false,
      items: [{
        ...provider("copilot", "Copilot", [
          { ...quotaWindow("Premium requests", 100), display: "1504.9 / 1500" },
          { ...quotaWindow("Chat requests", 0, resetAt, true), resets_at_ms: null, display: "Unlimited" },
          { ...quotaWindow("Completions", 0, resetAt, true), resets_at_ms: null, display: "Unlimited" },
        ]),
        plan: "Pro",
      }],
    };
    render(<MetricGridFixture usage={usage} quota={copilotQuota} />);
    const stack = screen.getByRole("group", { name: "账户额度卡片" });
    expect(stack.children).toHaveLength(1);
    const windowCard = within(stack).getByText("Copilot·requests").closest(".absolute") as HTMLElement;
    expect(within(stack).queryByText(/Chat requests|Completions|Premium/)).not.toBeInTheDocument();
    expect(within(windowCard).getByText(`下次重置：${formatQuotaResetTime(resetAt)}`)).toHaveAttribute("title", "1504.9 / 1500");
    expect(within(windowCard).queryByText("1504.9 / 1500")).not.toBeInTheDocument();
    expect(within(windowCard).getByLabelText("0%")).toBeInTheDocument();
  });

  it("lists Claude 5 hours and 7 days windows between Codex and Copilot", () => {
    const mixed: QuotaResponse = {
      reading: false,
      items: [
        provider("copilot", "Copilot", [quotaWindow("Premium requests", 10)]),
        provider("claude", "Claude Code", [quotaWindow("5 hours", 38), quotaWindow("7 days", 18)]),
        provider("codex", "Codex", [quotaWindow("5 hours", 16), quotaWindow("7 days", 30)]),
      ],
    };
    render(<MetricGridFixture usage={usage} quota={mixed} />);
    const stack = screen.getByRole("group", { name: "账户额度卡片" });
    const labels = Array.from(stack.children, (entry) => entry.firstElementChild?.firstElementChild?.firstElementChild?.textContent);
    expect(labels).toEqual(["Codex·5 hours", "Codex·7 days", "Claude·5 hours", "Claude·7 days", "Copilot·requests"]);
    const claude = within(stack).getByText("Claude·5 hours").closest(".absolute") as HTMLElement;
    expect(within(claude).getByLabelText("62%")).toBeInTheDocument();
    expect(claude.querySelector('button[aria-label="Claude Code 账户信息"]')).toHaveTextContent("Claude Code Plan");
    expect(within(claude).getByText(`下次重置：${formatQuotaResetTime(resetAt)}`)).toBeInTheDocument();
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
