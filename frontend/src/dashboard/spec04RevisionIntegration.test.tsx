import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RevisionFeed, type RevisionEventSource } from "../data/revisionFeed";
import { usagiClient } from "../data/usagiClient";
import type { DashboardFilters, DashboardRange, RevisionTuple, SessionListItemDto, SessionThreadDto, ThreadKey, UsageDto } from "../data/types";
import { useDashboardChartsController } from "./charts/useDashboardChartsController";
import { useSessionDetailController } from "./session/useSessionDetailController";
import { useSessionTableController } from "./session/useSessionTableController";
import { useDashboardQuotaController } from "./useDashboardQuotaController";
import { useDashboardController } from "./useDashboardController";

const range: DashboardRange = { key: "today" };
const filters: DashboardFilters = { agents: [], models: [], projects: [] };
const key: ThreadKey = { agent: "codex", session_id: "session-1", thread_id: "thread-1" };
const applicationReadPaths = new Set([
  "/api/usage/filter-options",
  "/api/usage/model-distribution",
  "/api/usage/projects",
  "/api/usage/sessions",
  "/api/usage/session-detail",
]);

function usage(revision: number): UsageDto {
  const input = revision === 0 ? 17 : revision;
  const output = 3;
  const cache_read = 2;
  const cache_write = 1;
  const reasoning = 1;
  return {
    calls: 1,
    errors: 0,
    input,
    output,
    cache_read,
    cache_write,
    cache_write_1h: 0,
    reasoning,
    total: input + output + cache_read + cache_write,
    cache_hit_rate: cache_read / (input + cache_read + cache_write),
    cost: revision / 100,
    unpriced: 0,
    retried: 0, output_speed: null, speed_samples: 0,
  };
}

function zeroUsage(): UsageDto {
  return { ...usage(0), calls: 0, input: 0, output: 0, cache_read: 0, cache_write: 0, reasoning: 0, total: 0, cache_hit_rate: null, cost: 0 };
}

function inclusiveUsage(self: UsageDto, subagents: UsageDto): UsageDto {
  const input = self.input + subagents.input;
  const cache_read = self.cache_read + subagents.cache_read;
  const cache_write = self.cache_write + subagents.cache_write;
  return {
    calls: self.calls + subagents.calls,
    errors: self.errors + subagents.errors,
    input,
    output: self.output + subagents.output,
    cache_read,
    cache_write,
    cache_write_1h: self.cache_write_1h + subagents.cache_write_1h,
    reasoning: self.reasoning + subagents.reasoning,
    total: self.total + subagents.total,
    cache_hit_rate: input + cache_read + cache_write === 0 ? null : cache_read / (input + cache_read + cache_write),
    cost: self.cost + subagents.cost,
    unpriced: self.unpriced + subagents.unpriced,
    retried: self.retried + subagents.retried,
    output_speed: null,
    speed_samples: 0,
  };
}

function thread(revision: number, title: string, threadKey: ThreadKey = key, type: "main" | "subagent" = "main"): SessionThreadDto {
  const currentUsage = usage(revision);
  return {
    key: threadKey,
    type,
    parent_thread_id: type === "main" ? null : "thread-1",
    title,
    nickname: "",
    project: { kind: "project", path: `/work/project-${revision}`, name: `project-${revision}` },
    first_usage_ms: 10,
    last_usage_ms: 20 + revision,
    model_effort: [{ model: `model-${revision}`, effort: "high", usage: currentUsage, compaction_tokens: 0 }],
    usage: currentUsage,
  };
}

