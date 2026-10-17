import { CircleAlert, RefreshCw } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";
import { memo, useRef, useState } from "react";

import type { QuotaProviderDto, QuotaResponse, QuotaWindowDto, RequestMetricsDto, UsageDto } from "../data/types";
import { chartMuted, chartSeriesColor } from "./charts/chartPalette";
import { EASE_OUT } from "../ui/lib/ease";
import { NumberTicker } from "../ui/beui/number-ticker";
import { Popover, PopoverContent, PopoverTrigger } from "../ui/beui/popover";
import { Button } from "../ui/beui/button";
import { NotificationStack, type NotificationStackItem } from "../ui/beui/notification-stack";
import { TiltCard } from "../ui/beui/tilt-card";
import { formatQuotaResetTime, formatCostFull, formatCostPerMillionTokens, formatIntegerFull, formatRatio, formatRequestCount, type FormattedValue } from "./format";
import { useHoverGesture } from "../ui/lib/use-hover-gesture";

type MetricGridProps = {
  usage: UsageDto | null;
  requestMetrics: RequestMetricsDto | null;
  displayMode: "session" | "request";
  sessionCount: number;
  sessionMedian: number | null;
  sessionP90: number | null;
  quota: QuotaResponse;
  onRefreshQuota: () => void;
  quotaRefreshing: boolean;
  quotaRefreshError: boolean;
  quotaRefreshAvailable: boolean;
};
type Focus = "input" | "cache_read" | "cache_write" | "output" | null;
type CacheFocus = "cached" | "input" | "cache_write" | null;

const CARD = "dashboard-metric-card h-36 min-w-0 border border-border bg-card p-4 text-foreground";
const TITLE = "text-xs font-medium leading-4 text-foreground";
const VALUE = "dashboard-metric-value mt-2 text-[28px] font-semibold leading-8 tracking-tight text-foreground";
const LEGEND = "dashboard-metric-legend h-4 text-xs leading-4 text-muted-foreground";

function CompactTicker({
  value,
  formatter,
  tickerValue = value,
  tickerFormatter,
  className = VALUE,
  wrapperClassName,
}: {
  value: number;
  formatter: (value: number) => FormattedValue;
  tickerValue?: number;
  tickerFormatter?: (value: number) => string;
  className?: string;
  wrapperClassName?: string;
}) {
  const formatted = formatter(value);
  const visibleFormatter = tickerFormatter ?? ((next: number) => formatter(next).text);

  return (
    <span className={wrapperClassName} title={formatted.title} aria-label={formatted.accessibleName}>
      <NumberTicker value={tickerValue} blur format={visibleFormatter} className={className} />
    </span>
  );
}

function Dot({ className, color }: { className?: string; color?: string }) {
  return (
    <span aria-hidden className="inline-flex h-4 shrink-0 items-center justify-center">
      <span className={`block h-1.5 w-1.5 rounded-full ${className ?? ""}`} style={color ? { backgroundColor: color } : undefined} />
    </span>
  );
}

