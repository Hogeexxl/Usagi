import { X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { SessionDetailResponse, SessionModelEffortDto, SessionThreadDto, UsageDto } from "../../data/types";
import { threadKeyId } from "../../data/usagiClient";
import { AnimatedToastStack, useAnimatedToastStack } from "../../ui/beui/animated-toast-stack";
import { BouncyAccordion } from "../../ui/beui/bouncy-accordion";
import { Button } from "../../ui/beui/button";
import { Drawer } from "../../ui/beui/drawer";
import { NumberTicker } from "../../ui/beui/number-ticker";
import { Tooltip } from "../../ui/beui/tooltip";
import { formatRatio } from "../format";
import { projectDisplay } from "../shared/projectDisplay";
import { CostValue } from "../shared/CostValue";
import {
  formatModelWithReasoningEffort,
  formatSessionIdForDisplay,
  formatSessionTime,
  formatSessionTimeWithSeconds,
  formatSessionTitle,
  formatSessionNullableTokenInteger,
  formatSessionTokenInteger,
} from "./sessionFormat";
import type { AgentDisplayLookup } from "../shared/agentDisplay";
import type { SessionDetailControllerViewModel } from "./useSessionDetailController";

type SessionDetailDrawerProps = {
  view: SessionDetailControllerViewModel;
  timezone: string;
  agentDisplay?: AgentDisplayLookup;
};

function StaticValue({ value, ariaLabel }: { value: string; ariaLabel: string }) {
  return <span className="tabular-nums text-foreground" aria-label={ariaLabel}>{value}</span>;
}

function ReceiptDivider() {
  return <div aria-hidden className="h-px w-full" style={{ backgroundImage: "repeating-linear-gradient(to right, var(--border-strong) 0 6px, transparent 6px 14px)" }} />;
}

function UsageReceipt({ usage, compactionTokens, showCompaction }: { usage: UsageDto; compactionTokens: number | null; showCompaction: boolean }) {
  const rows = [
    { label: "Total Tokens", value: formatSessionTokenInteger(usage.total), emphasized: true },
    { label: "Input", value: formatSessionTokenInteger(usage.input), emphasized: false },
    { label: "Output", value: formatSessionTokenInteger(usage.output), emphasized: false },
    { label: "Reasoning", value: formatSessionTokenInteger(usage.reasoning), emphasized: false },
    { label: "Cache Read", value: formatSessionTokenInteger(usage.cache_read), emphasized: false },
    { label: "Cache Write", value: formatSessionNullableTokenInteger(usage.cache_write), emphasized: false },
    { label: "Cache Write 1h", value: formatSessionNullableTokenInteger(usage.cache_write_1h), emphasized: false },
    { label: "Cache Hit Rate", value: formatRatio(usage.cache_hit_rate), emphasized: false },
  ] as const;
  const compactionValue = formatSessionNullableTokenInteger(compactionTokens);

  return (
    <dl className="space-y-2 text-sm">
      {rows.map(({ label, value, emphasized }) => (
        <div key={label} className="flex items-baseline justify-between gap-4">
          <dt className={emphasized ? "font-semibold text-foreground" : "text-muted-foreground"}>{label}</dt>
          <dd className={emphasized ? "font-semibold" : undefined}><StaticValue value={value.text} ariaLabel={`${label}：${value.accessibleName}`} /></dd>
        </div>
      ))}
      {showCompaction ? <div className="flex items-baseline justify-between gap-4"><dt className="text-muted-foreground">Compaction</dt><dd><StaticValue value={compactionValue.text} ariaLabel={`Compaction：${compactionValue.accessibleName}`} /></dd></div> : null}
      <div className="flex items-baseline justify-between gap-4">
        <dt className="font-semibold text-foreground">Cost</dt>
        <dd className="font-semibold tabular-nums text-foreground"><CostValue value={usage.cost} unpriced={usage.unpriced} className="tabular-nums text-foreground" /></dd>
      </div>
    </dl>
  );
}

function ModelReceipt({ item, showCompaction }: { item: SessionModelEffortDto; showCompaction: boolean }) {
  return <UsageReceipt usage={item.usage} compactionTokens={item.compaction_tokens} showCompaction={showCompaction} />;
}

function ChildReceipt({ item, timezone }: { item: SessionThreadDto; timezone: string }) {
  const time = formatSessionTimeWithSeconds(item.last_usage_ms, timezone);
  return (
    <div>
      <dl className="grid grid-cols-[72px_minmax(0,1fr)] items-baseline gap-x-4 gap-y-2 text-sm">
        <dt className="text-muted-foreground">Thread ID</dt>
        <dd className="min-w-0 break-all text-right tabular-nums text-foreground">{formatSessionIdForDisplay(item.key.agent, item.key.thread_id)}</dd>
        <dt className="text-muted-foreground">Last Active</dt>
        <dd className="text-right tabular-nums text-foreground" title={time.title}>{time.text}</dd>
      </dl>
      {item.model_effort.map((modelUsage, index) => {
        const model = formatModelWithReasoningEffort(modelUsage.model, modelUsage.effort, false);
        return (
          <div key={`${modelUsage.model}:${modelUsage.effort}:${index}`}>
            <div className="my-4"><ReceiptDivider /></div>
            <dl className="grid grid-cols-[72px_minmax(0,1fr)] items-baseline gap-x-4 gap-y-2 text-sm">
              <dt className="text-muted-foreground">Model</dt>
              <dd className="min-w-0 truncate text-right text-foreground" title={model}>{model}</dd>
            </dl>
            <div className="mt-4"><ModelReceipt item={modelUsage} showCompaction={item.key.agent === "codex"} /></div>
          </div>
        );
      })}
    </div>
  );
}

function DetailSkeleton() {
  return (
    <div role="status" aria-label="Session 详情加载中">
      <section aria-label="Session 合计加载中"><div className="space-y-2 text-sm">{Array.from({ length: 6 }, (_, index) => <div key={index} className="flex items-baseline justify-between gap-4"><span className="h-4 w-28 animate-pulse rounded bg-muted" /><span className="h-4 w-20 animate-pulse rounded bg-muted" /></div>)}</div></section>
      <section aria-label="Main 加载中" className="mt-6"><div className="mb-2 h-4 w-16 animate-pulse rounded bg-muted" /><div className="space-y-2"><div className="h-[54px] animate-pulse rounded-[28px] bg-muted" /><div className="h-[54px] animate-pulse rounded-[28px] bg-muted" /></div></section>
      <section aria-label="Subagent 加载中" className="mt-6"><div className="mb-2 h-4 w-20 animate-pulse rounded bg-muted" /><div className="space-y-2"><div className="h-[54px] animate-pulse rounded-[28px] bg-muted" /><div className="h-[54px] animate-pulse rounded-[28px] bg-muted" /></div></section>
    </div>
  );
}

function SummaryReceipt({ detail, agentDisplay }: { detail: SessionDetailResponse; agentDisplay?: AgentDisplayLookup }) {
  const thread = detail.thread;
  const agent = agentDisplay ? agentDisplay(thread.key.agent) : thread.key.agent;
  const project = projectDisplay(thread.project);
  const projectPath = (thread.project.kind === "project" || thread.project.kind === "workspace") ? thread.project.path : undefined;
  const rows = [
    ["Main Tokens", detail.self_usage.total, false],
    ["Subagent Tokens", detail.subagent_usage.total, false],
    ["Total Tokens", detail.inclusive_usage.total, true],
  ] as const;

  return (
    <section aria-label="Session 合计">
      <dl className="space-y-2 text-sm">
        <div className="flex items-baseline justify-between gap-4"><dt className="text-muted-foreground">Agent</dt><dd className="text-foreground">{agent}</dd></div>
        <div className="flex items-baseline justify-between gap-4"><dt className="text-muted-foreground">Project</dt><dd className="text-foreground" title={projectPath}>{project}</dd></div>
        {rows.map(([label, value, emphasized]) => <div key={label} className="flex items-baseline justify-between gap-4"><dt className={emphasized ? "font-semibold text-foreground" : "text-muted-foreground"}>{label}</dt><dd className={emphasized ? "font-semibold text-foreground" : "text-foreground"}><NumberTicker value={value} locale blur={false} className="tabular-nums" /></dd></div>)}
        <div className="flex items-baseline justify-between gap-4"><dt className="font-semibold text-foreground">Cost</dt><dd className="font-semibold tabular-nums text-foreground"><CostValue value={detail.inclusive_usage.cost} unpriced={detail.inclusive_usage.unpriced} ticker tickerBlur={false} className="tabular-nums text-foreground" /></dd></div>
      </dl>
    </section>
  );
}

export function SessionDetailDrawer({ view, timezone, agentDisplay }: SessionDetailDrawerProps) {
  const toast = useAnimatedToastStack();
  const [mainOpen, setMainOpen] = useState<string | null>(null);
  const [childOpen, setChildOpen] = useState<string | null>(null);
  const [mountedMainItems, setMountedMainItems] = useState<Set<string>>(() => new Set());
  const [mountedChildItems, setMountedChildItems] = useState<Set<string>>(() => new Set());
  const previousRefreshError = useRef<string | undefined>(undefined);
  const detail = view.detail;
  const handleOpenChange = useCallback((open: boolean) => { if (!open) view.close_detail(); }, [view.close_detail]);
  const handleMainValueChange = useCallback((value: string | null) => {
    if (value) setMountedMainItems((current) => current.has(value) ? current : new Set(current).add(value));
    setMainOpen(value);
  }, []);
  const handleChildValueChange = useCallback((value: string | null) => {
    if (value) setMountedChildItems((current) => current.has(value) ? current : new Set(current).add(value));
    setChildOpen(value);
  }, []);

  useEffect(() => {
    setMainOpen(null);
    setChildOpen(null);
    setMountedMainItems(new Set());
    setMountedChildItems(new Set());
  }, [view.selected_thread_key_id]);

  useEffect(() => {
    if (view.refresh_error_code && view.refresh_error_code !== previousRefreshError.current) toast.showToast({ status: "error", title: "详情更新失败" });
    previousRefreshError.current = view.refresh_error_code;
  }, [view.refresh_error_code, toast.showToast]);

  const mainItems = useMemo(() => (detail?.thread.model_effort ?? []).map((item, index) => {
    const id = `${item.model}:${item.effort}:${index}`;
    return {
      id,
      title: formatModelWithReasoningEffort(item.model, item.effort, false),
      ...(mountedMainItems.has(id) ? { description: <ModelReceipt item={item} showCompaction={detail?.thread.key.agent === "codex"} /> } : {}),
    };
  }), [detail, mountedMainItems]);

  const childItems = useMemo(() => (detail?.children ?? []).map((item) => {
    const id = threadKeyId(item.key);
    return {
      id,
      title: <Tooltip content={formatSessionTitle(item.title)} side="top" wrapperClassName="max-w-full"><span className="block truncate">{formatSessionTitle(item.title)}</span></Tooltip>,
      ...(mountedChildItems.has(id) ? { description: <ChildReceipt item={item} timezone={timezone} /> } : {}),
    };
  }), [detail, mountedChildItems, timezone]);

  if (!view.selected_row && !view.open) return null;

  const selected = view.selected_row;
  const thread = detail?.thread ?? selected?.thread ?? null;
  const title = formatSessionTitle(thread?.title ?? null);
  const agent = thread?.key.agent ?? "";
  const threadId = thread?.key.thread_id ?? "";
  const timeValue = thread?.last_usage_ms ?? 0;
  const time = formatSessionTime(timeValue, timezone);

  return (
    <>
      <Drawer open={view.open} onOpenChange={handleOpenChange} side="right" ariaLabel="Session 详情" backdropClassName="backdrop-blur-none" className="w-[480px] max-w-full max-[480px]:w-screen [contain:layout_paint]">
        <div className="flex h-full min-w-0 flex-col overflow-hidden" aria-busy={view.load_state === "loading" || view.load_state === "refreshing"}>
          <header className="flex items-start justify-between gap-3 px-5 pt-5 pb-4">
            <div className="min-w-0 flex-1 space-y-1">
              <Tooltip content={title} side="bottom" wrapperClassName="max-w-full"><h2 id="session-detail-title" className="m-0 block truncate text-base font-semibold leading-6 text-foreground">{title}</h2></Tooltip>
              <p className="min-w-0 break-all text-sm leading-5 tabular-nums text-muted-foreground">{formatSessionIdForDisplay(agent, threadId)}</p>
              <time className="block text-sm leading-5 tabular-nums text-muted-foreground" dateTime={timeValue > 0 ? new Date(timeValue).toISOString() : undefined} title={time.title}>{time.text}</time>
            </div>
            <div className="shrink-0"><Button variant="ghost" size="icon" aria-label="关闭 Session 详情" onClick={view.close_detail}><X className="h-4 w-4" /></Button></div>
          </header>
          <div className="px-5"><ReceiptDivider /></div>
          <div className="min-h-0 min-w-0 flex-1 overflow-y-auto overflow-x-hidden px-5 pt-4 pb-5">
            {view.load_state === "loading" && !detail ? <DetailSkeleton /> : null}
            {view.load_state === "error" && !detail ? <div className="rounded-2xl border border-destructive/20 p-5 text-center" role="alert"><p className="m-0 text-sm text-destructive">Session 详情加载失败</p><Button variant="secondary" size="sm" className="mt-3" onClick={view.retry_detail}>重试</Button></div> : null}
            {detail ? (
              <div>
                <SummaryReceipt detail={detail} agentDisplay={agentDisplay} />
                <section aria-labelledby="drawer-main-heading" className="mt-6">
                  <h3 id="drawer-main-heading" className="mb-2 text-sm font-semibold text-foreground">Main ({mainItems.length})</h3>
                  <BouncyAccordion items={mainItems} value={mainOpen} onValueChange={handleMainValueChange} />
                </section>
                {childItems.length > 0 ? <section aria-labelledby="drawer-subagent-heading" className="mt-6"><h3 id="drawer-subagent-heading" className="mb-2 text-sm font-semibold text-foreground">Subagent ({childItems.length})</h3><BouncyAccordion items={childItems} value={childOpen} onValueChange={handleChildValueChange} /></section> : null}
              </div>
            ) : null}
          </div>
        </div>
      </Drawer>
      <AnimatedToastStack toasts={toast.toasts} onDismiss={toast.dismissToast} />
    </>
  );
}
