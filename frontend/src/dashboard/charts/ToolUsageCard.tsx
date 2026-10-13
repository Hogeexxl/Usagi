import { motion, useReducedMotion } from "motion/react";
import { useEffect, useMemo, useState } from "react";

import { NumberTicker } from "../../ui/beui/number-ticker";
import { Tabs, TabsList, TabsTrigger } from "../../ui/beui/tabs";
import { SPRING_LAYOUT } from "../../ui/lib/ease";
import { formatCompact, formatIntegerFull } from "../format";
import { chartMuted, chartSeriesColor } from "./chartPalette";
import { focusOpacity } from "./chartMotion";
import { ChartSurface } from "./ChartSurface";
import type { ToolUsageControllerView, ToolUsageRange } from "./useToolUsageController";

const DONUT_RADIUS = 65.5;
const DONUT_STROKE_WIDTH = 8;
const DONUT_PATH_LENGTH = 100;
const DONUT_GAP_PX = 2;
const DONUT_PATH_UNITS_PER_PIXEL = DONUT_PATH_LENGTH / (2 * Math.PI * DONUT_RADIUS);
const DONUT_CAP_UNITS = DONUT_STROKE_WIDTH * DONUT_PATH_UNITS_PER_PIXEL;
const DONUT_GAP_UNITS = DONUT_GAP_PX * DONUT_PATH_UNITS_PER_PIXEL;

type ToolSegment = { id: string; name: string; calls: number; percentage: number; isOther?: boolean };

const RANGE_LABELS: Array<{ value: ToolUsageRange; label: string }> = [
  { value: "today", label: "今天" },
  { value: "7d", label: "7 天" },
  { value: "30d", label: "30 天" },
];

export function ToolUsageCard({ view }: { view: ToolUsageControllerView }) {
  const reduce = useReducedMotion();
  const [focusedId, setFocusedId] = useState<string | null>(null);
  const response = view.response?.days === (view.range === "today" ? 1 : view.range === "7d" ? 7 : 30) ? view.response : null;
  const segments = useMemo<ToolSegment[]>(() => {
    if (!response || response.calls === 0) return [];
    const top = response.top.slice(0, 8);
    const topCalls = top.reduce((sum, item) => sum + item.calls, 0);
    const items: Array<Omit<ToolSegment, "percentage">> = top.map((item) => ({
      id: item.name,
      name: item.name,
      calls: item.calls,
    }));
    const otherCalls = response.calls - topCalls;
    if (otherCalls > 0) items.push({ id: "__other__", name: "其他", calls: otherCalls, isOther: true });
    return items.map((item) => ({ ...item, percentage: item.calls / response.calls }));
  }, [response]);
  const ids = useMemo(() => new Set(segments.map((segment) => segment.id)), [segments]);
  useEffect(() => {
    if (focusedId !== null && !ids.has(focusedId)) setFocusedId(null);
  }, [focusedId, ids]);

  const calls = response?.calls ?? 0;
  const focused = segments.find((segment) => segment.id === focusedId) ?? null;
  const hasMultipleSegments = segments.length > 1;
  let offset = 0;
  const centerValue = focused ? focused.calls : calls;
  const centerLabel = focused ? `${(focused.percentage * 100).toFixed(1)}%` : "调用次数";
  const emptyText = view.loading ? "工具使用加载中…" : view.error ? "工具使用加载失败" : "暂无工具使用数据";

  return (
    <ChartSurface className="h-[264px]">
      <header className="flex items-center justify-between gap-4">
        <h2 className="m-0 text-sm font-medium text-foreground">Tool 使用分布</h2>
        <Tabs value={view.range} onValueChange={(value) => view.select_range(value as ToolUsageRange)} variant="pill">
          <TabsList>
            {RANGE_LABELS.map((range) => <TabsTrigger key={range.value} value={range.value}>{range.label}</TabsTrigger>)}
          </TabsList>
        </Tabs>
      </header>
      {view.error ? <div className="mt-1 text-xs text-destructive" role="status">工具使用加载失败</div> : null}
      <div className="mt-4 grid min-w-0 grid-cols-[140px_minmax(0,1fr)] items-start gap-8">
        <div className="relative h-[140px] w-[140px]">
          <svg className="h-[140px] w-[140px] -rotate-90" width="140" height="140" viewBox="0 0 140 140" role="img" aria-label="Tool 调用分布">
            {segments.length === 0 ? <circle cx="70" cy="70" r={DONUT_RADIUS} pathLength={DONUT_PATH_LENGTH} fill="none" stroke={chartMuted} strokeWidth={DONUT_STROKE_WIDTH} /> : null}
            {segments.map((segment, index) => {
              const percent = segment.percentage * 100;
              const current = offset;
              offset += percent;
              const inset = hasMultipleSegments ? DONUT_CAP_UNITS + DONUT_GAP_UNITS : 0;
              const dashLength = hasMultipleSegments ? Math.max(0, percent - inset) : percent;
              const dashStart = hasMultipleSegments ? current + inset / 2 : current;
              return (
                <motion.circle
                  key={segment.id}
                  cx="70"
                  cy="70"
                  r={DONUT_RADIUS}
                  pathLength={DONUT_PATH_LENGTH}
                  fill="none"
                  stroke={chartSeriesColor(index, segment.isOther === true)}
                  strokeWidth={DONUT_STROKE_WIDTH}
                  strokeLinecap="round"
                  strokeDasharray={`${dashLength} ${DONUT_PATH_LENGTH - dashLength}`}
                  strokeDashoffset={-dashStart}
                  animate={{ opacity: focusOpacity(focusedId, segment.id) }}
                  transition={reduce ? { duration: 0 } : SPRING_LAYOUT}
                  onPointerEnter={() => setFocusedId(segment.id)}
                  onPointerLeave={() => setFocusedId(null)}
                />
              );
            })}
          </svg>
          <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center text-center">
            <NumberTicker value={centerValue} blur format={formatCompact} className="text-lg font-semibold leading-6 text-foreground" />
            <span className="mt-0.5 max-w-[128px] truncate text-xs leading-4 text-muted-foreground">{centerLabel}</span>
          </div>
        </div>
        <div className="flex min-w-0 flex-col">
          {segments.map((segment, index) => (
            <motion.button
              type="button"
              key={segment.id}
              title={segment.name}
              aria-label={`${segment.name}，${formatIntegerFull(segment.calls).text} 次，${(segment.percentage * 100).toFixed(1)}%`}
              className="grid h-3.5 w-full grid-cols-[10px_minmax(0,1fr)_auto_48px] items-center gap-x-2 text-xs leading-3 text-muted-foreground"
              animate={{ opacity: focusOpacity(focusedId, segment.id) }}
              transition={reduce ? { duration: 0 } : SPRING_LAYOUT}
              onPointerEnter={() => setFocusedId(segment.id)}
              onPointerLeave={() => setFocusedId(null)}
              onFocus={() => setFocusedId(segment.id)}
              onBlur={() => setFocusedId(null)}
            >
              <span className="h-[10px] w-[10px] rounded-full" style={{ background: chartSeriesColor(index, segment.isOther === true) }} />
              <span className="min-w-0 truncate text-left">{segment.name}</span>
              <span className="tabular-nums" title={`${segment.calls} 次`}>{formatIntegerFull(segment.calls).text}</span>
              <span className="w-12 text-right tabular-nums">{(segment.percentage * 100).toFixed(1)}%</span>
            </motion.button>
          ))}
          {segments.length === 0 ? <div className="py-8 text-center text-xs text-muted-foreground" role="status">{emptyText}</div> : null}
        </div>
      </div>
    </ChartSurface>
  );
}