export function TotalTokenMetric({ usage }: { usage: UsageDto }) {
  const reduce = useReducedMotion();
  const [focus, setFocus] = useState<Focus>(null);
  const input = usage.input;
  const cacheRead = usage.cache_read;
  const cacheWrite = usage.cache_write;
  const output = usage.output;
  const total = usage.total;
  const inputPct = total > 0 ? (input / total) * 100 : 0;
  const cacheReadPct = total > 0 ? (cacheRead / total) * 100 : 0;
  const cacheWritePct = total > 0 ? (cacheWrite / total) * 100 : 0;
  const cacheWriteStartPct = inputPct + cacheReadPct;
  const outputStartPct = cacheWriteStartPct + cacheWritePct;
  const outputPct = total > 0 ? (output / total) * 100 : 0;
  const dim = (key: Exclude<Focus, null>) => focus !== null && focus !== key;
  const transition = reduce ? { duration: 0 } : { ease: EASE_OUT };

  return (
    <TiltCard className={`${CARD} flex flex-col max-[1440px]:col-span-4 max-[1024px]:col-span-2`}>
      <div className={TITLE}>总 Token</div>
      <CompactTicker value={total} formatter={formatIntegerFull} />
      <div className="relative mt-1 h-[5px] overflow-hidden rounded-full bg-muted" aria-label="Token 分布：输入、缓存读、缓存写和输出">
        <motion.div
          className="absolute inset-y-0"
          style={{ left: `${cacheWriteStartPct}%`, width: `${cacheWritePct}%`, backgroundColor: chartSeriesColor(3) }}
          animate={{ opacity: dim("cache_write") ? 0.3 : 1, scaleY: focus === "cache_write" && !reduce ? 1.25 : 1 }}
          transition={transition}
        />
        <motion.div
          className="absolute inset-y-0 left-0 bg-[#68c0e8]"
          style={{ width: `${inputPct}%` }}
          animate={{ opacity: dim("input") ? 0.3 : 1, scaleY: focus === "input" && !reduce ? 1.25 : 1 }}
          transition={transition}
        />
        <motion.div
          className="absolute inset-y-0 bg-[#be506e]"
          style={{ left: `${inputPct}%`, width: `${cacheReadPct}%` }}
          animate={{ opacity: dim("cache_read") ? 0.3 : 1, scaleY: focus === "cache_read" && !reduce ? 1.25 : 1 }}
          transition={transition}
        />
        <motion.div
          className="absolute inset-y-0 bg-[#be753e]"
          style={{ left: `${outputStartPct}%`, width: `${outputPct}%` }}
          animate={{ opacity: dim("output") ? 0.3 : 1, scaleY: focus === "output" && !reduce ? 1.25 : 1 }}
          transition={transition}
        />
      </div>
      <div className="mt-auto flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        {input !== 0 ? <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("input")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("input")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#68c0e8]" /><span className="inline-flex h-4 items-center">输入</span><CompactTicker value={input} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button> : null}
        {cacheRead !== 0 ? <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("cache_read")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("cache_read")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#be506e]" /><span className="inline-flex h-4 items-center">缓存读</span><CompactTicker value={cacheRead} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button> : null}
        {cacheWrite !== 0 ? <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("cache_write")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("cache_write")} onBlur={() => setFocus(null)}>
          <Dot color={chartSeriesColor(3)} /><span className="inline-flex h-4 items-center">缓存写</span><CompactTicker value={cacheWrite} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button> : null}
        {output !== 0 ? <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("output")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("output")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#be753e]" /><span className="inline-flex h-4 items-center">输出</span><CompactTicker value={output} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button> : null}
      </div>
    </TiltCard>
  );
}

export function CacheHitMetric({ usage, glare = true }: { usage: UsageDto; glare?: boolean }) {
  const reduce = useReducedMotion();
  const [focus, setFocus] = useState<CacheFocus>(null);
  const input = usage.input;
  const cached = usage.cache_read;
  const cacheWrite = usage.cache_write;
  const rate = usage.cache_hit_rate;
  const visibleTotal = input + cached + cacheWrite;
  const cachedPct = visibleTotal > 0 ? Math.min(1, Math.max(0, cached / visibleTotal)) * 100 : 0;
  const inputPct = visibleTotal > 0 ? (input / visibleTotal) * 100 : 0;
  const cacheWritePct = visibleTotal > 0 ? (cacheWrite / visibleTotal) * 100 : 0;

  return (
    <TiltCard glare={glare} className={`${CARD} flex flex-col max-[1440px]:col-span-2 max-[1024px]:col-span-1`}>
      <div className={TITLE}>缓存命中</div>
      {rate === null ? (
        <div className={VALUE}>—</div>
      ) : (
        <CompactTicker value={rate} tickerValue={Math.round(rate * 1000)} formatter={formatRatio} tickerFormatter={(next) => formatRatio(next / 1000).text} />
      )}
      <motion.div className="relative mt-1 h-[5px] overflow-hidden rounded-full bg-muted" aria-label="缓存读、输入与缓存写 Token 分布" animate={{ scaleY: focus !== null && !reduce ? 1.12 : 1 }} transition={reduce ? { duration: 0 } : { ease: EASE_OUT }}>
        <motion.div className="absolute inset-y-0 left-0 bg-[#be506e]" style={{ width: `${cachedPct}%` }} animate={{ opacity: focus === null || focus === "cached" ? 1 : 0.3 }} transition={reduce ? { duration: 0 } : { ease: EASE_OUT }} />
        <motion.div className="absolute inset-y-0 bg-[#4057a5]" style={{ left: `${cachedPct}%`, width: `${inputPct}%` }} animate={{ opacity: focus === null || focus === "input" ? 1 : 0.3 }} transition={reduce ? { duration: 0 } : { ease: EASE_OUT }} />
        <motion.div className="absolute inset-y-0" style={{ left: `${cachedPct + inputPct}%`, width: `${cacheWritePct}%`, backgroundColor: chartSeriesColor(3) }} animate={{ opacity: focus === null || focus === "cache_write" ? 1 : 0.3 }} transition={reduce ? { duration: 0 } : { ease: EASE_OUT }} />
      </motion.div>
      <div className="mt-auto flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        {cached !== 0 ? <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("cached")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("cached")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#be506e]" /><span className="inline-flex h-4 items-center">缓存读</span><CompactTicker value={cached} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button> : null}
        {input !== 0 ? <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("input")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("input")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#4057a5]" /><span className="inline-flex h-4 items-center">输入</span><CompactTicker value={input} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button> : null}
        {cacheWrite !== 0 ? <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("cache_write")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("cache_write")} onBlur={() => setFocus(null)}>
          <Dot color={chartSeriesColor(3)} /><span className="inline-flex h-4 items-center">缓存写</span><CompactTicker value={cacheWrite} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button> : null}
      </div>
    </TiltCard>
  );
}

