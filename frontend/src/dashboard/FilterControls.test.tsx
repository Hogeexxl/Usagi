import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DashboardFilters,
  FilterOptionsResponse,
} from "../data/types";
import { FilterControls } from "./FilterControls";

const modelOptions: FilterOptionsResponse = {
  data_revision: 1,
  sources: [],
  models: [
    { model: "gpt-4o", provider: "openai" },
    { model: "gpt-4o-mini", provider: "openai" },
    { model: "codex-auto-review", provider: "openai" },
    { model: "claude-3", provider: "route-models" },
    { model: "github-copilot/gpt-5.6-luna", provider: "route-models" },
    { model: "gemini-2.5", provider: "route-models" },
  ],
  projects: [],
};

const projectOptions: FilterOptionsResponse = {
  data_revision: 1,
  sources: [],
  models: [],
  projects: [
    { kind: "project", project_name: "Workspace", project_path: "/workspace" },
    { kind: "projectless" },
    { kind: "unknown" },
  ],
};

const emptyFilters: DashboardFilters = { sources: [], models: [], projects: [] };

type RenderOverrides = {
  filters?: DashboardFilters;
  options?: FilterOptionsResponse | null;
  optionsLoading?: boolean;
  optionsStale?: boolean;
  optionsErrorCode?: string;
  anyFilterActive?: boolean;
  onChange?: (filters: DashboardFilters) => void;
  onClear?: () => void;
  onRetryOptions?: () => void;
};

function renderControls(overrides: RenderOverrides = {}) {
  const props = {
    filters: emptyFilters,
    options: modelOptions,
    optionsLoading: false,
    optionsStale: false,
    optionsErrorCode: undefined,
    anyFilterActive: false,
    onChange: vi.fn(),
    onClear: vi.fn(),
    onRetryOptions: vi.fn(),
    ...overrides,
  };
  return render(<FilterControls {...props} />);
}

async function openPopover(name: string) {
  fireEvent.click(screen.getByRole("button", { name }));
  return screen.findByRole("dialog");
}

