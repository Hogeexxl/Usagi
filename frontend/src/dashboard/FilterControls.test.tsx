import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DashboardFilters,
  FilterOptionsResponse,
} from "../data/types";
import { FilterControls } from "./FilterControls";
import type { PersistedFilterOptions } from "./filterPersistence";

const modelOptions: FilterOptionsResponse = {
  data_revision: 1,
  agents: [],
  models: [
    { model: "gpt-4o", provider_id: "openai", provider_name: "OpenAI" },
    { model: "gpt-4o-mini", provider_id: "openai", provider_name: "OpenAI" },
    { model: "codex-auto-review", provider_id: "openai", provider_name: "OpenAI" },
    { model: "claude-3", provider_id: "route-models", provider_name: "Route-models" },
    { model: "github-copilot/gpt-5.6-luna", provider_id: "route-models", provider_name: "Route-models" },
    { model: "gemini-2.5", provider_id: "route-models", provider_name: "Route-models" },
  ],
  projects: [],
};

const projectOptions: FilterOptionsResponse = {
  data_revision: 1,
  agents: [],
  models: [],
  projects: [
    { kind: "project", name: "Workspace", path: "/workspace" },
    { kind: "workspace" },
    { kind: "projectless" },
    { kind: "unknown" },
  ],
};

function modelSelections(...models: string[]) {
  return models.map((model) => ({
    provider_id: model.startsWith("claude-") ? "route-models" : model.startsWith("gemini-") ? "antigravity" : model.startsWith("gpt-") || model.startsWith("codex-") ? "openai" : "orphan-provider",
    model,
  }));
}

const emptyFilters: DashboardFilters = { agents: [], models: [], projects: [] };