export function RequestMetric({ metrics, glare = false }: { metrics: RequestMetricsDto; glare?: boolean }) {
  const successRate = metrics.calls > 0 ? formatRatio((metrics.calls - metrics.errors) / metrics.calls).text : "—";
  const failures = formatRequestCount(metrics.errors);
  const retries = formatRequestCount(metrics.retried);

  return (
    <TiltCard glare={glare} className={`${CARD} flex flex-col max-[1440px]:col-span-2 max-[1024px]:col-span-1`}>
      <div className={TITLE}>请求数</div>
      <CompactTicker value={metrics.calls} formatter={formatRequestCount} />
      <div className="mt-auto flex flex-col text-xs leading-4">
        <div>
          <span className="text-destructive" title={failures.title}>{failures.text} 次失败</span>
          <span className="text-muted-foreground"> · 成功 {successRate}</span>
        </div>
        <div>
          <span className="text-warning" title={retries.title}>{retries.text} 次重试后成功</span>
        </div>
      </div>
    </TiltCard>
  );
}

export function OutputSpeedMetric({ metrics, glare = false }: { metrics: RequestMetricsDto; glare?: boolean }) {
  const hasSpeedSample = metrics.output_speed !== null && metrics.speed_samples > 0;
  const speedSamples = formatRequestCount(metrics.speed_samples);
  const ttftSamples = formatRequestCount(metrics.ttft_samples);
  const meanTtft = metrics.mean_ttft_seconds === null ? "—" : `${metrics.mean_ttft_seconds.toFixed(1)}s`;

  return (
    <TiltCard glare={glare} className={`${CARD} flex flex-col max-[1440px]:col-span-2 max-[1024px]:col-span-1`}>
      <div className={TITLE}>输出速度</div>
      {hasSpeedSample ? (
        <>
          <div className="dashboard-metric-value-row mt-2 flex min-w-0 items-center gap-1">
            <CompactTicker
              value={Math.round(metrics.output_speed!)}
              formatter={formatIntegerFull}
              tickerFormatter={(next) => formatIntegerFull(next).title}
              className="dashboard-metric-value text-[28px] font-semibold leading-8 tracking-tight text-foreground"
              wrapperClassName="inline-flex items-center"
            />
            <span className="dashboard-metric-value inline-flex h-[1.1em] items-center truncate text-[28px] font-normal leading-none tracking-tight text-muted-foreground">token/s</span>
          </div>
          <div className={`${LEGEND} mt-1`} title={`${speedSamples.title} 次计时`} aria-label={`${speedSamples.accessibleName} 次计时`}>
            {speedSamples.text} 次计时
          </div>
          <div className={`${LEGEND} mt-auto`} title={`首字平均 ${meanTtft} · ${ttftSamples.title} 次计时`} aria-label={`首字平均 ${meanTtft} · ${ttftSamples.accessibleName} 次计时`}>
            首字平均 {meanTtft} · {ttftSamples.text} 次计时
          </div>
        </>
      ) : (
        <div className="mt-2 text-[28px] font-semibold leading-8 tracking-tight text-foreground">—</div>
      )}
    </TiltCard>
  );
}