function sessionRow(revision: number): SessionListItemDto {
  const value = usage(revision);
  const subagentUsage = usage(revision);
  return {
    thread: thread(revision, `session-${revision}`),
    self_usage: value,
    subagent_usage: subagentUsage,
    unknown_usage: zeroUsage(),
    inclusive_usage: inclusiveUsage(value, subagentUsage),
    has_usage_rows: true,
    last_activity_ms: Date.UTC(2026, 7, 10, 8, revision),
    subagent_count: 1,
  };
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

type HeldResponse = {
  path: string;
  signal: AbortSignal | null | undefined;
  response: Response;
  resolve: (response: Response) => void;
};

function setupHttpWire(initial: {
  revision?: RevisionTuple;
  runtimeState?: "starting" | "ready" | "unavailable";
  sessionsFailures?: number;
  quotaReading?: boolean;
  quotaItems?: unknown[];
} = {}) {
  const backend = {
    revision: initial.revision ?? { data_revision: 80, status_revision: 12 },
    runtimeState: initial.runtimeState ?? "ready",
    sessionsFailures: initial.sessionsFailures ?? 0,
    quotaReading: initial.quotaReading ?? true,
    quotaItems: initial.quotaItems ?? [],
    holdApplicationReads: false,
  };
  const requests: Array<{ path: string; method: string; signal: AbortSignal | null | undefined }> = [];
  const pending: HeldResponse[] = [];

  const payload = (path: string): { body: unknown; status: number } => {
    const dataRevision = backend.revision.data_revision;
    const statusRevision = backend.revision.status_revision;
    if (path === "/api/revision") return { body: backend.revision, status: 200 };
    if (path === "/api/status") return { body: { status_revision: statusRevision, runtime_state: backend.runtimeState, last_error_code: null }, status: 200 };
    if (path === "/api/quota" || path === "/api/quota/refresh") {
      return { body: { reading: backend.quotaReading, items: backend.quotaItems }, status: 200 };
    }
    if (path === "/api/usage/summary") return { body: { data_revision: dataRevision, usage: usage(dataRevision), request_metrics: { calls: 1, errors: 0, retried: 0, output_speed: null, speed_samples: 0, mean_ttft_seconds: null, ttft_samples: 0 }, session_count: 1, session_median: 1, session_p90: 1 }, status: 200 };
    if (path === "/api/usage/filter-options") {
      return {
        body: {
          data_revision: dataRevision,
          agents: [{ agent: `agent-${dataRevision}`, display_name: `Agent ${dataRevision}` }],
          models: [{ model: `model-${dataRevision}`, provider_id: "provider", provider_name: "Provider" }],
          projects: [{ kind: "project", path: `/work/project-${dataRevision}`, name: `project-${dataRevision}` }],
        },
        status: 200,
      };
    }
    if (path === "/api/usage/model-distribution") {
      return { body: { data_revision: dataRevision, items: [{ model: `model-${dataRevision}`, usage: usage(dataRevision) }] }, status: 200 };
    }
    if (path === "/api/usage/projects") {
      return {
        body: { data_revision: dataRevision, items: [{ kind: "project", path: `/work/project-${dataRevision}`, name: `project-${dataRevision}`, usage: usage(dataRevision) }] },
        status: 200,
      };
    }
    if (path === "/api/usage/sessions") {
      if (backend.sessionsFailures > 0) {
        backend.sessionsFailures -= 1;
        return { body: { error: "MAGPIE_UNAVAILABLE" }, status: 503 };
      }
      return {
        body: {
          data_revision: dataRevision,
          summary: { data_revision: dataRevision, usage: usage(dataRevision), request_metrics: { calls: 1, errors: 0, retried: 0, output_speed: null, speed_samples: 0, mean_ttft_seconds: null, ttft_samples: 0 }, session_count: 1, session_median: 1, session_p90: 1 },
          items: [sessionRow(dataRevision)],
        },
        status: 200,
      };
    }
    if (path === "/api/usage/session-detail") {
      const parent = thread(dataRevision, `detail-${dataRevision}`);
      const subagentUsage = usage(dataRevision);
      return {
        body: {
          data_revision: dataRevision,
          last_activity_ms: dataRevision,
          thread: parent,
          self_usage: usage(dataRevision),
          subagent_usage: subagentUsage,
          unknown_usage: zeroUsage(),
          inclusive_usage: inclusiveUsage(usage(dataRevision), subagentUsage),
          children: [thread(dataRevision, `child-${dataRevision}`, { ...key, thread_id: "child-thread" }, "subagent")],
        },
        status: 200,
      };
    }
    throw new Error(`Unexpected API path: ${path}`);
  };

  const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = new URL(String(input), window.location.origin);
    const path = url.pathname;
    const method = init?.method ?? "GET";
    const signal = init?.signal;
    requests.push({ path, method, signal });
    const { body, status } = payload(path);
    if (backend.holdApplicationReads && method === "GET" && applicationReadPaths.has(path)) {
      return new Promise((resolve) => pending.push({ path, signal, response: jsonResponse(body, status), resolve }));
    }
    return Promise.resolve(jsonResponse(body, status));
  });
  vi.stubGlobal("fetch", fetchMock);
  return { backend, requests, pending, fetchMock };
}