type RenderOverrides = {
  filters?: DashboardFilters;
  options?: FilterOptionsResponse | null;
  optionsLoading?: boolean;
  optionsErrorCode?: string;
  savedOptions?: PersistedFilterOptions;
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

    optionsErrorCode: undefined,
    savedOptions: undefined,
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

function ensureModelGroupExpanded(dialog: HTMLElement, name: string) {
  const toggle = within(dialog).getByRole("button", { name });
  if (toggle.getAttribute("aria-expanded") !== "true") fireEvent.click(toggle);
}

describe("FilterControls", () => {
  it("sorts models lexicographically within backend-ordered dynamic providers", async () => {
    renderControls({ options: {
      ...modelOptions,
      models: [
        { model: "gpt-5.4", provider_id: "openai", provider_name: "OpenAI" },
        { model: "gpt-5.4-mini", provider_id: "openai", provider_name: "OpenAI" },
        { model: "gpt-6.1-sol", provider_id: "openai", provider_name: "OpenAI" },
        { model: "gpt-6-sol", provider_id: "openai", provider_name: "OpenAI" },
        { model: "gpt-6-luna", provider_id: "openai", provider_name: "OpenAI" },
        { model: "unknown-10", provider_id: "openai", provider_name: "OpenAI" },
        { model: "unknown-2", provider_id: "openai", provider_name: "OpenAI" },
        { model: "claude-opus-4-6-thinking", provider_id: "antigravity", provider_name: "Antigravity" },
        { model: "gemini-3.8-flash", provider_id: "antigravity", provider_name: "Antigravity" },
        { model: "copilot/claude-opus-5.5", provider_id: "route-models", provider_name: "Route-models" },
        { model: "copiLot/cLaude-sonnet-5.5", provider_id: "route-models", provider_name: "Route-models" },
        { model: "claude-fable-5-1", provider_id: "route-models", provider_name: "Route-models" },
        { model: "custom/gemini-3.8-flash-high", provider_id: "route-models", provider_name: "Route-models" },
        { model: "github-copilot/gpt-5.6-luna", provider_id: "route-models", provider_name: "Route-models" },
      ],
    } });
    const dialog = await openPopover("模型筛选，全部");
    ensureModelGroupExpanded(dialog, "Antigravity");
    ensureModelGroupExpanded(dialog, "Route-models");
    expect(within(dialog).getAllByRole("checkbox").map(node => node.getAttribute("aria-label") ?? node.closest("label")?.textContent)).toEqual([
      "OpenAI", "gpt-5.4", "gpt-5.4-mini", "gpt-6-luna", "gpt-6-sol", "gpt-6.1-sol", "unknown-2", "unknown-10",
      "Antigravity", "claude-opus-4-6-thinking", "gemini-3.8-flash",
      "Route-models", "claude-fable-5-1", "copilot/claude-opus-5.5", "copiLot/cLaude-sonnet-5.5", "custom/gemini-3.8-flash-high", "github-copilot/gpt-5.6-luna",
    ]);
  });

  it("uses the secondary all-model trigger when no model is selected", () => {
    renderControls();
    const trigger = screen.getByRole("button", { name: "模型筛选，全部" });
    expect(trigger).toHaveTextContent("模型 · 全部");
    expect(trigger).toHaveClass("bg-card");
  });

  it("uses the primary one-item trigger when one model is selected", () => {
    renderControls({ filters: { agents: [], models: modelSelections("claude-3"), projects: [] } });
    const trigger = screen.getByRole("button", { name: "模型筛选，已选1项" });
    expect(trigger).toHaveTextContent("模型 · 1 项");
    expect(trigger).toHaveClass("bg-primary");
  });

  it("toggles an ordinary model from its checkbox", async () => {
    const onChange = vi.fn();
    renderControls({ onChange });
    const dialog = await openPopover("模型筛选，全部");
    ensureModelGroupExpanded(dialog, "Route-models");
    const checkbox = within(dialog).getByRole("checkbox", { name: "claude-3" });

    fireEvent.click(checkbox);

    expect(onChange).toHaveBeenCalledWith({ agents: [], models: modelSelections("claude-3"), projects: [] });
  });

  it("toggles an ordinary model when its label text is clicked", async () => {
    const onChange = vi.fn();
    renderControls({ onChange });
    const dialog = await openPopover("模型筛选，全部");
    ensureModelGroupExpanded(dialog, "Route-models");

    fireEvent.click(within(dialog).getByText("claude-3"));

    expect(onChange).toHaveBeenCalledWith({ agents: [], models: modelSelections("claude-3"), projects: [] });
  });

  it.each([
    { name: "0/N", selected: [], ariaChecked: "false" },
    { name: "partial", selected: modelSelections("gpt-4o"), ariaChecked: "mixed" },
    { name: "N/N", selected: modelSelections("gpt-4o", "gpt-4o-mini", "codex-auto-review"), ariaChecked: "true" },
  ])("exposes OpenAI $name state", async ({ selected, ariaChecked }) => {
    renderControls({ filters: { agents: [], models: selected, projects: [] } });
    const triggerName = selected.length
      ? `模型筛选，已选${selected.length}项`
      : "模型筛选，全部";
    const dialog = await openPopover(triggerName);

    expect(within(dialog).getByRole("checkbox", { name: "OpenAI" })).toHaveAttribute(
      "aria-checked",
      ariaChecked,
    );
  });

  it("expands only the first model group initially and preserves later group state", async () => {
    renderControls();
    const dialog = await openPopover("模型筛选，全部");
    const openAiToggle = within(dialog).getByRole("button", { name: "OpenAI" });
    const routeModelsToggle = within(dialog).getByRole("button", { name: "Route-models" });

    expect(openAiToggle).toHaveAttribute("aria-expanded", "true");
    expect(routeModelsToggle).toHaveAttribute("aria-expanded", "false");
    expect(within(dialog).getByRole("checkbox", { name: "gpt-4o" })).toBeInTheDocument();
    expect(within(dialog).queryByRole("checkbox", { name: "claude-3" })).not.toBeInTheDocument();

    fireEvent.click(openAiToggle);
    fireEvent.click(routeModelsToggle);
    expect(openAiToggle).toHaveAttribute("aria-expanded", "false");
    expect(routeModelsToggle).toHaveAttribute("aria-expanded", "true");
    expect(within(dialog).queryByRole("checkbox", { name: "gpt-4o" })).not.toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "claude-3" })).toBeInTheDocument();

    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    const reopened = await openPopover("模型筛选，全部");
    expect(within(reopened).getByRole("button", { name: "OpenAI" })).toHaveAttribute("aria-expanded", "false");
    expect(within(reopened).getByRole("button", { name: "Route-models" })).toHaveAttribute("aria-expanded", "true");
  });

  it("uses backend provider metadata instead of guessing from model names", async () => {
    renderControls();
    const dialog = await openPopover("模型筛选，全部");
    ensureModelGroupExpanded(dialog, "Route-models");

    expect(within(dialog).getByRole("button", { name: "OpenAI" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Route-models" })).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "github-copilot/gpt-5.6-luna" })).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "gemini-2.5" })).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "codex-auto-review" })).toBeInTheDocument();
  });

  it("pins a selected model the range has no data for at the top, still selected and cancellable", async () => {
    const onChange = vi.fn();
    renderControls({
      filters: { agents: [], models: modelSelections("orphan-rollout"), projects: [] },
      onChange,
    });
    const dialog = await openPopover("模型筛选，已选1项");
    const orphan = within(dialog).getByRole("checkbox", { name: "orphan-rollout · orphan-provider" });
    expect(orphan).toHaveAttribute("aria-checked", "true");
    expect(within(dialog).getAllByRole("checkbox")[0]).toBe(orphan);
    expect(within(dialog).getByText("当前范围无数据")).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: "已选" })).not.toBeInTheDocument();

    fireEvent.click(orphan);
    expect(onChange).toHaveBeenCalledWith({ agents: [], models: [], projects: [] });
  });

  it("lists selections without the no-data hint while the range's options are not loaded", async () => {
    renderControls({ options: null, optionsLoading: true, filters: { agents: [], models: modelSelections("gpt-4o"), projects: [] }, anyFilterActive: true });
    const dialog = await openPopover("模型筛选，已选1项");
    expect(within(dialog).getByRole("checkbox", { name: "gpt-4o · openai" })).toHaveAttribute("aria-checked", "true");
    expect(within(dialog).queryByText("当前范围无数据")).not.toBeInTheDocument();
  });

  it("labels unavailable selections with names seen in earlier ranges", async () => {
    const props = { filters: { agents: ["codex"], models: [{ provider_id: "openai", model: "gpt-4o" }], projects: [{ kind: "project" as const, path: "/workspace" }] }, optionsLoading: false, anyFilterActive: true, onChange: vi.fn(), onClear: vi.fn(), onRetryOptions: vi.fn() };
    const { rerender } = render(<FilterControls {...props} options={{ ...projectOptions, agents: [{ agent: "codex", display_name: "Codex" }], models: modelOptions.models }} />);
    rerender(<FilterControls {...props} options={{ data_revision: 2, agents: [], models: [], projects: [] }} />);
    expect(within(await openPopover("模型筛选，已选1项")).getByRole("checkbox", { name: "gpt-4o · OpenAI" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(within(await openPopover("项目筛选，已选1项")).getByRole("checkbox", { name: "Workspace" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    const agents = await openPopover("Agent筛选，已选1项");
    expect(within(agents).getByRole("checkbox", { name: "Codex" })).toHaveAttribute("aria-checked", "true");
    expect(within(agents).getByText("当前范围暂无 Agent")).toBeInTheDocument();
  });

  it("uses persisted labels before the current range options arrive", async () => {
    renderControls({
      options: null,
      optionsLoading: true,
      anyFilterActive: true,
      filters: {
        agents: ["codex"],
        models: [{ provider_id: "openai", model: "gpt-5" }],
        projects: [{ kind: "project", path: "/work/Usagi" }],
      },
      savedOptions: {
        agents: [{ agent: "codex", display_name: "Codex" }],
        models: [{ provider_id: "openai", model: "gpt-5", provider_name: "OpenAI" }],
        projects: [{ kind: "project", path: "/work/Usagi", name: "Usagi" }],
      },
    });

    expect(within(await openPopover("Agent筛选，已选1项")).getByRole("checkbox", { name: "Codex" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(within(await openPopover("模型筛选，已选1项")).getByRole("checkbox", { name: "gpt-5 · OpenAI" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(within(await openPopover("项目筛选，已选1项")).getByRole("checkbox", { name: "Usagi" })).toBeInTheDocument();
  });


  it("waits for the first model group and then preserves group state", async () => {
    const props = { filters: emptyFilters, optionsLoading: false, anyFilterActive: false, onChange: vi.fn(), onClear: vi.fn(), onRetryOptions: vi.fn() };
    const { rerender } = render(<FilterControls {...props} options={{ data_revision: 1, agents: [], models: [], projects: [] }} />);

    const dialog = await openPopover("模型筛选，全部");
    expect(within(dialog).getByText("当前范围暂无模型")).toBeInTheDocument();
    rerender(<FilterControls {...props} options={{ data_revision: 2, agents: [], models: [{ model: "gpt-4o", provider_id: "openai", provider_name: "OpenAI" }], projects: [] }} />);
    const openAiToggle = within(screen.getByRole("dialog")).getByRole("button", { name: "OpenAI" });
    expect(openAiToggle).toHaveAttribute("aria-expanded", "true");

    fireEvent.click(openAiToggle);
    rerender(<FilterControls {...props} options={{
      data_revision: 3,
      agents: [],
      models: [
        { model: "gpt-4o", provider_id: "openai", provider_name: "OpenAI" },
        { model: "gemini-2.5", provider_id: "antigravity", provider_name: "Antigravity" },
      ],
      projects: [],
    }} />);
    const changedDialog = screen.getByRole("dialog");
    expect(within(changedDialog).getByRole("button", { name: "OpenAI" })).toHaveAttribute("aria-expanded", "false");
    expect(within(changedDialog).getByRole("button", { name: "Antigravity" })).toHaveAttribute("aria-expanded", "false");
  });


  it("selects normal projects and each special project kind from one-level labels", async () => {
    const onChange = vi.fn();
    renderControls({ options: projectOptions, onChange });
    const dialog = await openPopover("项目筛选，全部");

    fireEvent.click(within(dialog).getByText("Workspace"));
    expect(onChange).toHaveBeenLastCalledWith({
      agents: [],
      models: [],
      projects: [{ kind: "project", path: "/workspace" }],
    });

    fireEvent.click(within(dialog).getByText("临时工作区"));
    expect(onChange).toHaveBeenLastCalledWith({
      agents: [],
      models: [],
      projects: [{ kind: "workspace" }],
    });

    fireEvent.click(within(dialog).getByText("无项目会话"));
    expect(onChange).toHaveBeenLastCalledWith({
      agents: [],
      models: [],
      projects: [{ kind: "projectless" }],
    });

    fireEvent.click(within(dialog).getByText("未知会话"));
    expect(onChange).toHaveBeenLastCalledWith({
      agents: [],
      models: [],
      projects: [{ kind: "unknown" }],
    });
  });

  it.each([
    { trigger: "模型筛选，全部", options: modelOptions },
    { trigger: "项目筛选，全部", options: projectOptions },
  ])("caps $trigger content at 624px and enables vertical scrolling", async ({ trigger, options }) => {
    renderControls({ options });
    const dialog = await openPopover(trigger);
    const scrollArea = dialog.querySelector<HTMLElement>("[data-multi-select-scroll-area]");
    if (!scrollArea) throw new Error("Scroll area not found");
    expect(scrollArea).toHaveStyle({ maxHeight: "624px", overflowY: "auto" });
  });

  it("shows the selected project count and primary trigger semantics", () => {
    renderControls({
      filters: { agents: [], models: [], projects: [{ kind: "project", path: "/workspace" }] },
      options: projectOptions,
      anyFilterActive: true,
    });
    const trigger = screen.getByRole("button", { name: "项目筛选，已选1项" });

    expect(trigger).toHaveTextContent("项目 · 1 项");
    expect(trigger).toHaveClass("bg-primary");
  });

  it("keeps the filter surface mounted across open/close like beUI Multi Select", async () => {
    renderControls();
    const panels = Array.from(
      document.querySelectorAll<HTMLElement>("[data-multi-select-content]"),
    );
    expect(panels).toHaveLength(3);

    const modelPanel = panels.find((panel) => panel.textContent?.includes("OpenAI"));
    if (!modelPanel) throw new Error("Model panel was not mounted");
    expect(modelPanel).toHaveAttribute("aria-hidden", "true");
    expect(modelPanel).toHaveAttribute("inert");

    await openPopover("模型筛选，全部");
    expect(modelPanel).toHaveAttribute("aria-hidden", "false");
    expect(modelPanel).not.toHaveAttribute("inert");

    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(modelPanel).toHaveAttribute("aria-hidden", "true");
    expect(document.body.contains(modelPanel)).toBe(true);
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

  it("shows a retry action for failed options", async () => {
    const onRetryOptions = vi.fn();
    renderControls({ optionsErrorCode: "QUERY_FAILED", onRetryOptions });
    const dialog = await openPopover("模型筛选，全部");

    expect(within(dialog).getByRole("status")).toHaveTextContent("选项加载失败");
    expect(within(dialog).queryByText("当前范围暂无模型")).not.toBeInTheDocument();
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
      filters: { agents: [], models: modelSelections("claude-3"), projects: [{ kind: "projectless" }] },
      anyFilterActive: true,
      onClear,
      onChange,
    });

    fireEvent.click(screen.getByRole("button", { name: "清除筛选" }));

    expect(onClear).toHaveBeenCalledTimes(1);
    expect(onChange).not.toHaveBeenCalled();
  });

  it("always shows the terminal filter, since options follow the selected range", () => {
    for (const options of [null, { data_revision: 1, agents: [], models: [], projects: [] }, { data_revision: 1, agents: [{ agent: "codex", display_name: "Codex" }], models: [], projects: [] }]) {
      const view = renderControls({ options });
      expect(screen.getByRole("button", { name: "Agent筛选，全部" })).toBeInTheDocument();
      view.unmount();
    }
  });


  it("TD-P5-TERMINAL-01 shows the terminal filter with SquareTerminal and identical geometry to Project filter", async () => {
    const onChange = vi.fn();
    const multiSourceOptions: FilterOptionsResponse = {
      data_revision: 1,
      agents: [
        { agent: "codex", display_name: "Codex" },
        { agent: "antigravity", display_name: "Antigravity" },
      ],
      models: [],
      projects: [],
    };

    renderControls({
      options: multiSourceOptions,
      filters: { agents: [], models: [], projects: [] },
      onChange,
    });

    const trigger = screen.getByRole("button", { name: "Agent筛选，全部" });
    expect(trigger).toBeInTheDocument();
    expect(trigger).toHaveTextContent("Agent · 全部");

    const dialog = await openPopover("Agent筛选，全部");
    expect(dialog).toBeInTheDocument();
    expect(dialog).toHaveClass("w-80");
    const scrollArea = dialog.querySelector<HTMLElement>("[data-multi-select-scroll-area]");
    expect(scrollArea).toHaveStyle({ maxHeight: "624px" });

    expect(within(dialog).getByText("Codex")).toBeInTheDocument();
    expect(within(dialog).getByText("Antigravity")).toBeInTheDocument();

    const antigravityCheckbox = within(dialog).getByRole("checkbox", { name: "Antigravity" });
    fireEvent.click(antigravityCheckbox);

    expect(onChange).toHaveBeenCalledWith({
      agents: ["antigravity"],
      models: [],
      projects: [],
    });
  });

  it("TD-P5-TERMINAL-ORPHAN-01 pins a selected terminal missing from the range above the options", async () => {
    const onChange = vi.fn();
    renderControls({
      options: {
        data_revision: 1,
        agents: [{ agent: "codex", display_name: "Codex" }, { agent: "antigravity", display_name: "Antigravity" }],
        models: [],
        projects: [],
      },
      filters: { agents: ["legacy-source"], models: [], projects: [] },
      onChange,
    });

    expect(screen.getByRole("button", { name: "Agent筛选，已选1项" })).toHaveTextContent("Agent · 1 项");
    const dialog = await openPopover("Agent筛选，已选1项");
    const labels = within(dialog).getAllByRole("checkbox").map((node) => node.closest("label")?.textContent?.replace("Selected", ""));
    expect(labels).toEqual(["legacy-source", "Codex", "Antigravity"]);
    fireEvent.click(within(dialog).getByRole("checkbox", { name: "legacy-source" }));
    expect(onChange).toHaveBeenCalledWith({ agents: [], models: [], projects: [] });
  });


  it("TD-P5-MODEL-ORPHAN-01 keeps an unavailable model out of the provider groups", async () => {
    renderControls({
      options: {
        data_revision: 1,
        agents: [],
        models: [
          { model: "gpt-4o", provider_id: "openai", provider_name: "OpenAI" },
          { model: "claude-3", provider_id: "route-models", provider_name: "Route-models" },
        ],
        projects: [],
      },
      filters: { agents: [], models: modelSelections("disappeared-model"), projects: [] },
    });

    const dialog = await openPopover("模型筛选，已选1项");
    expect(within(dialog).getByRole("checkbox", { name: "disappeared-model · orphan-provider" })).toBeInTheDocument();
    ensureModelGroupExpanded(dialog, "Route-models");
    expect(within(dialog).getAllByRole("checkbox", { name: /disappeared-model/ })).toHaveLength(1);
  });


  it("TD-P5-MODEL-EXPAND-01 initially expands first active group and preserves expanded state on options change", async () => {
    const initialOptions: FilterOptionsResponse = {
      data_revision: 1,
      agents: [],
      models: [
        { model: "gpt-4o", provider_id: "openai", provider_name: "OpenAI" },
        { model: "claude-3", provider_id: "route-models", provider_name: "Route-models" },
      ],
      projects: [],
    };

    const { rerender } = render(
      <FilterControls
        filters={emptyFilters}
        options={initialOptions}
        optionsLoading={false}
        
        anyFilterActive={false}
        onChange={vi.fn()}
        onClear={vi.fn()}
        onRetryOptions={vi.fn()}
      />,
    );

    const dialog = await openPopover("模型筛选，全部");
    const openAiToggle = within(dialog).getByRole("button", { name: "OpenAI" });
    const routeModelsToggle = within(dialog).getByRole("button", { name: "Route-models" });

    // Initial: only OpenAI expanded
    expect(openAiToggle).toHaveAttribute("aria-expanded", "true");
    expect(routeModelsToggle).toHaveAttribute("aria-expanded", "false");

    // User collapses OpenAI and expands Route-models
    fireEvent.click(openAiToggle);
    fireEvent.click(routeModelsToggle);
    expect(openAiToggle).toHaveAttribute("aria-expanded", "false");
    expect(routeModelsToggle).toHaveAttribute("aria-expanded", "true");

    // Dynamic update: Antigravity model added
    const updatedOptions: FilterOptionsResponse = {
      data_revision: 2,
      agents: [],
      models: [
        { model: "gpt-4o", provider_id: "openai", provider_name: "OpenAI" },
        { model: "gemini-2.5-flash", provider_id: "antigravity", provider_name: "Antigravity" },
        { model: "claude-3", provider_id: "route-models", provider_name: "Route-models" },
      ],
      projects: [],
    };

    rerender(
      <FilterControls
        filters={{ agents: [], models: modelSelections("new-orphan"), projects: [] }}
        options={updatedOptions}
        optionsLoading={false}
        
        anyFilterActive={true}
        onChange={vi.fn()}
        onClear={vi.fn()}
        onRetryOptions={vi.fn()}
      />,
    );

    // States are preserved: OpenAI remains collapsed, Route-models remains expanded, Antigravity starts collapsed
    const dialogUpdated = screen.getByRole("dialog");
    expect(within(dialogUpdated).getByRole("button", { name: "OpenAI" })).toHaveAttribute("aria-expanded", "false");
    expect(within(dialogUpdated).getByRole("button", { name: "Route-models" })).toHaveAttribute("aria-expanded", "true");
    expect(within(dialogUpdated).getByRole("button", { name: "Antigravity" })).toHaveAttribute("aria-expanded", "false");
  });
});