export function CostMetric({ usage, glare = true }: { usage: UsageDto; glare?: boolean }) {
  const hasUnpriced = usage.unpriced > 0;
  const costPerMillionTokens = formatCostPerMillionTokens(usage.total > 0 ? (usage.cost / usage.total) * 1_000_000 : null);

  return (
    <TiltCard glare={glare} className={`${CARD} flex flex-col max-[1440px]:col-span-2 max-[1024px]:col-span-1`}>
      <div className="flex items-center justify-between gap-2">
        <div className={TITLE}>等价费用</div>
        <Popover trigger="hover" side="bottom" align="end">
          <PopoverTrigger>
            <button
              type="button"
              aria-label="未计价使用记录提示"
              className={`inline-flex items-center justify-center ${hasUnpriced ? "text-warning" : "text-foreground"} outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2`}
            >
              <CircleAlert className="h-4 w-4" />
            </button>
          </PopoverTrigger>
          <PopoverContent inverseTheme className="w-max max-w-64 text-xs">
            <div className="flex flex-col gap-1">
              {hasUnpriced ? <div>{usage.unpriced} 条使用记录未计价</div> : <div>所有使用记录均已计价</div>}
            </div>
          </PopoverContent>
        </Popover>
      </div>
      <CompactTicker value={usage.cost} tickerValue={Math.round(usage.cost * 100)} formatter={formatCostFull} tickerFormatter={(next) => formatCostFull(next / 100).text} />
      <div className={`${LEGEND} mt-auto`} title={costPerMillionTokens.title} aria-label={costPerMillionTokens.accessibleName}>
        {costPerMillionTokens.text}
      </div>
    </TiltCard>
  );
}

export function SessionMetric({ count, median, p90 }: { count: number; median: number | null; p90: number | null }) {
  const percentileText = (value: number | null) => value === null ? "—" : formatIntegerFull(value).text;
  return (
    <TiltCard glare={false} className={`${CARD} flex flex-col max-[1440px]:col-span-2 max-[1024px]:col-span-1`}>
      <div className={TITLE}>会话数</div>
      <CompactTicker value={count} formatter={formatIntegerFull} />
      <div className={`${LEGEND} mt-auto`} aria-label={`会话 Token 中位 ${percentileText(median)}，P90 ${percentileText(p90)}`}>
        中位 {percentileText(median)} · P90 {percentileText(p90)}
      </div>
    </TiltCard>
  );
}

export function quotaColor(remainingPercent: number): string {
  if (remainingPercent >= 60) return chartSeriesColor(8);
  if (remainingPercent >= 20) return chartSeriesColor(5);
  return chartSeriesColor(9);
}

const QUOTA_PROVIDER_ORDER = ["codex", "claude", "copilot"];

function quotaProviderRank(provider: QuotaProviderDto): number {
  const rank = QUOTA_PROVIDER_ORDER.indexOf(provider.provider);
  return rank === -1 ? QUOTA_PROVIDER_ORDER.length : rank;
}

// Copilot reports chat and completions as unlimited windows; only premium requests carry a quota worth showing.
function visibleQuotaWindows(provider: QuotaProviderDto): QuotaWindowDto[] {
  if (provider.provider !== "copilot") return provider.windows;
  return provider.windows.filter((window) => /premium/i.test(window.name));
}

// Magpie names this provider "Claude Code", which is too long for a card title or badge.
function quotaProviderName(provider: QuotaProviderDto): string {
  return provider.provider === "claude" ? "Claude" : provider.name;
}

function quotaTitle(provider: QuotaProviderDto, window: QuotaWindowDto): string {
  if (provider.provider === "copilot") return "Copilot·requests";
  return `${quotaProviderName(provider)}·${window.name}`;
}

