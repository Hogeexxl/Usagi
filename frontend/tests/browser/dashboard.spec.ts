import { readFileSync, writeFileSync } from "node:fs";
import { expect, test, type Page, type Route } from "@playwright/test";

const REVISION = 100;
const FIXTURE_NOW_MS = 1_800_000_000_000;

type RangeKey = "today" | "yesterday" | "7d" | "30d" | "year" | "custom";
type UsageKind = "base" | "model" | "filtered" | "main" | "sub" | "empty";

function rangeFrom(url: string): RangeKey {
  const value = new URL(url).searchParams.get("range");
  return value === "today" || value === "yesterday" || value === "7d" || value === "30d" || value === "year" || value === "custom"
    ? value
    : "today";
}

function usage(kind: UsageKind = "base", cost?: number) {
  const values = {
    base: { calls: 6, errors: 1, input: 1200, output: 600, cache_read: 400, cache_write: 100, cache_write_1h: 50, reasoning: 180, total: 2300, cost: 1.2, retried: 1, output_speed: 30, speed_samples: 2 },
    model: { calls: 4, errors: 0, input: 600, output: 250, cache_read: 250, cache_write: 50, cache_write_1h: 25, reasoning: 100, total: 1150, cost: 0.8, retried: 1, output_speed: 25, speed_samples: 1 },
    filtered: { calls: 2, errors: 0, input: 400, output: 150, cache_read: 150, cache_write: 50, cache_write_1h: 25, reasoning: 70, total: 750, cost: 0.55, retried: 0, output_speed: 20, speed_samples: 1 },
    main: { calls: 2, errors: 0, input: 230, output: 100, cache_read: 70, cache_write: 20, cache_write_1h: 10, reasoning: 30, total: 420, cost: 0.2, retried: 0, output_speed: 20, speed_samples: 1 },
    sub: { calls: 1, errors: 0, input: 95, output: 50, cache_read: 25, cache_write: 10, cache_write_1h: 5, reasoning: 15, total: 180, cost: 0.1, retried: 0, output_speed: 15, speed_samples: 1 },
    empty: { calls: 0, errors: 0, input: 0, output: 0, cache_read: 0, cache_write: 0, cache_write_1h: 0, reasoning: 0, total: 0, cost: 0, retried: 0, output_speed: null, speed_samples: 0 },
  }[kind];
  return {
    ...values,
    cache_hit_rate: values.input + values.cache_read + values.cache_write > 0
      ? values.cache_read / (values.input + values.cache_read + values.cache_write)
      : null,
    cost: cost ?? values.cost,
    unpriced: 0,
  };
}

function summary(url: string, kind: UsageKind = "base") {
  const params = new URL(url).searchParams;
  const hasModel = params.getAll("model_at").length > 0;
  const hasProject = params.getAll("project_path").length > 0 || params.has("include_projectless") || params.has("include_unknown_project");
  const selectedKind: UsageKind = hasModel && hasProject ? "filtered" : hasModel || hasProject ? "model" : kind;
  const selectedUsage = usage(selectedKind);
  return {
    data_revision: REVISION,
    usage: selectedUsage,
    request_metrics: {
      calls: selectedUsage.calls + 10,
      errors: selectedUsage.errors + 1,
      retried: selectedUsage.retried + 2,
      output_speed: selectedUsage.output_speed,
      speed_samples: selectedUsage.speed_samples,
      mean_ttft_seconds: selectedUsage.output_speed === null ? null : 7.6,
      ttft_samples: selectedUsage.output_speed === null ? 0 : 4,
    },
    session_count: selectedKind === "filtered" ? 1 : 2,
    session_median: 120,
    session_p90: 180,
  };
}

function project(kind: "project" | "projectless" | "unknown", path = "", name = "") {
  return kind === "project" ? { kind, path, name } : { kind };
}

function modelEffort(model: string, effort: string, value: ReturnType<typeof usage>, compaction: number | null = null) {
  return { model, effort, usage: value, compaction_tokens: compaction };
}

function thread(agent: string, id: string, title: string, activity: number, projectValue = project("project", "/work/Usagi", "Usagi"), type: "main" | "subagent" = "main", parent: string | null = null) {
  const ownUsage = usage("main");
  return {
    key: { agent, session_id: "session-2", thread_id: id },
    type,
    parent_thread_id: parent,
    title,
    nickname: "",
    project: projectValue,
    first_usage_ms: activity - 60_000,
    last_usage_ms: activity,
    model_effort: [
      modelEffort("gpt-5", "high", ownUsage, 7),
      modelEffort("o4-mini", "", usage("sub"), 0),
    ],
    usage: ownUsage,
  };
}

