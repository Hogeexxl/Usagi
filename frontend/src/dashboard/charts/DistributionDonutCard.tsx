import { motion, useReducedMotion } from "motion/react";
import { useEffect, useMemo, useState } from "react";
import { NumberTicker } from "../../ui/beui/number-ticker";
import { Tabs, TabsList, TabsTrigger } from "../../ui/beui/tabs";
import { SPRING_LAYOUT } from "../../ui/lib/ease";
import { formatCompact, formatCompactCost } from "../format";
import { chartMuted, chartSeriesColor } from "./chartPalette";
import { focusOpacity } from "./chartMotion";
import { ChartSurface } from "./ChartSurface";
import { buildDistribution, type DistributionItem, type DistributionMetric } from "./distribution";

const DONUT_RADIUS = 65.5;
const DONUT_STROKE_WIDTH = 8;
const DONUT_PATH_LENGTH = 100;
const DONUT_GAP_PX = 2;
const DONUT_PATH_UNITS_PER_PIXEL = DONUT_PATH_LENGTH / (2 * Math.PI * DONUT_RADIUS);
const DONUT_CAP_UNITS = DONUT_STROKE_WIDTH * DONUT_PATH_UNITS_PER_PIXEL;
const DONUT_GAP_UNITS = DONUT_GAP_PX * DONUT_PATH_UNITS_PER_PIXEL;

function metricText(metric: DistributionMetric, value: number) {
  return metric === "cost" ? formatCompactCost(value) : formatCompact(Math.round(value));
}

export function DistributionDonutCard({ modelItems, projectItems }: { modelItems: DistributionItem[]; projectItems: DistributionItem[] }) {
  const [dimension, setDimension] = useState<"model" | "project">("model");
  const [metric, setMetric] = useState<DistributionMetric>("tokens");
  const [focusedId, setFocusedId] = useState<string | null>(null);
  const reduce = useReducedMotion();
  const title = dimension === "model" ? "模型分布" : "项目分布";
  const items = dimension === "model" ? modelItems : projectItems;
  const data = useMemo(() => buildDistribution(items, metric), [items, metric]);
  useEffect(() => {
    if (focusedId !== null && !data.segments.some((segment) => segment.id === focusedId)) setFocusedId(null);
  }, [data.segments, focusedId]);
  const focused = data.segments.find((segment) => segment.id === focusedId) ?? null;
  const visibleSegments = data.segments.filter((segment) => segment.percentage > 0);
  const hasData = visibleSegments.length > 0;
  const hasMultipleSegments = visibleSegments.length > 1;
  let offset = 0;
  const centerValue = focused ? focused.value : data.total;
  const centerLabel = focused
    ? `${(focused.percentage * 100).toFixed(1)}%`
    : metric === "tokens"
      ? "Token"
      : "Cost";

  return (
    <ChartSurface className="h-[264px]">
      <header className="flex items-center justify-between gap-4">
        <Tabs value={dimension} onValueChange={(value) => { setDimension(value as "model" | "project"); setFocusedId(null); }} variant="pill">
          <TabsList>
            <TabsTrigger value="model">模型</TabsTrigger>
            <TabsTrigger value="project">项目</TabsTrigger>
          </TabsList>
        </Tabs>
        <Tabs value={metric} onValueChange={(value) => { setMetric(value as DistributionMetric); setFocusedId(null); }} variant="pill">
          <TabsList>
            <TabsTrigger value="tokens">Token</TabsTrigger>
            <TabsTrigger value="cost">费用</TabsTrigger>
          </TabsList>
        </Tabs>
      </header>
      <div className="mt-4 grid min-w-0 grid-cols-[140px_minmax(0,1fr)] items-start gap-8">
        <div className="relative h-[140px] w-[140px]">
          <svg
            className="h-[140px] w-[140px] -rotate-90"
            width="140"
            height="140"
            viewBox="0 0 140 140"
            role="img"
            aria-label={`${title}${metric === "tokens" ? "Token" : "费用"}分布`}
          >
            {!hasData ? <circle cx="70" cy="70" r={DONUT_RADIUS} pathLength={DONUT_PATH_LENGTH} fill="none" stroke={chartMuted} strokeWidth={DONUT_STROKE_WIDTH} /> : null}
            {data.segments.map((segment, index) => {
              if (segment.percentage <= 0) return null;
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
                  strokeDasharray={`${dashLength} ${DONUT_PATH_LENGTH - dashLength}`} strokeDashoffset={-dashStart}
                  animate={{ opacity: focusOpacity(focusedId, segment.id) }}
                  transition={reduce ? { duration: 0 } : SPRING_LAYOUT}
                  onPointerEnter={() => setFocusedId(segment.id)} onPointerLeave={() => setFocusedId(null)}
                />
              );
            })}
          </svg>
          <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center text-center">
            {metric === "cost" ? <NumberTicker value={Math.round(centerValue * 100)} blur format={(next) => formatCompactCost(next / 100)} className="text-lg font-semibold leading-6 text-foreground" /> : <NumberTicker value={Math.round(centerValue)} blur format={formatCompact} className="text-lg font-semibold leading-6 text-foreground" />}
            <span className="mt-0.5 max-w-[128px] truncate text-xs leading-4 text-muted-foreground">{centerLabel}</span>
          </div>
        </div>
        <div className="flex min-w-0 flex-col gap-0.5">
          {data.segments.map((segment, index) => (
            <motion.button
              type="button"
              key={segment.id}
              title={segment.title}
              className="grid h-5 w-full grid-cols-[10px_minmax(0,1fr)_auto_48px] items-center gap-x-2 text-xs leading-4 text-muted-foreground"
              animate={{ opacity: focusOpacity(focusedId, segment.id) }}
              transition={reduce ? { duration: 0 } : SPRING_LAYOUT}
              onPointerEnter={() => setFocusedId(segment.id)} onPointerLeave={() => setFocusedId(null)} onFocus={() => setFocusedId(segment.id)} onBlur={() => setFocusedId(null)}
            >
              <span className="h-[10px] w-[10px] rounded-full" style={{ background: chartSeriesColor(index, segment.isOther === true) }} />
              <span className="min-w-0 truncate text-left">{segment.label}</span>
              <span className="tabular-nums">{metricText(metric, segment.value)}</span>
              <span className="w-12 text-right tabular-nums">{(segment.percentage * 100).toFixed(1)}%</span>
            </motion.button>
          ))}
          {data.segments.length === 0 ? <div className="py-8 text-center text-xs text-muted-foreground">暂无数据</div> : null}
        </div>
      </div>
    </ChartSurface>
  );
}