function QuotaWindowCard({ provider, window }: { provider: QuotaProviderDto; window: QuotaWindowDto }) {
  const used = Math.max(0, Math.min(100, window.used));
  const remaining = Math.round(100 - used);
  const resetText = window.resets_at_ms ? `下次重置：${formatQuotaResetTime(window.resets_at_ms)}` : "";

  return (
    <div className="flex h-full min-w-0 flex-col justify-between">
      <div className="flex h-4 min-w-0 items-center justify-between gap-2">
        <span className="truncate text-xs font-medium leading-4 text-foreground">{quotaTitle(provider, window)}</span>
        <span className="shrink-0 text-xs font-semibold leading-4 text-foreground" title={`${remaining}%`} aria-label={`${remaining}%`}>
          {window.unlimited ? "∞" : <NumberTicker value={remaining} blur format={(value) => `${value}%`} />}
        </span>
      </div>
      <div className="relative h-[5px] overflow-hidden rounded-full bg-muted" aria-label="剩余与已使用配额">
        {!window.unlimited ? <div className="absolute inset-y-0 left-0" style={{ width: `${remaining}%`, backgroundColor: quotaColor(remaining) }} /> : null}
        <div className="absolute inset-y-0 right-0" style={{ width: `${window.unlimited ? 0 : used}%`, backgroundColor: chartMuted }} />
      </div>
      <div className="flex h-4 min-w-0 items-center justify-between gap-2">
        <div className="min-w-0 flex-1 truncate text-xs leading-4 text-muted-foreground" title={window.display || undefined}>
          {resetText || window.display || provider.plan || "暂无重置信息"}
        </div>
        <Popover trigger="hover" side="bottom" align="end">
          <PopoverTrigger>
            <button type="button" aria-label={`${provider.name} 账户信息`} className="inline-flex h-4 shrink-0 items-center justify-center rounded-full border border-foreground/40 px-1 text-center text-[10px] font-medium leading-[10px] whitespace-nowrap text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2">
              {provider.plan || quotaProviderName(provider)}
            </button>
          </PopoverTrigger>
          <PopoverContent inverseTheme className="w-max max-w-64 text-xs">
            <div className="flex flex-col gap-1"><div>{provider.user || "—"}</div>{provider.balance ? <div>{provider.balance}</div> : null}</div>
          </PopoverContent>
        </Popover>
      </div>
    </div>
  );
}