describe("FilterControls", () => {
  it("uses the secondary all-model trigger when no model is selected", () => {
    renderControls();
    const trigger = screen.getByRole("button", { name: "模型筛选，全部" });
    expect(trigger).toHaveTextContent("模型 · 全部");
    expect(trigger).toHaveClass("bg-card");
  });

  it("uses the primary one-item trigger when one model is selected", () => {
    renderControls({ filters: { sources: [], models: ["claude-3"], projects: [] } });
    const trigger = screen.getByRole("button", { name: "模型筛选，已选1项" });
    expect(trigger).toHaveTextContent("模型 · 1 项");
    expect(trigger).toHaveClass("bg-primary");
  });

  it("toggles an ordinary model from its checkbox", async () => {
    const onChange = vi.fn();
    renderControls({ onChange });
    const dialog = await openPopover("模型筛选，全部");
    const checkbox = within(dialog).getByRole("checkbox", { name: "claude-3" });

    fireEvent.click(checkbox);

    expect(onChange).toHaveBeenCalledWith({ sources: [], models: ["claude-3"], projects: [] });
  });

  it("toggles an ordinary model when its label text is clicked", async () => {
    const onChange = vi.fn();
    renderControls({ onChange });
    const dialog = await openPopover("模型筛选，全部");

    fireEvent.click(within(dialog).getByText("claude-3"));

    expect(onChange).toHaveBeenCalledWith({ sources: [], models: ["claude-3"], projects: [] });
  });

  it.each([
    { name: "0/N", selected: [], ariaChecked: "false" },
    { name: "partial", selected: ["gpt-4o"], ariaChecked: "mixed" },
    { name: "N/N", selected: ["gpt-4o", "gpt-4o-mini", "codex-auto-review"], ariaChecked: "true" },
  ])("exposes OpenAI $name state", async ({ selected, ariaChecked }) => {
    renderControls({ filters: { sources: [], models: selected, projects: [] } });
    const triggerName = selected.length
      ? `模型筛选，已选${selected.length}项`
      : "模型筛选，全部";
    const dialog = await openPopover(triggerName);

    expect(within(dialog).getByRole("checkbox", { name: "OpenAI" })).toHaveAttribute(
      "aria-checked",
      ariaChecked,
    );
  });

  it("renders OpenAI and Route-models as independent collapsible groups", async () => {
    renderControls();
    const dialog = await openPopover("模型筛选，全部");
    const openAiToggle = within(dialog).getByRole("button", { name: "OpenAI" });
    const routeModelsToggle = within(dialog).getByRole("button", { name: "Route-models" });

    expect(within(dialog).getByRole("checkbox", { name: "gpt-4o" })).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "claude-3" })).toBeInTheDocument();
    fireEvent.click(openAiToggle);
    expect(within(dialog).queryByRole("checkbox", { name: "gpt-4o" })).not.toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "claude-3" })).toBeInTheDocument();
    fireEvent.click(routeModelsToggle);
    expect(within(dialog).queryByRole("checkbox", { name: "claude-3" })).not.toBeInTheDocument();
    fireEvent.click(openAiToggle);
    fireEvent.click(routeModelsToggle);
    expect(within(dialog).getByRole("checkbox", { name: "gpt-4o" })).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "claude-3" })).toBeInTheDocument();
  });

  it("uses backend provider metadata instead of guessing from model names", async () => {
    renderControls();
    const dialog = await openPopover("模型筛选，全部");

    expect(within(dialog).getByRole("button", { name: "OpenAI" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Route-models" })).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "github-copilot/gpt-5.6-luna" })).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "gemini-2.5" })).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "codex-auto-review" })).toBeInTheDocument();
  });

  it("keeps a selected orphan model cancellable in the Route-models fallback group", async () => {
    const onChange = vi.fn();
    renderControls({
      filters: { sources: [], models: ["orphan-rollout"], projects: [] },
      onChange,
    });
    const dialog = await openPopover("模型筛选，已选1项");
    const orphan = within(dialog).getByRole("checkbox", { name: "orphan-rollout" });

    expect(within(dialog).getByRole("button", { name: "Route-models" })).toBeInTheDocument();
    fireEvent.click(orphan);

    expect(onChange).toHaveBeenCalledWith({ sources: [], models: [], projects: [] });
  });

  it("selects normal, projectless, and unknown projects using their labels", async () => {
    const onChange = vi.fn();
    renderControls({ options: projectOptions, onChange });
    const dialog = await openPopover("项目筛选，全部");

    fireEvent.click(within(dialog).getByText("Workspace"));
    expect(onChange).toHaveBeenLastCalledWith({
      sources: [],
      models: [],
      projects: [{ kind: "project", project_path: "/workspace" }],
    });

    fireEvent.click(within(dialog).getByText("无项目会话"));
    expect(onChange).toHaveBeenLastCalledWith({
      sources: [],
      models: [],
      projects: [{ kind: "projectless" }],
    });

    fireEvent.click(within(dialog).getByText("未识别项目"));
    expect(onChange).toHaveBeenLastCalledWith({
      sources: [],
      models: [],
      projects: [{ kind: "unknown" }],
    });
  });

  it("shows the selected project count and primary trigger semantics", () => {
    renderControls({
      filters: { sources: [], models: [], projects: [{ kind: "project", project_path: "/workspace" }] },
      options: projectOptions,
      anyFilterActive: true,
    });
    const trigger = screen.getByRole("button", { name: "项目筛选，已选1项" });

    expect(trigger).toHaveTextContent("项目 · 1 项");
    expect(trigger).toHaveClass("bg-primary");
  });

  it.each([
    { trigger: "模型筛选，全部", options: modelOptions },
    { trigger: "项目筛选，全部", options: projectOptions },
  ])("opens $trigger without entrance animation", async ({ trigger, options }) => {
    renderControls({ options });
    await openPopover(trigger);

    const portal = document.querySelector("[data-morph-popover-portal]");
    expect(portal).toHaveAttribute("data-morph-popover-animated", "false");
  });

  it("opens the model popover and closes it on Escape and outside pointer", async () => {
    renderControls();
    const trigger = screen.getByRole("button", { name: "模型筛选，全部" });

    await openPopover("模型筛选，全部");
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());

    await openPopover("模型筛选，全部");
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    fireEvent.pointerDown(document.body);
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });

  it.each([
    { label: "error", optionsErrorCode: "QUERY_FAILED", optionsStale: false, status: "选项加载失败" },
    { label: "stale", optionsErrorCode: undefined, optionsStale: true, status: "选项可能已更新" },
  ])("shows a retry action for $label options", async ({ optionsErrorCode, optionsStale, status }) => {
    const onRetryOptions = vi.fn();
    renderControls({ optionsErrorCode, optionsStale, onRetryOptions });
    const dialog = await openPopover("模型筛选，全部");

    expect(within(dialog).getByRole("status")).toHaveTextContent(status);
    fireEvent.click(within(dialog).getByRole("button", { name: "重试" }));
    expect(onRetryOptions).toHaveBeenCalledTimes(1);
  });

  it.each([
    { anyFilterActive: true, visible: true },
    { anyFilterActive: false, visible: false },
  ])("$anyFilterActive anyFilterActive controls clear visibility", ({ anyFilterActive, visible }) => {
    renderControls({ anyFilterActive });
    const clear = screen.queryByRole("button", { name: "清除筛选" });
    if (visible) expect(clear).toBeInTheDocument();
    else expect(clear).not.toBeInTheDocument();
  });

  it("clears model and project filters without changing a range", () => {
    const onClear = vi.fn();
    const onChange = vi.fn();
    renderControls({
      filters: { sources: [], models: ["claude-3"], projects: [{ kind: "projectless" }] },
      anyFilterActive: true,
      onClear,
      onChange,
    });

    fireEvent.click(screen.getByRole("button", { name: "清除筛选" }));

    expect(onClear).toHaveBeenCalledTimes(1);
    expect(onChange).not.toHaveBeenCalled();
  });

  it("hides the source filter button when options are null, empty, or have only 1 source", () => {
    // null options
    const firstRender = renderControls({ options: null });
    expect(screen.queryByRole("button", { name: /来源/ })).not.toBeInTheDocument();
    firstRender.unmount();

    // 0 sources
    const secondRender = renderControls({ options: { data_revision: 1, sources: [], models: [], projects: [] } });
    expect(screen.queryByRole("button", { name: /来源/ })).not.toBeInTheDocument();
    secondRender.unmount();

    // 1 source
    const thirdRender = renderControls({
      options: {
        data_revision: 1,
        sources: [{ source: "codex", display_name: "Codex" }],
        models: [],
        projects: [],
      },
    });
    expect(screen.queryByRole("button", { name: /来源/ })).not.toBeInTheDocument();
    thirdRender.unmount();
  });

  it("shows the source filter button when sources > 1, opens popover, and toggles selection", async () => {
    const onChange = vi.fn();
    const multiSourceOptions: FilterOptionsResponse = {
      data_revision: 1,
      sources: [
        { source: "codex", display_name: "Codex" },
        { source: "antigravity", display_name: "Antigravity" },
      ],
      models: [],
      projects: [],
    };

    renderControls({
      options: multiSourceOptions,
      filters: { sources: [], models: [], projects: [] },
      onChange,
    });

    const trigger = screen.getByRole("button", { name: "来源筛选，全部" });
    expect(trigger).toBeInTheDocument();
    expect(trigger).toHaveTextContent("来源 · 全部");

    const dialog = await openPopover("来源筛选，全部");
    expect(dialog).toBeInTheDocument();
    expect(within(dialog).getByText("Codex")).toBeInTheDocument();
    expect(within(dialog).getByText("Antigravity")).toBeInTheDocument();

    const antigravityCheckbox = within(dialog).getByRole("checkbox", { name: "Antigravity" });
    fireEvent.click(antigravityCheckbox);

    expect(onChange).toHaveBeenCalledWith({
      sources: ["antigravity"],
      models: [],
      projects: [],
    });
  });

  it("displays and allows toggling orphan selected sources when sources > 1", async () => {
    const onChange = vi.fn();
    const multiSourceOptions: FilterOptionsResponse = {
      data_revision: 1,
      sources: [
        { source: "codex", display_name: "Codex" },
        { source: "antigravity", display_name: "Antigravity" },
      ],
      models: [],
      projects: [],
    };

    renderControls({
      options: multiSourceOptions,
      filters: { sources: ["orphan-source"], models: [], projects: [] },
      onChange,
    });

    const trigger = screen.getByRole("button", { name: "来源筛选，已选1项" });
    expect(trigger).toHaveTextContent("来源 · 1 项");

    const dialog = await openPopover("来源筛选，已选1项");
    const orphanCheckbox = within(dialog).getByRole("checkbox", { name: "orphan-source" });
    expect(orphanCheckbox).toBeInTheDocument();
    expect(orphanCheckbox).toBeChecked();

    fireEvent.click(orphanCheckbox);
    expect(onChange).toHaveBeenCalledWith({
      sources: [],
      models: [],
      projects: [],
    });
  });
});
