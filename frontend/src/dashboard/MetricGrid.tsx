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
  modelFilterActive: boolean;
  quota: QuotaResponse;
  onRefreshQuota: () => void;
  quotaRefreshing: boolean;
  quotaRefreshError: boolean;
  quotaRefreshAvailable: boolean;
};
type Focus = "input" | "cache_read" | "cache_write" | "output" | "reasoning" | null;
type CacheFocus = "cached" | "input" | "write" | null;

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

function Dot({ className }: { className: string }) {
  return (
    <span aria-hidden className="inline-flex h-4 shrink-0 items-center justify-center">
      <span className={`block h-1.5 w-1.5 rounded-full ${className}`} />
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
  const reasoning = Math.min(usage.reasoning, output);
  const total = usage.total;
  const inputPct = total > 0 ? (input / total) * 100 : 0;
  const cacheReadPct = total > 0 ? (cacheRead / total) * 100 : 0;
  const cacheWritePct = total > 0 ? (cacheWrite / total) * 100 : 0;
  const cacheWriteStartPct = inputPct + cacheReadPct;
  const outputStartPct = cacheWriteStartPct + cacheWritePct;
  const outputPct = total > 0 ? (output / total) * 100 : 0;
  const reasoningPct = total > 0 ? (reasoning / total) * 100 : 0;
  const reasoningStartPct = outputStartPct + outputPct - reasoningPct;
  const dim = (key: Exclude<Focus, null>) => focus !== null && focus !== key;
  const transition = reduce ? { duration: 0 } : { ease: EASE_OUT };

  return (
    <TiltCard className={`${CARD} flex flex-col min-w-0 max-[1439px]:col-span-2 max-[767px]:col-span-1`}>
      <div className={TITLE}>总 Token</div>
      <CompactTicker value={total} formatter={formatIntegerFull} />
      <div className="relative mt-1 h-[5px] overflow-hidden rounded-full bg-muted" aria-label="Token 构成：输入、缓存读、缓存写和输出；推理 Token 包含在输出 Token 中">
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
          className="absolute inset-y-0 bg-[#8660a8]"
          style={{ left: `${cacheWriteStartPct}%`, width: `${cacheWritePct}%` }}
          animate={{ opacity: dim("cache_write") ? 0.3 : 1, scaleY: focus === "cache_write" && !reduce ? 1.25 : 1 }}
          transition={transition}
        />
        <motion.div
          className="absolute inset-y-0 bg-[#be753e]"
          style={{ left: `${outputStartPct}%`, width: `${outputPct}%`, zIndex: focus === "output" ? 3 : 1 }}
          animate={{ opacity: dim("output") ? 0.3 : 1, scaleY: focus === "output" && !reduce ? 1.25 : 1 }}
          transition={transition}
        />
        <motion.div
          className="absolute inset-y-0 bg-[#a6333d]"
          style={{ left: `${reasoningStartPct}%`, width: `${reasoningPct}%`, zIndex: focus === "output" ? 0 : 2 }}
          animate={{ opacity: focus === "output" ? 0 : dim("reasoning") ? 0.3 : 1, scaleY: focus === "reasoning" && !reduce ? 1.25 : 1 }}
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
          <Dot className="bg-[#8660a8]" /><span className="inline-flex h-4 items-center">缓存写</span><CompactTicker value={cacheWrite} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button>
        <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("output")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("output")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#be753e]" /><span className="inline-flex h-4 items-center">输出</span><CompactTicker value={output} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button>
        <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("reasoning")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("reasoning")} onBlur={() => setFocus(null)} aria-label={`推理 ${formatIntegerFull(reasoning).accessibleName}，包含在输出 Token 中`}>
          <Dot className="bg-[#a6333d]" /><span className="inline-flex h-4 items-center">推理</span><CompactTicker value={reasoning} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
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
  const cacheTotal = input + cached + cacheWrite;
  const cachedPct = cacheTotal > 0 ? Math.min(1, Math.max(0, cached / cacheTotal)) * 100 : 0;
  const inputPct = cacheTotal > 0 ? (input / cacheTotal) * 100 : 0;
  const writePct = cacheTotal > 0 ? (cacheWrite / cacheTotal) * 100 : 0;

  return (
    <TiltCard glare={glare} className={`${CARD} flex flex-col`}>
      <div className={TITLE}>缓存命中</div>
      {rate === null ? (
        <div className={VALUE}>—</div>
      ) : (
        <CompactTicker value={rate} tickerValue={Math.round(rate * 1000)} formatter={formatRatio} tickerFormatter={(next) => formatRatio(next / 1000).text} />
      )}
      <motion.div className="relative mt-1 h-[5px] overflow-hidden rounded-full bg-muted" aria-label="缓存命中率分母包括输入、缓存读和缓存写" animate={{ scaleY: focus !== null && !reduce ? 1.12 : 1 }} transition={reduce ? { duration: 0 } : { ease: EASE_OUT }}>
        <motion.div className="absolute inset-y-0 left-0 bg-[#be506e]" style={{ width: `${cachedPct}%` }} animate={{ opacity: focus === null || focus === "cached" ? 1 : 0.3 }} transition={reduce ? { duration: 0 } : { ease: EASE_OUT }} />
        <motion.div className="absolute inset-y-0 bg-[#4057a5]" style={{ left: `${cachedPct}%`, width: `${inputPct}%` }} animate={{ opacity: focus === null || focus === "input" ? 1 : 0.3 }} transition={reduce ? { duration: 0 } : { ease: EASE_OUT }} />
        <motion.div className="absolute inset-y-0 right-0 bg-[#8660a8]" style={{ width: `${writePct}%` }} animate={{ opacity: focus === null || focus === "write" ? 1 : 0.3 }} transition={reduce ? { duration: 0 } : { ease: EASE_OUT }} />
      </motion.div>
      <div className="mt-auto flex items-center gap-2 whitespace-nowrap">
        <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("cached")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("cached")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#be506e]" /><span className="inline-flex h-4 items-center">缓存读</span><CompactTicker value={cached} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button>
        <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("input")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("input")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#4057a5]" /><span className="inline-flex h-4 items-center">输入</span><CompactTicker value={input} formatter={formatIntegerFull} className="h-4 text-foreground" wrapperClassName="inline-flex h-4 items-center" />
        </button>
        <button type="button" className={`${LEGEND} flex items-center gap-1`} onPointerEnter={() => setFocus("write")} onPointerLeave={() => setFocus(null)} onFocus={() => setFocus("write")} onBlur={() => setFocus(null)}>
          <Dot className="bg-[#8660a8]" /><span className="inline-flex h-4 items-center" aria-label={`缓存写 ${formatIntegerFull(cacheWrite).accessibleName}`}>缓存写</span>
        </button>
      </div>
    </TiltCard>
  );
}

function SessionCountMetric({ count }: { count: number }) {
  return (
    <TiltCard glare={false} className={`${CARD} flex flex-col`}>
      <div className={TITLE}>会话数量</div>
      <CompactTicker value={count} formatter={formatIntegerFull} />
      <div className={`${LEGEND} mt-auto`}>仅统计主线程会话。</div>
    </TiltCard>
  );
}

export function CostMetric({ usage, glare = true }: { usage: UsageDto; glare?: boolean }) {
  const hasUnpriced = usage.unpriced > 0;
  const costPerMillionTokens = formatCostPerMillionTokens(usage.total > 0 ? (usage.cost / usage.total) * 1_000_000 : null);

  return (
    <TiltCard glare={glare} className={`${CARD} flex flex-col`}>
      <div className="flex items-center justify-between gap-2">
        <div className={TITLE}>费用</div>
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

export function quotaColor(remainingPercent: number): string {
  if (remainingPercent >= 60) return chartSeriesColor(8);
  if (remainingPercent >= 20) return chartSeriesColor(5);
  return chartSeriesColor(9);
}

function QuotaWindowCard({ provider, window }: { provider: QuotaProviderDto; window: QuotaWindowDto }) {
  const used = Math.max(0, Math.min(100, window.used * 100));
  const remaining = Math.round(100 - used);

  return (
    <div className="flex h-full min-w-0 flex-col justify-between">
      <div className="flex h-4 min-w-0 items-center justify-between gap-2">
        <span className="truncate text-xs font-medium leading-4 text-foreground">{provider.name}·{window.name}</span>
        <span className="shrink-0 text-xs font-semibold leading-4 text-foreground" title={`${remaining}%`} aria-label={`${remaining}%`}>
          {window.unlimited ? "∞" : <NumberTicker value={remaining} blur format={(value) => `${value}%`} />}
        </span>
      </div>
      <div className="relative h-[5px] overflow-hidden rounded-full bg-muted" aria-label="剩余与已使用配额">
        {!window.unlimited ? <div className="absolute inset-y-0 left-0" style={{ width: `${remaining}%`, backgroundColor: quotaColor(remaining) }} /> : null}
        <div className="absolute inset-y-0 right-0" style={{ width: `${window.unlimited ? 0 : used}%`, backgroundColor: chartMuted }} />
      </div>
      <div className="flex h-4 min-w-0 items-center justify-between gap-2">
        <div className="min-w-0 flex-1 truncate text-xs leading-4 text-muted-foreground">
          {window.display || (window.resets_at_ms ? `下次重置：${formatQuotaResetTime(window.resets_at_ms)}` : provider.plan || "暂无重置信息")}
        </div>
        <Popover trigger="hover" side="bottom" align="end">
          <PopoverTrigger>
            <button type="button" aria-label={`${provider.name} 账户信息`} className="inline-flex h-4 shrink-0 items-center justify-center rounded-full border border-foreground/40 px-1 text-center text-[10px] font-medium leading-[10px] whitespace-nowrap text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2">
              {provider.plan || provider.name}
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

  for (const provider of quota.items) {
    for (const window of provider.windows) {
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
  modelFilterActive,
  quota,
  onRefreshQuota,
  quotaRefreshing,
  quotaRefreshError,
  quotaRefreshAvailable,
}: MetricGridProps) {
  const columns = modelFilterActive
    ? "[grid-template-columns:minmax(0,1fr)_repeat(3,236px)]"
    : "[grid-template-columns:minmax(0,1fr)_repeat(4,236px)]";

  return (
    <div className={`grid gap-4 ${columns} max-[1439px]:[grid-template-columns:minmax(0,1fr)_236px] max-[767px]:grid-cols-1`} aria-label={usage ? "KPI 指标" : "KPI 加载中"}>
      {!usage ? (
        <>
          <SkeletonCard wide bar />
          <SkeletonCard bar />
          {!modelFilterActive ? <SkeletonCard /> : null}
          <SkeletonCard />
          <SkeletonCard bar />
        </>
      ) : (
        <>
          <TotalTokenMetric usage={usage} />
          <CacheHitMetric usage={usage} glare={false} />
          {!modelFilterActive ? <SessionCountMetric count={sessionCount} /> : null}
          <CostMetric usage={usage} glare={false} />
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