export function AccountQuotaCard({
  quota,
  onRefresh,
  refreshing,
  refreshError,
  refreshAvailable,
}: {
  quota: QuotaResponse;
  onRefresh: () => void;
  refreshing: boolean;
  refreshError: boolean;
  refreshAvailable: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const shellRef = useRef<HTMLDivElement>(null);
  const hasFocus = useRef(false);
  const hover = useHoverGesture();
  const reduce = useReducedMotion();
  const items: NotificationStackItem[] = [];

  for (const provider of [...quota.items].sort((a, b) => quotaProviderRank(a) - quotaProviderRank(b))) {
    for (const window of visibleQuotaWindows(provider)) {
      items.push({ id: `${provider.provider}:${window.name}`, content: <QuotaWindowCard provider={provider} window={window} /> });
    }
  }
  const emptyMessage = quota.reading ? "正在读取账户额度…" : quota.items.some((item) => item.error)
    ? quota.items.find((item) => item.error)?.error ?? "额度读取失败"
    : "暂无可用额度";

  return (
    <div
      ref={shellRef}
      role="group"
      aria-labelledby="account-quota-title"
      className={`dashboard-quota-shell relative h-[144px] w-full overflow-visible text-foreground max-[1440px]:col-span-2 max-[1024px]:col-span-1 ${expanded ? "z-50" : "z-0"}`}
      onPointerEnter={(event) => {
        if (hover.enter(event)) setExpanded(true);
      }}
      onPointerLeave={(event) => {
        if (hover.leave(event) && !hasFocus.current) setExpanded(false);
      }}
      onFocusCapture={() => {
        hasFocus.current = true;
        setExpanded(true);
      }}
      onBlurCapture={(event) => {
        if (event.currentTarget.contains(event.relatedTarget as Node | null)) return;
        hasFocus.current = false;
        setExpanded(false);
      }}
    >
      <div className="dashboard-quota-inner absolute left-0 top-0 min-h-[144px] w-full overflow-visible rounded-3xl p-[10px] text-foreground">
        <motion.div
          aria-hidden
          layout="size"
          initial={false}
          transition={reduce ? { duration: 0 } : { duration: 0.26, ease: EASE_OUT }}
          className="absolute inset-0 rounded-3xl bg-muted"
        />
        <div className="relative z-10">
          <div className="flex h-5 min-w-0 items-center justify-between gap-2">
            <div id="account-quota-title" className="pl-[2px] text-xs font-medium leading-4 text-foreground">账户额度</div>
            <Button
              variant="ghost"
              size="icon"
              className={`h-5 w-5 shrink-0 rounded-md p-0 ${refreshError ? "text-destructive" : "text-muted-foreground"}`}
              disabled={refreshing || !refreshAvailable}
              aria-label={refreshError ? "刷新失败，重试账户额度" : "刷新账户额度"}
              title={refreshError ? "刷新失败，点击重试" : "刷新账户额度"}
              onClick={onRefresh}
            >
              <RefreshCw aria-hidden className={`h-3.5 w-3.5 ${refreshing ? "animate-spin" : ""}`} />
            </Button>
          </div>
          <div className="dashboard-quota-stack-wrap mt-1 w-full">
            {items.length > 0 ? (
              <NotificationStack
                items={items}
                expanded={expanded}
                dismissRef={shellRef}
                onExpandedChange={setExpanded}
                className="dashboard-quota-stack relative h-[100px] w-full outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
              />
            ) : (
              <div className="dashboard-quota-empty flex h-[100px] items-start pt-3 text-xs text-muted-foreground" aria-live="polite">{emptyMessage}</div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

export function SkeletonCard({ wide = false, bar = false }: { wide?: boolean; bar?: boolean }) {
  return (
    <div aria-hidden className={`dashboard-metric-card dashboard-metric-skeleton h-36 min-w-0 animate-pulse rounded-2xl border border-border bg-card p-4 ${wide ? "max-[1440px]:col-span-4 max-[1024px]:col-span-2" : "max-[1440px]:col-span-2 max-[1024px]:col-span-1"}`}>
      <div className="h-3 w-20 rounded bg-muted" />
      <div className="mt-2 h-8 w-28 rounded bg-muted" />
      {bar ? <div className="mt-1 h-[5px] rounded-full bg-muted" /> : null}
    </div>
  );
}

export const MetricGrid = memo(function MetricGrid({
  usage,
  requestMetrics,
  displayMode,
  sessionCount,
  sessionMedian,
  sessionP90,
  quota,
  onRefreshQuota,
  quotaRefreshing,
  quotaRefreshError,
  quotaRefreshAvailable,
}: MetricGridProps) {
  return (
    <div className={`dashboard-metric-grid grid gap-4 [grid-template-columns:minmax(0,1fr)_repeat(4,236px)] max-[1440px]:[grid-template-columns:repeat(6,minmax(0,1fr))] max-[1024px]:grid-cols-2`} aria-label={usage ? "KPI 指标" : "KPI 加载中"}>
      {!usage ? (
        <>
          <SkeletonCard wide bar />
          <SkeletonCard bar />
          <SkeletonCard />
          <SkeletonCard />
          <SkeletonCard bar />
        </>
      ) : (
        <>
          <TotalTokenMetric usage={usage} />
          <CacheHitMetric usage={usage} glare={false} />
          <CostMetric usage={usage} glare={false} />
          {displayMode === "session" ? (
            <>
              <SessionMetric count={sessionCount} median={sessionMedian} p90={sessionP90} />
              <AccountQuotaCard
                quota={quota}
                onRefresh={onRefreshQuota}
                refreshing={quotaRefreshing}
                refreshError={quotaRefreshError}
                refreshAvailable={quotaRefreshAvailable}
              />
            </>
          ) : requestMetrics ? (
            <>
              <RequestMetric metrics={requestMetrics} glare={false} />
              <OutputSpeedMetric metrics={requestMetrics} glare={false} />
            </>
          ) : (
            <>
              <SkeletonCard />
              <SkeletonCard />
            </>
          )}
        </>
      )}
    </div>
  );
});