function eventSource(): RevisionEventSource {
  return { onerror: null, onmessage: null, close: vi.fn() };
}

function useIntegratedDashboard(feed: RevisionFeed) {
  const dashboard = useDashboardController({ client: usagiClient, revisionFeed: feed });
  const charts = useDashboardChartsController({
    range: dashboard.range,
    filters: dashboard.filters,
    dataRevision: dashboard.data_revision,
    reloadKey: dashboard.reload_key,
    epochResetKey: dashboard.epoch_reset_key,
    client: usagiClient,
  });
  const sessions = useSessionTableController(
    dashboard.range,
    dashboard.filters,
    dashboard.sessions,
    dashboard.load_state,
    dashboard.error_code,
    dashboard.retry_load,
  );
  const detail = useSessionDetailController(dashboard.range, dashboard.filters, {
    client: usagiClient,
    revisionFeed: feed,
    dataRevision: sessions.data_revision,
    sessionsReady: sessions.load_state === "ready",
    onStaleRevision: sessions.retry_load,
  });
  const quota = useDashboardQuotaController({ client: usagiClient });
  return { dashboard, charts, sessions, detail, quota };
}

async function waitForRevisionZero(result: { current: ReturnType<typeof useIntegratedDashboard> }) {
  await waitFor(() => {
    expect(result.current.dashboard.epoch_reset_key).toBe(1);
    expect(result.current.dashboard.data_revision).toBe(0);
    expect(result.current.dashboard.metrics?.input).toBe(17);
    expect(result.current.dashboard.filter_options?.agents[0]?.agent).toBe("agent-0");
    expect(result.current.charts.models?.data_revision).toBe(0);
    expect(result.current.charts.models?.items[0]?.model).toBe("model-0");
    const project = result.current.charts.projects?.items[0];
    if (!project || project.kind !== "project") throw new Error("Expected a project distribution item");
    expect(project.name).toBe("project-0");
    expect(result.current.sessions.data_revision).toBe(0);
    expect(result.current.sessions.rows[0]?.thread.title).toBe("session-0");
    expect(result.current.detail.detail?.data_revision).toBe(0);
    expect(result.current.detail.detail?.thread.title).toBe("detail-0");
    expect(result.current.detail.detail?.children[0]?.title).toBe("child-0");
  });
}