function session(id: string, title: string, activity: number, cost: number, total: number, projectPath: string, projectName: string) {
  const itemUsage = { ...usage("main", cost), total };
  const sessionThread = thread("codex", id, title, activity, project("project", projectPath, projectName));
  sessionThread.key.session_id = id;
  sessionThread.key.thread_id = id;
  sessionThread.usage = itemUsage;
  sessionThread.model_effort[0].usage = itemUsage;
  return {
    thread: sessionThread,
    self_usage: itemUsage,
    subagent_usage: usage("empty"),
    unknown_usage: usage("empty"),
    inclusive_usage: itemUsage,
    has_usage_rows: true,
    last_activity_ms: activity,
    subagent_count: null,
  };
}

const SESSIONS = [
  session("session-1", "First session", FIXTURE_NOW_MS + 200, 0.2, 600, "/work/Usagi", "Usagi"),
  session("session-2", "Second session", FIXTURE_NOW_MS + 100, 0.8, 800, "/work/Docs", "Docs"),
];

function sessionList(url: string) {
  const params = new URL(url).searchParams;
  const selectedModels = params.getAll("model_at");
  const selectedProjects = params.getAll("project_path");
  const items = SESSIONS.filter((item) => {
    const modelMatches = selectedModels.length === 0 || item.thread.model_effort.some((model) => selectedModels.includes(`openai/${model.model}`));
    const projectMatches = selectedProjects.length === 0 || item.thread.project.kind !== "project" || selectedProjects.includes(item.thread.project.path);
    return modelMatches && projectMatches;
  });
  const snapshotSummary = summary(url);
  return { data_revision: REVISION, summary: snapshotSummary, items };
}

function modelDistribution() {
  return {
    data_revision: REVISION,
    items: [
      { model: "gpt-5", usage: usage("model", 0.6) },
      { model: "o4-mini", usage: usage("sub", 0.3) },
      { model: "gpt-5-mini", usage: usage("filtered", 0.15) },
      { model: "codex-auto-review", usage: { ...usage("empty"), unpriced: 1 } },
      { model: "gpt-4.1", usage: usage("main", 0.08) },
      { model: "other-model", usage: usage("sub", 0.02) },
    ],
  };
}

function projectDistribution() {
  return {
    data_revision: REVISION,
    items: [
      { kind: "project", path: "/work/Usagi", name: "Usagi", usage: usage("model", 0.7) },
      { kind: "project", path: "/work/Docs", name: "Docs", usage: usage("filtered", 0.25) },
      { kind: "projectless", usage: usage("sub", 0.1) },
      { kind: "unknown", usage: { ...usage("empty"), unpriced: 1 } },
    ],
  };
}

function sessionDetail(url: string) {
  const params = new URL(url).searchParams;
  const id = params.get("thread_id") ?? "session-2";
  const activity = id === "session-1" ? FIXTURE_NOW_MS + 200 : FIXTURE_NOW_MS + 100;
  const selected = SESSIONS.find((item) => item.thread.key.thread_id === id) ?? SESSIONS[1];
  const main = thread("codex", id, `Detail ${id}`, activity, selected.thread.project);
  main.key.session_id = selected.thread.key.session_id;
  main.key.thread_id = id;
  return {
    data_revision: REVISION,
    last_activity_ms: activity,
    thread: main,
    self_usage: usage("main", 0.4),
    subagent_usage: usage("sub", 0.2),
    unknown_usage: usage("empty"),
    inclusive_usage: { ...usage("main", 0.6), total: 780 },
    children: [
      thread("codex", `${id}-sub-recent`, "Recent subagent", FIXTURE_NOW_MS + 180, selected.thread.project, "subagent", id),
      thread("codex", `${id}-sub-old`, "Old subagent", FIXTURE_NOW_MS + 160, selected.thread.project, "subagent", id),
    ],
  };
}

