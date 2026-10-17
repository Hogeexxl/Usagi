import { describe, expect, it } from "vitest";

import {
  parseFilterOptions,
  parseModelDistribution,
  parseProjectDistribution,
  parseQuota,
  parseRevision,
  parseSessionDetail,
  parseSessionList,
  parseStatus,
  parseSummary,
  parseToolUsage,
} from "./usagiParsers";
import { UsagiClientError, type UsageDto } from "./types";

const usage: UsageDto = {
  calls: 2,
  errors: 1,
  input: 10,
  output: 20,
  cache_read: 4,
  cache_write: 2,
  cache_write_1h: 1,
  reasoning: 5,
  total: 36,
  cache_hit_rate: 0.25,
  cost: 1.25,
  unpriced: 1,
  retried: 0,
  output_speed: null,
  speed_samples: 0,
};
const zeroUsage: UsageDto = {
  ...usage,
  calls: 0,
  errors: 0,
  input: 0,
  output: 0,
  cache_read: 0,
  cache_write: 0,
  cache_write_1h: 0,
  reasoning: 0,
  total: 0,
  cache_hit_rate: null,
  cost: 0,
  unpriced: 0,
};
const requestMetrics = {
  calls: 2,
  errors: 1,
  retried: 0,
  output_speed: null,
  speed_samples: 0,
  mean_ttft_seconds: null,
  ttft_samples: 0,
};

const summary = {
  data_revision: 8,
  usage,
  request_metrics: requestMetrics,
  session_count: 3,
  session_median: null,
  session_p90: 24,
};

const thread = {
  key: { agent: "claude", session_id: "session-1", thread_id: "thread-1" },
  type: "main",
  parent_thread_id: null,
  title: "Session one",
  nickname: "",
  project: { kind: "project", path: "/work/Usagi", name: "Usagi" },
  first_usage_ms: 100,
  last_usage_ms: 200,
  model_effort: [{ model: "gpt-6", effort: "high", usage, compaction_tokens: 5 }],
  usage,
};

function expectParseFailure(parse: (value: unknown) => unknown, value: unknown) {
  let error: unknown;
  try {
    parse(value);
  } catch (caught) {
    error = caught;
  }
  expect(error).toBeInstanceOf(UsagiClientError);
  expect(error).toMatchObject({ code: "HTTP_ERROR", status: 200 });
}

const responses = [
  ["quota", parseQuota, {
    reading: false,
    items: [{
      provider: "codex",
      name: "Codex",
      plan: "Pro",
      user: "user@example.test",
      windows: [{
        unlimited: false,
        name: "5H",
        used: 0.2,
        resets_at_ms: null,
        reset_secs: 0,
        display: "",
        amount: 2,
        limit: 10,
        unit: "calls",
        family: "usage",
        pool: "primary",
      }],
      balance: "",
      error: "",
      as_of_ms: 100,
      read_at_ms: 101,
    }],
  }],
  ["filter options", parseFilterOptions, {
    data_revision: 8,
    agents: [{ agent: "aider", display_name: "Aider" }],
    models: [{ model: "gpt-6", provider_id: "openai", provider_name: "OpenAI" }],
    projects: [{ kind: "project", path: "/work/Usagi", name: "Usagi" }, { kind: "workspace" }, { kind: "projectless" }, { kind: "unknown" }],
  }],
  ["summary", parseSummary, summary],
  ["model distribution", parseModelDistribution, { data_revision: 8, items: [{ model: "gpt-6", usage }] }],
  ["project distribution", parseProjectDistribution, {
    data_revision: 8,
    items: [{ kind: "project", path: "/work/Usagi", name: "Usagi", usage }, { kind: "workspace", usage }, { kind: "projectless", usage }, { kind: "unknown", usage }],
  }],
  ["session list", parseSessionList, {
    data_revision: 8,
    summary,
    items: [{
      thread,
      self_usage: usage,
      subagent_usage: usage,
      unknown_usage: usage,
      inclusive_usage: usage,
      has_usage_rows: true,
      last_activity_ms: null,
      subagent_count: null,
    }],
  }],
  ["tool usage", parseToolUsage, { days: 7, calls: 12, top: [{ name: "Read", calls: 8 }, { name: "Edit", calls: 4 }] }],
  ["session detail", parseSessionDetail, {
    data_revision: 8,
    last_activity_ms: null,
    thread,
    self_usage: usage,
    subagent_usage: usage,
    unknown_usage: usage,
    inclusive_usage: usage,
    children: [{ ...thread, type: "subagent", parent_thread_id: "thread-1" }],
  }],
  ["status", parseStatus, { status_revision: 3, runtime_state: "starting", last_error_code: null }],
  ["revision", parseRevision, { data_revision: 8, status_revision: 3 }],
] as const;