async function flushMicrotasks() {
  await act(async () => {
    for (let index = 0; index < 20; index += 1) await Promise.resolve();
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("Spec04 RevisionFeed and HTTP consumer integration", () => {
  it("bootstraps every open consumer after authoritative epoch reset and ignores aborted revision-80 responses", async () => {
    const api = setupHttpWire();
    const source = eventSource();
    const feed = new RevisionFeed({ client: usagiClient, eventSourceFactory: () => source });
    const { result, unmount } = renderHook(() => useIntegratedDashboard(feed));

    await waitFor(() => {
      expect(result.current.dashboard.load_state).toBe("ready");
      expect(result.current.dashboard.filter_options?.data_revision).toBe(80);
      expect(result.current.charts.models?.data_revision).toBe(80);
      expect(result.current.sessions.data_revision).toBe(80);
    });
    await act(async () => result.current.detail.open_detail(result.current.sessions.rows[0]));
    await waitFor(() => expect(result.current.detail.detail?.data_revision).toBe(80));

    api.backend.holdApplicationReads = true;
    await act(async () => {
      result.current.dashboard.request_refresh();
      result.current.detail.retry_detail();
    });
    await waitFor(() => {
      expect(new Set(api.pending.map((request) => request.path))).toEqual(applicationReadPaths);
    });
    api.backend.revision = { data_revision: 0, status_revision: 0 };
    api.backend.holdApplicationReads = false;
    await act(async () => {
      source.onmessage?.({ data: JSON.stringify(api.backend.revision) } as MessageEvent<string>);
    });

    await waitForRevisionZero(result);
    expect(api.pending.every((request) => request.signal?.aborted)).toBe(true);
    expect(api.requests.filter((request) => request.path === "/api/status")).toHaveLength(2);

    await act(async () => {
      for (const request of api.pending) request.resolve(request.response);
    });
    await waitForRevisionZero(result);
    expect(result.current.dashboard.metrics?.input).toBe(17);
    expect(result.current.charts.models?.items[0]?.model).toBe("model-0");
    expect(result.current.sessions.rows[0]?.thread.title).toBe("session-0");
    expect(result.current.detail.detail?.thread.title).toBe("detail-0");

    unmount();
    feed.dispose();
  });

  it("recovers a starting sessions 503 when the ready data revision arrives over SSE", async () => {
    const api = setupHttpWire({ revision: { data_revision: 0, status_revision: 0 }, runtimeState: "starting", sessionsFailures: 1 });
    const source = eventSource();
    const feed = new RevisionFeed({ client: usagiClient, eventSourceFactory: () => source });
    const { result, unmount } = renderHook(() => useIntegratedDashboard(feed));

    await waitFor(() => expect(result.current.dashboard.load_state).toBe("error"));
    expect(api.requests.filter((request) => request.path === "/api/usage/sessions")).toHaveLength(1);
    api.backend.revision = { data_revision: 1, status_revision: 1 };
    api.backend.runtimeState = "ready";
    await act(async () => {
      source.onmessage?.({ data: JSON.stringify(api.backend.revision) } as MessageEvent<string>);
    });

    await waitFor(() => {
      expect(result.current.dashboard.load_state).toBe("ready");
      expect(result.current.dashboard.data_revision).toBe(1);
      expect(result.current.dashboard.metrics?.input).toBe(1);
      expect(result.current.charts.models?.items[0]?.model).toBe("model-1");
      expect(result.current.sessions.rows[0]?.thread.title).toBe("session-1");
    });
    expect(api.requests.filter((request) => request.path === "/api/status").length).toBeGreaterThanOrEqual(2);
    expect(api.requests.filter((request) => request.path === "/api/usage/sessions").length).toBeGreaterThanOrEqual(2);
    unmount();
    feed.dispose();
  });

  it("recovers application data from the 60s SSE fallback and stops fallback after a successful frame", async () => {
    vi.useFakeTimers();
    const api = setupHttpWire();
    const source = eventSource();
    const feed = new RevisionFeed({ client: usagiClient, eventSourceFactory: () => source });
    const { result, unmount } = renderHook(() => useIntegratedDashboard(feed));

    await flushMicrotasks();
    expect(result.current.dashboard.load_state).toBe("ready");
    expect(result.current.charts.models?.data_revision).toBe(80);
    expect(result.current.sessions.data_revision).toBe(80);
    await act(async () => result.current.detail.open_detail(result.current.sessions.rows[0]));
    await flushMicrotasks();
    expect(result.current.detail.detail?.data_revision).toBe(80);
    expect(result.current.detail.detail?.last_activity_ms).toBe(80);
    const beforeFallback = api.requests.filter((request) => request.path === "/api/revision").length;
    source.onerror?.(new Event("error"));
    api.backend.revision = { data_revision: 81, status_revision: 12 };
    await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
    await flushMicrotasks();

    expect(result.current.dashboard.data_revision).toBe(81);
    expect(result.current.dashboard.metrics?.input).toBe(81);
    expect(result.current.charts.models?.items[0]?.model).toBe("model-81");
    expect(result.current.sessions.rows[0]?.thread.title).toBe("session-81");
    expect(result.current.detail.detail?.data_revision).toBe(81);
    expect(result.current.detail.detail?.last_activity_ms).toBe(81);
    expect(api.requests.filter((request) => request.path === "/api/revision")).toHaveLength(beforeFallback + 1);
    const applicationReads = api.requests.filter((request) => applicationReadPaths.has(request.path)).length;
    source.onmessage?.({ data: JSON.stringify(api.backend.revision) } as MessageEvent<string>);
    expect(api.requests.filter((request) => applicationReadPaths.has(request.path))).toHaveLength(applicationReads);
    await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
    expect(api.requests.filter((request) => request.path === "/api/revision")).toHaveLength(beforeFallback + 1);
    unmount();
    feed.dispose();
  });

  it("polls cached Quota while reading, resumes 60 seconds after ready, and keeps manual refresh isolated", async () => {
    vi.useFakeTimers();
    const api = setupHttpWire();
    const source = eventSource();
    const feed = new RevisionFeed({ client: usagiClient, eventSourceFactory: () => source });
    const { result, unmount } = renderHook(() => useIntegratedDashboard(feed));

    await flushMicrotasks();
    expect(result.current.sessions.load_state).toBe("ready");
    await act(async () => result.current.detail.open_detail(result.current.sessions.rows[0]));
    await flushMicrotasks();
    expect(result.current.detail.load_state).toBe("ready");
    await act(async () => result.current.detail.close_detail());
    const detailGets = api.requests.filter((request) => request.path === "/api/usage/session-detail").length;
    const sessionGets = api.requests.filter((request) => request.path === "/api/usage/sessions").length;

    expect(result.current.quota.quota.reading).toBe(true);
    api.backend.quotaReading = false;
    api.backend.quotaItems = [{
      provider: "codex",
      name: "Codex",
      plan: "Pro",
      user: "quota-user",
      windows: [],
      balance: "",
      error: "",
      as_of_ms: 80,
      read_at_ms: 81,
    }];

    // A snapshot that is still reading is re-polled after 5 seconds; ready snapshots resume at 60 seconds.
    await act(async () => { await vi.advanceTimersByTimeAsync(59_999); });
    expect(api.requests.filter((request) => request.path === "/api/quota" && request.method === "GET")).toHaveLength(2);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    await flushMicrotasks();
    expect(api.requests.filter((request) => request.path === "/api/quota" && request.method === "GET")).toHaveLength(2);
    expect(result.current.quota.quota).toEqual({ reading: false, items: api.backend.quotaItems });

    await act(async () => { await vi.advanceTimersByTimeAsync(59_999); });
    expect(api.requests.filter((request) => request.path === "/api/quota" && request.method === "GET")).toHaveLength(3);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    await flushMicrotasks();
    expect(api.requests.filter((request) => request.path === "/api/quota" && request.method === "GET")).toHaveLength(3);
    expect(result.current.quota.quota.reading).toBe(false);

    await act(async () => result.current.quota.refresh());
    await flushMicrotasks();
    expect(api.requests.filter((request) => request.path === "/api/quota/refresh" && request.method === "POST")).toHaveLength(1);
    expect(result.current.quota.quota.reading).toBe(false);
    expect(api.requests.filter((request) => request.path === "/api/quota" && request.method === "GET")).toHaveLength(3);
    expect(api.requests.filter((request) => request.path === "/api/usage/sessions")).toHaveLength(sessionGets);
    await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
    await flushMicrotasks();
    expect(api.requests.filter((request) => request.path === "/api/quota" && request.method === "GET")).toHaveLength(4);
    expect(api.requests.filter((request) => request.path === "/api/quota/refresh" && request.method === "POST")).toHaveLength(1);
    expect(api.requests.filter((request) => request.path === "/api/usage/session-detail")).toHaveLength(detailGets);
    expect(api.requests.filter((request) => request.path === "/api/usage/sessions")).toHaveLength(sessionGets);
    unmount();
    feed.dispose();
  });
});