function quota() {
  return {
    reading: false,
    items: [{
      provider: "codex",
      name: "Codex",
      plan: "Pro 5x",
      user: "hoge@example.com",
      windows: [{
        unlimited: false,
        name: "weekly",
        used: 55,
        resets_at_ms: Date.UTC(2026, 7, 12, 4, 23),
        reset_secs: 604800,
        display: "Resets weekly",
        amount: 100,
        limit: 100,
        unit: "requests",
        family: "weekly",
        pool: "",
      }],
      balance: "",
      error: "",
      as_of_ms: null,
      read_at_ms: Date.UTC(2026, 7, 1),
    }],
  };
}

async function json(route: Route, body: unknown) {
  await route.fulfill({ status: 200, contentType: "application/json", json: body });
}

async function routeStableDashboardData(page: Page, toolCalls?: number[]) {
  await page.route("**/api/events*", (route) => route.abort());
  await page.route("**/api/health", (route) => route.fulfill({ status: 204, headers: { "X-Usagi-App": "Usagi" } }));
  await page.route("**/api/quota", (route) => json(route, quota()));
  await page.route("**/api/quota/refresh", (route) => json(route, quota()));
  await page.route("**/api/revision", (route) => json(route, { data_revision: REVISION, status_revision: 1 }));
  await page.route("**/api/status", (route) => json(route, { status_revision: 1, runtime_state: "ready", last_error_code: null }));
  await page.route("**/api/service", (route) => json(route, { state: "running" }));
  await page.route("**/api/service/stop", (route) => json(route, { state: "stopped" }));
  await page.route("**/api/usage/filter-options*", (route) => json(route, {
    data_revision: REVISION,
    agents: [{ agent: "codex", display_name: "Codex" }],
    models: [
      { model: "gpt-5", provider_id: "openai", provider_name: "OpenAI" },
      { model: "o4-mini", provider_id: "openai", provider_name: "OpenAI" },
    ],
    projects: [
      { kind: "project", path: "/work/Usagi", name: "Usagi" },
      { kind: "project", path: "/work/Docs", name: "Docs" },
    ],
  }));
  await page.route("**/api/usage/summary*", (route) => json(route, summary(route.request().url())));
  await page.route("**/api/usage/model-distribution*", (route) => json(route, modelDistribution()));
  await page.route("**/api/usage/projects*", (route) => json(route, projectDistribution()));
  await page.route("**/api/usage/tools*", (route) => {
    const days = Number(new URL(route.request().url()).searchParams.get("days"));
    toolCalls?.push(days);
    return json(route, {
      days,
      calls: 100,
      top: Array.from({ length: 8 }, (_, index) => ({ name: `tool-${index + 1}`, calls: 10 })).concat([]),
    });
  });
  await page.route(/\/api\/usage\/sessions\?/, (route) => json(route, sessionList(route.request().url())));
  await page.route(/\/api\/usage\/session-detail\?/, (route) => json(route, sessionDetail(route.request().url())));
}

async function waitForDashboard(page: Page) {
  await expect(page.getByRole("heading", { name: "Usagi" })).toBeVisible();
  await expect(page.getByLabel("KPI 指标")).toBeVisible();
  await expect(page.getByRole("table")).toBeVisible();
  await expect(page.locator("tr[data-session-thread-key-id]")).toHaveCount(2);
}

async function illegalStorageWrites(page: Page) {
  return page.evaluate(() => (window as unknown as { __miniIllegalStorageWrites?: number }).__miniIllegalStorageWrites ?? 0);
}

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const originalSetItem = Storage.prototype.setItem;
    Object.defineProperty(window, "__miniIllegalStorageWrites", { value: 0, writable: true, configurable: true });
    Storage.prototype.setItem = function patchedSetItem(key: string, value: string) {
      if (key === "usagi.theme") {
        originalSetItem.call(this, key, value);
        return;
      }
      const target = window as unknown as { __miniIllegalStorageWrites: number };
      target.__miniIllegalStorageWrites += 1;
      throw new Error(`unexpected persistent browser state: ${key}`);
    };
  });
});

