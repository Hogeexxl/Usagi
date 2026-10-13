import { CircleAlert, RefreshCw } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";
import { memo, useRef, useState } from "react";

import type { QuotaProviderDto, QuotaResponse, QuotaWindowDto, UsageDto } from "../data/types";
import { chartMuted, chartSeriesColor } from "./charts/chartPalette";
import { EASE_OUT } from "../ui/lib/ease";
import { NumberTicker } from "../ui/beui/number-ticker";
import { Popover, PopoverContent, PopoverTrigger } from "../ui/beui/popover";
import { Button } from "../ui/beui/button";
import { NotificationStack, type NotificationStackItem } from "../ui/beui/notification-stack";
import { TiltCard } from "../ui/beui/tilt-card";
import { formatQuotaResetTime, formatCostFull, formatCostPerMillionTokens, formatIntegerFull, formatRatio, type FormattedValue } from "./format";
import { useHoverGesture } from "../ui/lib/use-hover-gesture";

type MetricGridProps = {
  usage: UsageDto | null;
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

const CARD = "h-36 min-w-0 border border-border bg-card p-4 text-foreground";
const TITLE = "text-xs font-medium leading-4 text-foreground";
const VALUE = "mt-2 text-[28px] font-semibold leading-8 tracking-tight text-foreground";
const LEGEND = "h-4 text-xs leading-4 text-muted-foreground";

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
    <TiltCard className={`${CARD} flex flex-col min-w-0 max-[1439px]:col-span-2 max-[767px]:col-span-1`}>
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
      <div className="mt-auto flex min-w-0 items-center gap-1 whitespace-nowrap">
        <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("input")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("input")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#68c0e8]" /><span className="inline-flex h-4 items-center">输入</span><CompactTicker value={input} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button>
        <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("cache_read")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("cache_read")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#be506e]" /><span className="inline-flex h-4 items-center">缓存读</span><CompactTicker value={cacheRead} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button>
        <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("cache_write")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("cache_write")} onBlur={() => setFocus(null)}>
          <Dot color={chartSeriesColor(3)} /><span className="inline-flex h-4 items-center">缓存写</span><CompactTicker value={cacheWrite} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button>
        <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("output")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("output")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#be753e]" /><span className="inline-flex h-4 items-center">输出</span><CompactTicker value={output} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button>
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
    <TiltCard glare={glare} className={`${CARD} flex flex-col`}>
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
      <div className="mt-auto flex items-center gap-2 whitespace-nowrap">
        <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("cached")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("cached")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#be506e]" /><span className="inline-flex h-4 items-center">缓存读</span><CompactTicker value={cached} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button>
        <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("input")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("input")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#4057a5]" /><span className="inline-flex h-4 items-center">输入</span><CompactTicker value={input} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button>
        <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("cache_write")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("cache_write")} onBlur={() => setFocus(null)}>
          <Dot color={chartSeriesColor(3)} /><span className="inline-flex h-4 items-center">缓存写</span><CompactTicker value={cacheWrite} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button>
      </div>
    </TiltCard>
  );
}

export function RequestMetric({ usage, glare = false }: { usage: UsageDto; glare?: boolean }) {
  const hasFailures = usage.errors > 0;
  const successRate = usage.calls > 0 ? formatRatio((usage.calls - usage.errors) / usage.calls).text : "—";
  const speed = usage.output_speed === null || usage.speed_samples === 0
    ? "—"
    : `${formatIntegerFull(Math.round(usage.output_speed)).title} token/s（${formatIntegerFull(usage.speed_samples).title} 次）`;

  return (
    <TiltCard glare={glare} className={`${CARD} flex flex-col`}>
      <div className="flex items-center justify-between gap-2">
        <div className={TITLE}>请求数</div>
        <Popover trigger="hover" side="bottom" align="end">
          <PopoverTrigger>
            <button
              type="button"
              aria-label="请求失败与重试提示"
              className={`inline-flex items-center justify-center ${hasFailures ? "text-warning" : "text-foreground"} outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2`}
            >
              <CircleAlert className="h-4 w-4" />
            </button>
          </PopoverTrigger>
          <PopoverContent inverseTheme className="w-max max-w-64 text-xs">
            <div className="flex flex-col gap-1">
              <div>{formatIntegerFull(usage.errors).title} 次失败 · 成功 {successRate}</div>
              <div>{formatIntegerFull(usage.retried).title} 次重试后成功</div>
            </div>
          </PopoverContent>
        </Popover>
      </div>
      <CompactTicker value={usage.calls} formatter={formatIntegerFull} />
      <div className={`${LEGEND} mt-auto truncate`} title={`输出速度：${speed}`}>输出速度：{speed}</div>
    </TiltCard>
  );
}

export function CostMetric({ usage, glare = true }: { usage: UsageDto; glare?: boolean }) {
  const costPerMillionTokens = formatCostPerMillionTokens(usage.total > 0 ? (usage.cost / usage.total) * 1_000_000 : null);

  return (
    <TiltCard glare={glare} className={`${CARD} flex flex-col`}>
      <div className={TITLE}>等价费用</div>
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
    <TiltCard glare={false} className={`${CARD} flex flex-col`}>
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
  wide = false,
}: {
  quota: QuotaResponse;
  onRefresh: () => void;
  refreshing: boolean;
  refreshError: boolean;
  refreshAvailable: boolean;
  wide?: boolean;
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
      className={`relative h-[144px] ${wide ? "w-[304px]" : "w-[236px]"} overflow-visible text-foreground ${expanded ? "z-50" : "z-0"}`}
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
      <div className="absolute left-0 top-0 min-h-[144px] w-full overflow-visible rounded-3xl p-[10px] text-foreground">
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
          <div className={`mt-1 ${wide ? "w-[284px]" : "w-[216px]"}`}>
            {items.length > 0 ? (
              <NotificationStack
                items={items}
                expanded={expanded}
                dismissRef={shellRef}
                onExpandedChange={setExpanded}
                className={`relative h-[100px] ${wide ? "w-[284px]" : "w-[216px]"} outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background`}
              />
            ) : (
              <div className="flex h-[100px] items-start pt-3 text-xs text-muted-foreground" aria-live="polite">{emptyMessage}</div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

export function SkeletonCard({ wide = false, bar = false }: { wide?: boolean; bar?: boolean }) {
  return (
    <div aria-hidden className={`h-36 animate-pulse rounded-2xl border border-border bg-card p-4 ${wide ? "min-w-0 max-[1439px]:col-span-2 max-[767px]:col-span-1" : ""}`}>
      <div className="h-3 w-20 rounded bg-muted" />
      <div className="mt-2 h-8 w-28 rounded bg-muted" />
      {bar ? <div className="mt-1 h-[5px] rounded-full bg-muted" /> : null}
    </div>
  );
}

export const MetricGrid = memo(function MetricGrid({
  usage,
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
    <div className={`grid gap-4 [grid-template-columns:minmax(0,1fr)_repeat(4,236px)] max-[1439px]:[grid-template-columns:minmax(0,1fr)_236px] max-[767px]:grid-cols-1`} aria-label={usage ? "KPI 指标" : "KPI 加载中"}>
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
          <SessionMetric count={sessionCount} median={sessionMedian} p90={sessionP90} />
          <AccountQuotaCard
            quota={quota}
            onRefresh={onRefreshQuota}
            refreshing={quotaRefreshing}
            refreshError={quotaRefreshError}
            refreshAvailable={quotaRefreshAvailable}
          />
        </>
      )}
    </div>
  );
});