describe("shared Usagi response parsers", () => {
  it.each(responses)("parses the %s DTO", (_name, parse, response) => {
    expect(parse(response)).toEqual(response);
  });

  it.each([
    ["quota", parseQuota, { reading: "false", items: [] }],
    ["filter options", parseFilterOptions, { data_revision: 0, agents: [null], models: [], projects: [] }],
    ["summary", parseSummary, { ...summary, request_metrics: { ...summary.request_metrics, ttft_samples: -1 } }],
    ["model distribution", parseModelDistribution, { data_revision: 0, items: [{ model: "gpt-6", usage: { ...usage, total: -1 } }] }],
    ["project distribution", parseProjectDistribution, { data_revision: 0, items: [{ kind: "invalid", usage }] }],
    ["session list", parseSessionList, { data_revision: 0, summary: null, items: [] }],
    ["tool usage", parseToolUsage, { days: 2, calls: 0, top: [] }],
    ["session detail", parseSessionDetail, { data_revision: 0, children: null }],
    ["status", parseStatus, { status_revision: 0, runtime_state: "stopped", last_error_code: null }],
    ["revision", parseRevision, { data_revision: -1, status_revision: 0 }],
  ] as const)("rejects malformed %s payloads", (_name, parse, response) => {
    expectParseFailure(parse, response);
  });

  it("requires a complete independent request_metrics DTO in summaries", () => {
    expect(parseSummary({
      data_revision: 8,
      usage,
      request_metrics: requestMetrics,
      session_count: 3,
      session_median: null,
      session_p90: 24,
    })).toEqual(summary);

    const { request_metrics: _requestMetrics, ...missingMetrics } = summary;
    expectParseFailure(parseSummary, missingMetrics);
    expectParseFailure(parseSummary, {
      ...summary,
      request_metrics: { ...requestMetrics, ttft_samples: -1 },
    });
  });

  it("preserves SessionListItemView and detail semantics, including nullable activity", () => {
    const workspaceThread = {
      ...thread,
      project: { kind: "workspace", path: "/private/tmp/claude-501/child" },
    };
    const list = {
      data_revision: 8,
      summary: { ...summary, session_count: 1, session_p90: null },
      items: [{
        thread: workspaceThread,
        self_usage: usage,
        subagent_usage: { ...usage, total: 1 },
        unknown_usage: zeroUsage,
        inclusive_usage: usage,
        has_usage_rows: false,
        last_activity_ms: null,
        subagent_count: null,
      }],
    };
    expect(parseSessionList(list)).toEqual(list);
    expect(parseSessionList(list).items[0].thread.project).toEqual(workspaceThread.project);

    const detail = {
      data_revision: 8,
      last_activity_ms: null,
      thread: workspaceThread,
      self_usage: usage,
      subagent_usage: usage,
      unknown_usage: zeroUsage,
      inclusive_usage: usage,
      children: [{
        ...thread,
        key: { agent: "claude", session_id: "session-1", thread_id: "subagent-1" },
        type: "subagent",
        parent_thread_id: "thread-1",
      }],
    };
    expect(parseSessionDetail(detail)).toEqual(detail);
    expect(parseSessionDetail(detail).children).toEqual(detail.children);

    expectParseFailure(parseSessionList, {
      ...list,
      items: [{ ...list.items[0], last_activity_ms: "not-a-number" }],
    });
    const { last_activity_ms: _listActivity, ...listItemWithoutActivity } = list.items[0];
    expectParseFailure(parseSessionList, {
      ...list,
      items: [listItemWithoutActivity],
    });
    expectParseFailure(parseSessionDetail, { ...detail, last_activity_ms: "not-a-number" });
    const { last_activity_ms: _detailActivity, ...detailWithoutActivity } = detail;
    expectParseFailure(parseSessionDetail, detailWithoutActivity);
  });

  it("parses Codex main and child identities alongside unknown thread usage", () => {
    const mainId = "01a135e4-9b71-7fa1-8bb0-2dd2bdaf5497";
    const childId = "01a138ab-909f-7990-a365-9555077faea2";
    const mainThread = {
      ...thread,
      key: { agent: "codex", session_id: mainId, thread_id: mainId },
      type: "main",
      parent_thread_id: null,
    };
    const childThread = {
      ...thread,
      key: { agent: "codex", session_id: mainId, thread_id: childId },
      type: "subagent",
      parent_thread_id: mainId,
      title: "Codex child",
      nickname: "Child nickname",
    };
    const unknownThread = {
      ...thread,
      key: { agent: "codex", session_id: mainId, thread_id: "unknown-calls" },
      type: "unknown",
      parent_thread_id: null,
      model_effort: [],
      usage: zeroUsage,
    };
    const unknownUsage = { ...zeroUsage, calls: 1, input: 4, total: 4, cost: 0.02 };
    const mainItem = {
      thread: mainThread,
      self_usage: usage,
      subagent_usage: usage,
      unknown_usage: zeroUsage,
      inclusive_usage: usage,
      has_usage_rows: true,
      last_activity_ms: 200,
      subagent_count: 1,
    };
    const unknownItem = {
      thread: unknownThread,
      self_usage: zeroUsage,
      subagent_usage: zeroUsage,
      unknown_usage: unknownUsage,
      inclusive_usage: unknownUsage,
      has_usage_rows: true,
      last_activity_ms: 200,
      subagent_count: null,
    };
    const sessionSummary = {
      data_revision: 8,
      usage,
      request_metrics: requestMetrics,
      session_count: 2,
      session_median: null,
      session_p90: null,
    };
    const sessions = parseSessionList({
      data_revision: 8,
      summary: sessionSummary,
      items: [mainItem, unknownItem],
    });

    expect(sessions.items.map((item) => item.thread.type)).toEqual(["main", "unknown"]);
    expect(sessions.items[0].thread.key).toEqual(mainThread.key);
    expect(sessions.items[0].thread.parent_thread_id).toBeNull();
    expect(sessions.items[0].thread.nickname).toBe(thread.nickname);
    expect(sessions.items[1].thread.key).toEqual(unknownThread.key);
    expect(sessions.items[1].unknown_usage).toEqual(unknownUsage);

    const mainDetail = parseSessionDetail({
      data_revision: 8,
      last_activity_ms: 200,
      thread: mainThread,
      self_usage: usage,
      subagent_usage: usage,
      unknown_usage: zeroUsage,
      inclusive_usage: usage,
      children: [childThread],
    });
    expect(mainDetail.children).toEqual([childThread]);
    expect(mainDetail.last_activity_ms).toBe(200);
    expect(mainDetail.thread.key).toEqual(mainThread.key);
    expect(mainDetail.thread.parent_thread_id).toBeNull();
    expect(mainDetail.children[0].key).toEqual(childThread.key);
    expect(mainDetail.children[0].parent_thread_id).toBe(mainId);
    expect(mainDetail.children[0].nickname).toBe("Child nickname");
    expect(mainDetail.children[0].model_effort[0].usage).toEqual(usage);

    const unknownDetail = parseSessionDetail({
      data_revision: 8,
      last_activity_ms: null,
      thread: unknownThread,
      self_usage: zeroUsage,
      subagent_usage: zeroUsage,
      unknown_usage: unknownUsage,
      inclusive_usage: unknownUsage,
      children: [],
    });
    expect(unknownDetail.thread.type).toBe("unknown");
    expect(unknownDetail.unknown_usage).toEqual(unknownUsage);
    expect(unknownDetail.last_activity_ms).toBeNull();
  });

  it("keeps tool usage days and top item order intact", () => {
    const toolUsage = { days: 7, calls: 12, top: [{ name: "Read", calls: 8 }, { name: "Edit", calls: 4 }] };
    expect(parseToolUsage(toolUsage)).toEqual(toolUsage);
    expect(parseToolUsage(toolUsage).top).toEqual(toolUsage.top);
  });

  it("rejects malformed usage values in summary responses", () => {
    expectParseFailure(parseSummary, {
      ...summary,
      usage: { ...usage, total: -1 },
    });
  });
});