test("runtime database returns DATA_LOADING, then complete offline session data", async ({ page }) => {
  const releaseFile = process.env.USAGI_BROWSER_RELEASE_FILE;
  const magpiePidFile = process.env.USAGI_BROWSER_MAGPIE_PID_FILE;
  if (!releaseFile || !magpiePidFile) throw new Error("browser gate runtime fixture is missing its control files");

  await page.goto("/");
  const readSummary = () => page.evaluate(async () => {
    const response = await fetch("/api/usage/summary?range=today");
    return { status: response.status, body: await response.json() };
  });
  const loading = await readSummary();
  expect(loading.status).toBe(503);
  expect(JSON.stringify(loading.body)).toContain("DATA_LOADING");

  writeFileSync(releaseFile, "release");
  await expect.poll(async () => (await readSummary()).status, { timeout: 15_000 }).toBe(200);
  const complete = await readSummary();
  expect(complete.body.usage).toMatchObject({ input: 10, output: 5 });
  expect(complete.body.data_revision).toBeGreaterThan(0);
  const readySessions = await page.evaluate(async () => {
    const response = await fetch("/api/usage/sessions?range=today");
    return { status: response.status, body: await response.json() };
  });
  expect(readySessions.status).toBe(200);
  expect(readySessions.body.items).toHaveLength(1);
  expect(readySessions.body.items[0].thread.title).toBe("Temporary runtime smoke session");

  const magpiePid = Number(readFileSync(magpiePidFile, "utf8"));
  expect(Number.isInteger(magpiePid)).toBe(true);
  process.kill(magpiePid, "SIGTERM");
  await expect.poll(async () => page.evaluate(async () => {
    const response = await fetch("/api/status");
    return (await response.json()).runtime_state;
  }), { timeout: 8_000 }).toBe("unavailable");

  const offlineSummary = await readSummary();
  const offlineSessions = await page.evaluate(async () => {
    const response = await fetch("/api/usage/sessions?range=today");
    return { status: response.status, body: await response.json() };
  });
  expect(offlineSummary.status).toBe(200);
  expect(offlineSummary.body.data_revision).toBe(complete.body.data_revision);
  expect(offlineSummary.body.usage).toMatchObject({ input: 10, output: 5 });
  expect(offlineSessions.status).toBe(200);
  expect(offlineSessions.body.data_revision).toBe(complete.body.data_revision);
  expect(offlineSessions.body.items[0].thread.title).toBe("Temporary runtime smoke session");
});

test("schema-one HTTP fixtures expose the current SessionDataset contract", async ({ page }) => {
  await routeStableDashboardData(page);
  await page.goto("/");
  const result = await page.evaluate(async () => {
    const [health, summary, sessions, tools, quota] = await Promise.all([
      fetch("/api/health"),
      fetch("/api/usage/summary?range=year"),
      fetch("/api/usage/sessions?range=year"),
      fetch("/api/usage/tools?days=7"),
      fetch("/api/quota"),
    ]);
    return {
      statuses: [health.status, summary.status, sessions.status, tools.status, quota.status],
      summary: await summary.json(),
      sessions: await sessions.json(),
      tools: await tools.json(),
      quota: await quota.json(),
    };
  });
  expect(result.statuses).toEqual([204, 200, 200, 200, 200]);
  expect(result.summary.usage).toHaveProperty("retried");
  expect(result.summary.usage).toHaveProperty("output_speed");
  expect(result.sessions.data_revision).toBe(REVISION);
  expect(result.sessions.items[0].thread.key).toEqual({ agent: "codex", session_id: "session-1", thread_id: "session-1" });
  expect(result.sessions.items[0]).toHaveProperty("inclusive_usage");
  expect(result.tools).toMatchObject({ days: 7, calls: 100 });
  expect(result.quota.items[0]).toHaveProperty("windows");
});

