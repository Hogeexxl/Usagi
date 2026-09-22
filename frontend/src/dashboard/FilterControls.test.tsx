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

function ensureModelGroupExpanded(dialog: HTMLElement, name: string) {
  const toggle = within(dialog).getByRole("button", { name });
  if (toggle.getAttribute("aria-expanded") !== "true") fireEvent.click(toggle);
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
    ensureModelGroupExpanded(dialog, "Route-models");
    const checkbox = within(dialog).getByRole("checkbox", { name: "claude-3" });

    fireEvent.click(checkbox);

    expect(onChange).toHaveBeenCalledWith({ sources: [], models: ["claude-3"], projects: [] });
  });

  it("toggles an ordinary model when its label text is clicked", async () => {
    const onChange = vi.fn();
    renderControls({ onChange });
    const dialog = await openPopover("模型筛选，全部");
    ensureModelGroupExpanded(dialog, "Route-models");

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

  it("keeps a selected orphan model cancellable in the Selected fallback group", async () => {
    const onChange = vi.fn();
    renderControls({
      filters: { sources: [], models: ["orphan-rollout"], projects: [] },
      onChange,
    });
    const dialog = await openPopover("模型筛选，已选1项");
    ensureModelGroupExpanded(dialog, "Selected");
    const orphan = within(dialog).getByRole("checkbox", { name: "orphan-rollout" });

    expect(within(dialog).getByRole("button", { name: "Selected" })).toBeInTheDocument();
    fireEvent.click(orphan);

    expect(onChange).toHaveBeenCalledWith({ sources: [], models: [], projects: [] });
  });

  it("does not auto-expand Selected when no active model group exists", async () => {
    const { rerender } = render(
      <FilterControls
        filters={{ sources: [], models: ["orphan-rollout"], projects: [] }}
        options={{ data_revision: 1, sources: [], models: [], projects: [] }}
        optionsLoading={false}
        optionsStale={false}
        anyFilterActive={true}
        onChange={vi.fn()}
        onClear={vi.fn()}
        onRetryOptions={vi.fn()}
      />,
    );

    const dialog = await openPopover("模型筛选，已选1项");
    const selectedToggle = within(dialog).getByRole("button", { name: "Selected" });
    expect(selectedToggle).toHaveAttribute("aria-expanded", "false");
    expect(within(dialog).queryByRole("checkbox", { name: "orphan-rollout" })).not.toBeInTheDocument();

    fireEvent.click(selectedToggle);
    expect(selectedToggle).toHaveAttribute("aria-expanded", "true");
    rerender(
      <FilterControls
        filters={{ sources: [], models: ["orphan-rollout"], projects: [] }}
        options={{
          data_revision: 2,
          sources: [],
          models: [{ model: "gpt-4o", provider: "openai" }],
          projects: [],
        }}
        optionsLoading={false}
        optionsStale={false}
        anyFilterActive={true}
        onChange={vi.fn()}
        onClear={vi.fn()}
        onRetryOptions={vi.fn()}
      />,
    );
    const updatedDialog = screen.getByRole("dialog");
    expect(within(updatedDialog).getByRole("button", { name: "Selected" })).toHaveAttribute("aria-expanded", "true");
    expect(within(updatedDialog).getByRole("button", { name: "OpenAI" })).toHaveAttribute("aria-expanded", "false");
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
      filters: { sources: [], models: [], projects: [{ kind: "project", project_path: "/workspace" }] },
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
    expect(panels).toHaveLength(2);

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

  it("hides the terminal filter button when options are null, empty, or have only 1 source", () => {
    // null options
    const firstRender = renderControls({ options: null });
    expect(screen.queryByRole("button", { name: /终端/ })).not.toBeInTheDocument();
    firstRender.unmount();

    // 0 sources
    const secondRender = renderControls({ options: { data_revision: 1, sources: [], models: [], projects: [] } });
    expect(screen.queryByRole("button", { name: /终端/ })).not.toBeInTheDocument();
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
    expect(screen.queryByRole("button", { name: /终端/ })).not.toBeInTheDocument();
    thirdRender.unmount();
  });

  it("TD-P5-TERMINAL-01 shows the terminal filter with SquareTerminal and identical geometry to Project filter", async () => {
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

    const trigger = screen.getByRole("button", { name: "终端筛选，全部" });
    expect(trigger).toBeInTheDocument();
    expect(trigger).toHaveTextContent("终端 · 全部");

    const dialog = await openPopover("终端筛选，全部");
    expect(dialog).toBeInTheDocument();
    expect(dialog).toHaveClass("w-80");
    const scrollArea = dialog.querySelector<HTMLElement>("[data-multi-select-scroll-area]");
    expect(scrollArea).toHaveStyle({ maxHeight: "624px" });

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

  it("TD-P5-TERMINAL-ORPHAN-01 does not append raw orphan source to menu and retains filter state", async () => {
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
      filters: { sources: ["legacy-source"], models: [], projects: [] },
      onChange,
    });

    const trigger = screen.getByRole("button", { name: "终端筛选，已选1项" });
    expect(trigger).toHaveTextContent("终端 · 1 项");

    const dialog = await openPopover("终端筛选，已选1项");
    expect(within(dialog).queryByRole("checkbox", { name: "legacy-source" })).not.toBeInTheDocument();
    expect(within(dialog).queryByText("legacy-source")).not.toBeInTheDocument();
    expect(within(dialog).getByText("Codex")).toBeInTheDocument();
    expect(within(dialog).getByText("Antigravity")).toBeInTheDocument();
  });

  it("TD-P5-MODEL-ORPHAN-01 puts orphan model into Selected group at the end, not Route-models", async () => {
    const multiOptions: FilterOptionsResponse = {
      data_revision: 1,
      sources: [],
      models: [
        { model: "gpt-4o", provider: "openai" },
        { model: "gemini-2.5-pro", provider: "antigravity" },
        { model: "claude-3", provider: "route-models" },
      ],
      projects: [],
    };

    renderControls({
      options: multiOptions,
      filters: { sources: [], models: ["disappeared-antigravity-model"], projects: [] },
    });

    const dialog = await openPopover("模型筛选，已选1项");
    const groupButtons = within(dialog).getAllByRole("button", { name: /OpenAI|Antigravity|Route-models|Selected/ });
    const labels = groupButtons.map((btn) => btn.textContent?.replace(/[^\w-]/g, "") || "");
    expect(labels).toEqual(["OpenAI", "Antigravity", "Route-models", "Selected"]);

    ensureModelGroupExpanded(dialog, "Selected");
    expect(within(dialog).getByRole("checkbox", { name: "disappeared-antigravity-model" })).toBeInTheDocument();

    // Collapse Selected group, expand Route-models, and verify orphan is not in Route-models
    fireEvent.click(within(dialog).getByRole("button", { name: "Selected" }));
    ensureModelGroupExpanded(dialog, "Route-models");
    expect(within(dialog).queryByRole("checkbox", { name: "disappeared-antigravity-model" })).not.toBeInTheDocument();
  });

  it("TD-P5-MODEL-EXPAND-01 initially expands first active group and preserves expanded state on options change", async () => {
    const initialOptions: FilterOptionsResponse = {
      data_revision: 1,
      sources: [],
      models: [
        { model: "gpt-4o", provider: "openai" },
        { model: "claude-3", provider: "route-models" },
      ],
      projects: [],
    };

    const { rerender } = render(
      <FilterControls
        filters={emptyFilters}
        options={initialOptions}
        optionsLoading={false}
        optionsStale={false}
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

    // Dynamic update: Antigravity model added and orphan selected model added
    const updatedOptions: FilterOptionsResponse = {
      data_revision: 2,
      sources: [],
      models: [
        { model: "gpt-4o", provider: "openai" },
        { model: "gemini-2.5-flash", provider: "antigravity" },
        { model: "claude-3", provider: "route-models" },
      ],
      projects: [],
    };

    rerender(
      <FilterControls
        filters={{ sources: [], models: ["new-orphan"], projects: [] }}
        options={updatedOptions}
        optionsLoading={false}
        optionsStale={false}
        anyFilterActive={true}
        onChange={vi.fn()}
        onClear={vi.fn()}
        onRetryOptions={vi.fn()}
      />,
    );

    // States are preserved: OpenAI remains collapsed, Route-models remains expanded, Antigravity & Selected start collapsed
    const dialogUpdated = screen.getByRole("dialog");
    expect(within(dialogUpdated).getByRole("button", { name: "OpenAI" })).toHaveAttribute("aria-expanded", "false");
    expect(within(dialogUpdated).getByRole("button", { name: "Route-models" })).toHaveAttribute("aria-expanded", "true");
    expect(within(dialogUpdated).getByRole("button", { name: "Antigravity" })).toHaveAttribute("aria-expanded", "false");
    expect(within(dialogUpdated).getByRole("button", { name: "Selected" })).toHaveAttribute("aria-expanded", "false");
  });
});
