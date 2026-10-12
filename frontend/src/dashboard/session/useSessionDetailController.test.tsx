import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "../../data/usagiClient";
import { createRevisionFeed, type RevisionEventSource } from "../../data/revisionFeed";
import type { DashboardFilters, SessionDetailResponse, SessionListItemDto, ThreadKey, UsageDto } from "../../data/types";
import { useSessionDetailController } from "./useSessionDetailController";

const filters: DashboardFilters = { agents: [], models: [], projects: [] };
const usage: UsageDto = {
  calls: 1,
  errors: 0,
  input: 7,
  output: 3,
  cache_read: 2,
  cache_write: 1,
  cache_write_1h: 0,
  reasoning: 1,
  total: 13,
  cache_hit_rate: 0.2,
  cost: 0.25,
  unpriced: 0,
};

function key(agent: string, sessionId: string, threadId: string): ThreadKey {
  return { agent, session_id: sessionId, thread_id: threadId };
}

function row(threadKey: ThreadKey): SessionListItemDto {
  const thread = {
    key: threadKey,
    type: "main" as const,
    parent_thread_id: null,
    title: "Session title",
    project: { kind: "project" as const, path: "/work", name: "Work" },
    first_usage_ms: 10,
    last_usage_ms: 20,
    model_effort: [{ model: "gpt-6", effort: "high", usage, compaction_tokens: 0 }],
    usage,
  };
  return { thread, self_usage: usage, subagent_usage: usage, inclusive_usage: usage, subagent_count: 1 };
}

function detail(threadKey: ThreadKey, revision: number): SessionDetailResponse {
  const parent = row(threadKey).thread;
  return {
    data_revision: revision,
    thread: parent,
    self_usage: usage,
    subagent_usage: usage,
    inclusive_usage: usage,
    children: [],
  };
}

function eventSource(): RevisionEventSource {
  return { onerror: null, onmessage: null, close: vi.fn() };
}

function clientWith(overrides: Partial<UsagiClient> = {}): UsagiClient {
  return {
    quota: vi.fn(),
    refreshQuota: vi.fn(),
    filterOptions: vi.fn(),
    summary: vi.fn(),
    modelDistribution: vi.fn(),
    projectDistribution: vi.fn(),
    getSessions: vi.fn(),
    getSessionDetail: vi.fn(async ({ key: threadKey }) => detail(threadKey, 1)),
    getStatus: vi.fn(),
    getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })),
    ...overrides,
  };
}

describe("useSessionDetailController ThreadKey and epoch behavior", () => {
  it("loads Detail on demand and identifies it with the complete ThreadKey", async () => {
    const first = row(key("codex", "session-1", "thread-1"));
    const second = row(key("claude", "session-1", "thread-1"));
    const client = clientWith();
    const { result } = renderHook(() => useSessionDetailController({ key: "today" }, filters, { client, dataRevision: 1 }));

    expect(client.getSessionDetail).not.toHaveBeenCalled();
    await act(async () => result.current.open_detail(first));
    await waitFor(() => expect(result.current.detail?.thread.key).toEqual(first.thread.key));
    await act(async () => result.current.open_detail(second));
    await waitFor(() => expect(result.current.detail?.thread.key).toEqual(second.thread.key));
    expect(client.getSessionDetail).toHaveBeenCalledTimes(2);
    expect(client.getSessionDetail).toHaveBeenNthCalledWith(1, expect.objectContaining({ key: first.thread.key }));
    expect(client.getSessionDetail).toHaveBeenNthCalledWith(2, expect.objectContaining({ key: second.thread.key }));
  });

  it("accepts epoch 0 and ignores a parent Session revision 80 while waiting for its new snapshot", async () => {
    const threadKey = key("codex", "root-session", "root-thread");
    const selected = row(threadKey);
    const source = eventSource();
    let resolveEpochDetail!: (response: SessionDetailResponse) => void;
    const client = clientWith({
      getRevision: vi.fn()
        .mockResolvedValueOnce({ data_revision: 80, status_revision: 12 })
        .mockResolvedValueOnce({ data_revision: 0, status_revision: 0 }),
      getSessionDetail: vi.fn()
        .mockResolvedValueOnce(detail(threadKey, 80))
        .mockImplementationOnce(() => new Promise((resolve) => { resolveEpochDetail = resolve; })),
    });
    const feed = createRevisionFeed({ client, eventSourceFactory: () => source });
    const options = { client, revisionFeed: feed, dataRevision: 80, sessionsReady: true };
    const { result, rerender } = renderHook(
      ({ dataRevision, sessionsReady }: { dataRevision: number; sessionsReady: boolean }) => useSessionDetailController({ key: "today" }, filters, { ...options, dataRevision, sessionsReady }),
      { initialProps: { dataRevision: 80, sessionsReady: true } },
    );

    await act(async () => result.current.open_detail(selected));
    await waitFor(() => expect(result.current.detail?.data_revision).toBe(80));
    expect(client.getSessionDetail).toHaveBeenCalledTimes(1);

    await act(async () => source.onmessage?.({ data: JSON.stringify({ data_revision: 0, status_revision: 0 }) } as MessageEvent<string>));
    await waitFor(() => expect(client.getSessionDetail).toHaveBeenCalledTimes(2));
    rerender({ dataRevision: 80, sessionsReady: false });
    expect(result.current.data_revision).toBe(0);

    await act(async () => resolveEpochDetail(detail(threadKey, 0)));
    await waitFor(() => expect(result.current.detail?.data_revision).toBe(0));
    rerender({ dataRevision: 0, sessionsReady: true });
    await waitFor(() => expect(result.current.data_revision).toBe(0));
    expect(result.current.load_state).toBe("ready");
    expect(client.getSessionDetail).toHaveBeenCalledTimes(2);
    feed.dispose();
  });

  it("replaces an older same-epoch Detail response with the authoritative revision", async () => {
    const threadKey = key("claude", "session", "thread");
    let resolveOld!: (response: SessionDetailResponse) => void;
    const client = clientWith({
      getSessionDetail: vi.fn()
        .mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }))
        .mockResolvedValueOnce(detail(threadKey, 2)),
    });
    const source = eventSource();
    const feed = createRevisionFeed({
      client: clientWith({ ...client, getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })) }),
      eventSourceFactory: () => source,
    });
    const { result } = renderHook(() => useSessionDetailController({ key: "today" }, filters, { client, revisionFeed: feed, dataRevision: 1 }));
    await act(async () => result.current.open_detail(row(threadKey)));
    source.onmessage?.({ data: JSON.stringify({ data_revision: 2, status_revision: 1 }) } as MessageEvent<string>);
    await waitFor(() => expect(client.getSessionDetail).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(result.current.detail?.data_revision).toBe(2));
    await act(async () => resolveOld(detail(threadKey, 1)));
    expect(result.current.detail?.data_revision).toBe(2);
    feed.dispose();
  });
});