test("C1 desktop layout keeps the five KPIs, session table, and two chart cards aligned", async ({ page }) => {
  await page.setViewportSize({ width: 1512, height: 1000 });
  await routeStableDashboardData(page);
  await page.goto("/");
  await waitForDashboard(page);

  const themeTokens = await page.evaluate(() => {
    const styles = getComputedStyle(document.documentElement);
    return ["--color-primary", "--color-card", "--color-border"].map((token) => styles.getPropertyValue(token).trim());
  });
  for (const token of themeTokens) expect(token).not.toBe("");

  const rangeTabList = page.locator('[role="tablist"]').first();
  const selectedRangeTab = rangeTabList.locator('[role="tab"][aria-selected="true"]');
  await expect(selectedRangeTab).toHaveCount(1);
  const rangeIndicator = selectedRangeTab.locator("xpath=..").locator(":scope > span").first();
  await expect(rangeIndicator).toHaveCount(1);
  const indicatorStyle = await rangeIndicator.evaluate((node) => {
    const style = getComputedStyle(node);
    const rect = node.getBoundingClientRect();
    return { backgroundColor: style.backgroundColor, width: rect.width, height: rect.height };
  });
  expect(indicatorStyle.backgroundColor).not.toBe("transparent");
  expect(indicatorStyle.backgroundColor).not.toBe("rgba(0, 0, 0, 0)");
  expect(indicatorStyle.width).toBeGreaterThan(0);
  expect(indicatorStyle.height).toBeGreaterThan(0);
  expect(await selectedRangeTab.evaluate((node) => getComputedStyle(node).fontSize)).toBe("14px");

  const refreshButton = page.getByRole("button", { name: "刷新数据", exact: true });
  const themeButton = page.getByRole("button", { name: /Switch to (light|dark) mode/ });
  const stopButton = page.getByRole("button", { name: "停止服务" });
  await expect(refreshButton).toHaveAttribute("title", "刷新数据");
  const controlButtons = [refreshButton, themeButton, stopButton];
  for (const controlButton of controlButtons) {
    await expect(controlButton).toHaveClass(/h-8/);
    await expect(controlButton).toHaveClass(/w-8/);
    await expect(controlButton).toHaveClass(/rounded-lg/);
    expect(await controlButton.evaluate((node) => {
      const { width, height } = node.getBoundingClientRect();
      return [Math.round(width), Math.round(height)];
    })).toEqual([32, 32]);
    const iconBox = await controlButton.locator("svg").boundingBox();
    expect(iconBox?.width).toBeCloseTo(16, 0);
    expect(iconBox?.height).toBeCloseTo(16, 0);
  }

  const shell = page.locator(".dashboard-shell");
  const content = page.locator(".dashboard-content");
  const topLevelStack = content.locator(":scope > div.flex.flex-col.gap-8").first();
  const kpi = page.getByLabel("KPI 指标");
  const cards = kpi.locator(":scope > *");
  await expect(cards).toHaveCount(5);
  await expect(topLevelStack.locator(":scope > *")).toHaveCount(5);
  const topLevelBoxes = await topLevelStack.locator(":scope > *").evaluateAll((nodes) => nodes.map((node) => {
    const rect = node.getBoundingClientRect();
    return { top: rect.top, bottom: rect.bottom };
  }));
  for (const gap of topLevelBoxes.slice(1).map((box, index) => box.top - topLevelBoxes[index].bottom)) {
    expect(gap).toBeGreaterThanOrEqual(31);
    expect(gap).toBeLessThanOrEqual(33);
  }

  const shellBox = await shell.boundingBox();
  const contentBox = await content.boundingBox();
  const kpiBox = await kpi.boundingBox();
  expect(shellBox?.x).toBe(0);
  expect(contentBox?.x).toBe(84);
  expect(contentBox?.width).toBe(1344);
  expect(kpiBox?.x).toBe(100);
  expect(kpiBox?.width).toBe(1312);
  const cardBoxes = await cards.evaluateAll((nodes) => nodes.map((node) => {
    const rect = node.getBoundingClientRect();
    return { width: rect.width, height: rect.height, x: rect.x };
  }));
  expect(cardBoxes.map((box) => Math.round(box.width))).toEqual([304, 236, 236, 236, 236]);
  expect(cardBoxes.map((box) => Math.round(box.height))).toEqual([144, 144, 144, 144, 144]);
  expect(Math.round(cardBoxes[1].x - (cardBoxes[0].x + cardBoxes[0].width))).toBe(16);

  const chartSection = page.getByLabel("使用分布图表");
  const chartCards = chartSection.locator("article");
  await expect(chartCards).toHaveCount(2);
  const chartBoxes = await chartCards.evaluateAll((nodes) => nodes.map((node) => {
    const rect = node.getBoundingClientRect();
    return { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
  }));
  expect(chartBoxes[0].width).toBeCloseTo(chartBoxes[1].width, 0);
  expect(chartBoxes[0].y).toBeCloseTo(chartBoxes[1].y, 0);
  expect(chartBoxes.map((box) => Math.round(box.height))).toEqual([264, 264]);
  await expect(chartCards.nth(0).getByRole("tab", { name: "模型" })).toBeVisible();
  await expect(chartCards.nth(1).getByRole("heading", { name: "Tool 使用分布" })).toBeVisible();

  const sessionSection = page.locator('section[aria-labelledby="session-heading"]');
  const sessionHeadingBox = await sessionSection.getByRole("heading", { name: "Session 记录" }).boundingBox();
  const paginationBox = await sessionSection.getByText("1 / 1", { exact: true }).boundingBox();
  expect((paginationBox?.x ?? 0)).toBeGreaterThan((sessionHeadingBox?.x ?? 0));
  expect(paginationBox?.y).toBeCloseTo(sessionHeadingBox?.y ?? 0, 0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(1512);
  expect(await page.evaluate(() => document.body.scrollWidth)).toBeLessThanOrEqual(1512);
  expect(await illegalStorageWrites(page)).toBe(0);
});

test("KPI view toggle replaces the trailing cards and preserves date, filters, and page content", async ({ page }) => {
  const sessionRequests: string[] = [];
  page.on("request", (request) => {
    if (new URL(request.url()).pathname === "/api/usage/sessions") sessionRequests.push(request.url());
  });
  await routeStableDashboardData(page);
  await page.goto("/");
  await waitForDashboard(page);

  await expect(page.getByRole("tab", { name: "会话" })).toHaveAttribute("aria-selected", "true");
  await expect(page.getByText("会话数")).toBeVisible();
  await expect(page.getByRole("group", { name: "账户额度", exact: true })).toBeVisible();
  await expect(page.getByText("First session")).toBeVisible();

  await page.getByRole("tab", { name: "7d" }).click();
  await expect(page.getByRole("tab", { name: "7d" })).toHaveAttribute("aria-selected", "true");
  await expect.poll(() => sessionRequests.filter((url) => new URL(url).searchParams.get("range") === "7d").length).toBeGreaterThan(0);
  await page.getByRole("button", { name: "Agent筛选，全部" }).click();
  await page.getByRole("checkbox", { name: "Codex" }).click();
  await expect(page.getByRole("button", { name: "Agent筛选，已选1项" })).toBeVisible();
  await expect.poll(() => sessionRequests.filter((url) => new URL(url).searchParams.get("agent") === "codex").length).toBeGreaterThan(0);
  const sessionRequestCount = sessionRequests.length;

  const grid = page.getByLabel("KPI 指标");
  await page.getByRole("tab", { name: "请求" }).click();
  await expect(page.getByRole("tab", { name: "请求" })).toHaveAttribute("aria-selected", "true");
  await expect(grid.locator(":scope > *")).toHaveCount(5);
  await expect(grid.getByText("总 Token")).toBeVisible();
  await expect(grid.getByText("缓存命中")).toBeVisible();
  await expect(grid.getByText("等价费用")).toBeVisible();
  await expect(grid.getByText("请求数")).toBeVisible();
  await expect(grid.getByText("输出速度")).toBeVisible();
  await expect(grid.getByTitle("16")).toBeVisible();
  await expect(grid.getByText("2 次计时")).toBeVisible();
  await expect(grid.getByText("首字平均 7.6s · 4 次计时")).toBeVisible();
  await expect(page.getByRole("button", { name: "Agent筛选，已选1项" })).toBeVisible();
  await expect(page.locator("tr[data-session-thread-key-id]")).toHaveCount(2);
  await expect(page.getByRole("heading", { name: "Tool 使用分布" })).toBeVisible();
  expect(sessionRequests).toHaveLength(sessionRequestCount);

  await page.getByRole("tab", { name: "会话" }).click();
  await expect(page.getByText("会话数")).toBeVisible();
  await expect(page.getByRole("group", { name: "账户额度", exact: true })).toBeVisible();
  await expect(page.getByRole("tab", { name: "7d" })).toHaveAttribute("aria-selected", "true");
  expect(sessionRequests).toHaveLength(sessionRequestCount);
});

test("C2 filters, merged charts, independent Tool range, sorting, and details remain interactive", async ({ page }) => {
  await page.setViewportSize({ width: 1512, height: 1000 });
  const toolRequests: number[] = [];
  await routeStableDashboardData(page, toolRequests);
  await page.goto("/");
  await waitForDashboard(page);

  await expect.poll(() => page.evaluate(() => document.documentElement.classList.contains("dark"))).toBe(true);
  await page.getByRole("button", { name: "Switch to light mode" }).click();
  await expect.poll(() => page.evaluate(() => document.documentElement.classList.contains("dark"))).toBe(false);
  expect(await page.evaluate(() => localStorage.getItem("usagi.theme"))).toBe("light");
  await page.getByRole("button", { name: "Switch to dark mode" }).click();
  await expect.poll(() => page.evaluate(() => document.documentElement.classList.contains("dark"))).toBe(true);

  await page.getByRole("tab", { name: "7d" }).click();
  await expect(page.getByRole("tab", { name: "7d" })).toHaveAttribute("aria-selected", "true");
  await page.getByRole("tab", { name: "自定义", exact: true }).click();
  const customRangeDialog = page.getByRole("dialog", { name: "自定义日期范围" });
  await expect(customRangeDialog).toBeVisible();
  const customRangeBox = await customRangeDialog.boundingBox();
  expect(customRangeBox).not.toBeNull();
  expect(customRangeBox?.x ?? -1).toBeGreaterThanOrEqual(0);
  expect(customRangeBox?.y ?? -1).toBeGreaterThanOrEqual(0);
  expect((customRangeBox?.x ?? 0) + (customRangeBox?.width ?? 0)).toBeLessThanOrEqual(1512);
  expect((customRangeBox?.y ?? 0) + (customRangeBox?.height ?? 0)).toBeLessThanOrEqual(1000);
  await page.keyboard.press("Escape");
  await expect(customRangeDialog).toBeHidden();

  const kpi = page.getByLabel("KPI 指标");
  await page.getByRole("button", { name: "模型筛选，全部" }).click();
  await page.getByRole("checkbox", { name: "gpt-5" }).click();
  await page.keyboard.press("Escape");
  await expect(kpi.locator('span[title="1,150"]')).toHaveCount(1);
  await page.getByRole("button", { name: "项目筛选，全部" }).click();
  await page.getByRole("checkbox", { name: "Usagi" }).click();
  await page.keyboard.press("Escape");
  await expect(kpi.locator('span[title="750"]')).toHaveCount(1);
  await expect(page.locator("tr[data-session-thread-key-id]")).toHaveCount(1);

  const chartSection = page.getByLabel("使用分布图表");
  const distributionCard = chartSection.locator("article").nth(0);
  await distributionCard.getByRole("tab", { name: "费用" }).click();
  await expect(distributionCard.getByRole("img", { name: "模型分布费用分布" })).toBeVisible();
  await distributionCard.getByRole("tab", { name: "项目" }).click();
  await expect(distributionCard.getByRole("img", { name: "项目分布费用分布" })).toBeVisible();
  await distributionCard.getByRole("tab", { name: "模型" }).click();
  await expect(distributionCard.getByRole("img", { name: "模型分布费用分布" })).toBeVisible();
  const legendButtons = distributionCard.locator("button:not([role=tab])");
  await expect.poll(() => legendButtons.count()).toBeGreaterThan(1);
  await legendButtons.nth(0).focus();
  await expect.poll(async () => Number(await legendButtons.nth(1).evaluate((node) => getComputedStyle(node).opacity))).toBeLessThan(1);

  const toolCard = chartSection.locator("article").nth(1);
  await expect(toolCard.locator("button[aria-label]")).toHaveCount(9);
  await expect(toolCard.getByRole("button", { name: /其他，20 次，20.0%/ })).toBeVisible();
  await toolCard.getByRole("tab", { name: "7 天" }).click();
  await expect(toolCard.getByRole("tab", { name: "7 天" })).toHaveAttribute("aria-selected", "true");
  await expect.poll(() => toolRequests.includes(7)).toBe(true);
  const toolRequestsBeforePageChange = toolRequests.length;
  await page.getByRole("tab", { name: "今天" }).first().click();
  await expect(toolCard.getByRole("tab", { name: "7 天" })).toHaveAttribute("aria-selected", "true");
  await page.waitForTimeout(150);
  expect(toolRequests.slice(toolRequestsBeforePageChange)).toEqual([]);

  await page.getByRole("button", { name: "清除筛选" }).click();
  const sessionRows = page.locator("tr[data-session-thread-key-id]");
  await expect(sessionRows).toHaveCount(2);
  await page.getByRole("button", { name: "合计费用" }).click();
  await expect(sessionRows.first()).toHaveAttribute("data-session-thread-key-id", /session-2/);
  await sessionRows.first().click();
  const dialog = page.getByRole("dialog", { name: "Session 详情" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("heading", { name: "Detail session-2" })).toBeVisible();

  const mainFirst = dialog.getByRole("button", { name: "gpt-5 (high)" });
  const mainSecond = dialog.getByRole("button", { name: "o4-mini (—)" });
  await expect(mainFirst).toHaveAttribute("aria-expanded", "false");
  await expect(mainSecond).toHaveAttribute("aria-expanded", "false");
  await mainFirst.click();
  await expect(mainFirst).toHaveAttribute("aria-expanded", "true");
  await mainSecond.click();
  await expect(mainFirst).toHaveAttribute("aria-expanded", "false");
  await expect(mainSecond).toHaveAttribute("aria-expanded", "true");
  const childFirst = dialog.getByRole("button", { name: "Recent subagent" });
  const childSecond = dialog.getByRole("button", { name: "Old subagent" });
  await childFirst.click();
  await expect(childFirst).toHaveAttribute("aria-expanded", "true");
  await childSecond.click();
  await expect(childFirst).toHaveAttribute("aria-expanded", "false");
  await expect(childSecond).toHaveAttribute("aria-expanded", "true");
  await dialog.getByRole("button", { name: "关闭 Session 详情" }).click();
  await expect(dialog).toBeHidden();
  expect(await illegalStorageWrites(page)).toBe(0);
});

test("C3 narrow viewport keeps chart and drawer within the screen while the table scrolls", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await routeStableDashboardData(page);
  await page.goto("/");
  await waitForDashboard(page);

  const kpiCards = page.getByLabel("KPI 指标").locator(":scope > *");
  await expect(kpiCards).toHaveCount(5);
  const kpiColumns = await kpiCards.evaluateAll((nodes) => new Set(nodes.map((node) => Math.round(node.getBoundingClientRect().x))).size);
  expect(kpiColumns).toBe(1);
  const chartCards = page.getByLabel("使用分布图表").locator("article");
  const chartBoxes = await chartCards.evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect()));
  expect(chartBoxes[0].x).toBeCloseTo(chartBoxes[1].x, 0);
  expect(chartBoxes[1].y).toBeGreaterThan(chartBoxes[0].y + chartBoxes[0].height);

  const table = page.getByRole("table");
  const tableScroller = table.locator("xpath=..");
  const scrollState = await tableScroller.evaluate((node) => ({
    clientWidth: (node as HTMLElement).clientWidth,
    scrollWidth: (node as HTMLElement).scrollWidth,
    overflowX: getComputedStyle(node).overflowX,
  }));
  expect(scrollState.scrollWidth).toBeGreaterThan(scrollState.clientWidth);
  expect(["auto", "scroll"]).toContain(scrollState.overflowX);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);

  await page.locator("tr[data-session-thread-key-id]").first().click();
  const dialog = page.getByRole("dialog", { name: "Session 详情" });
  await expect(dialog).toBeVisible();
  await expect.poll(async () => (await dialog.boundingBox())?.x ?? Number.POSITIVE_INFINITY).toBeCloseTo(0, 0);
  await expect.poll(async () => (await dialog.boundingBox())?.width ?? 0).toBeCloseTo(390, 0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  await dialog.getByRole("button", { name: "关闭 Session 详情" }).click();
  await expect(dialog).toBeHidden();
  expect(await illegalStorageWrites(page)).toBe(0);
});

test("T-Q-008 Codex quota stays last, shows account details, and fits required viewports", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  await routeStableDashboardData(page);
  await page.goto("/");
  await waitForDashboard(page);

  const cards = page.getByLabel("KPI 指标").locator(":scope > *");
  await expect(cards).toHaveCount(5);
  await expect(cards.last().getByText("账户额度")).toBeVisible();
  const cardWidths = await cards.evaluateAll((nodes) => nodes.map((node) => Math.round(node.getBoundingClientRect().width)));
  expect(cardWidths.slice(1)).toEqual([236, 236, 236, 236]);
  expect(cardWidths[0]).toBeLessThan(236);
  const quotaAccount = cards.last().getByRole("button", { name: "Codex 账户信息" });
  await expect(quotaAccount).toHaveText("Pro 5x");
  await quotaAccount.hover();
  await expect(page.getByRole("dialog")).toContainText("hoge@example.com");

  for (const width of [1280, 767]) {
    await page.setViewportSize({ width, height: 1000 });
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    await expect.poll(() => page.evaluate(() => document.body.scrollWidth)).toBeLessThanOrEqual(width);
  }
});
