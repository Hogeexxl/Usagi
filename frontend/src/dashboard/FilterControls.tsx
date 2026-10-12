import { ChevronRight, Cpu, Folder, SquareTerminal } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";
import { forwardRef, useEffect, useMemo, useRef, useState } from "react";

import type {
  DashboardFilters,
  FilterOptionsResponse,
  ModelFilterOption,
  ProjectFilterOption,
  ProjectSelection,
  AgentFilterOption,
} from "../data/types";
import { modelSelectionKey } from "../data/usagiClient";
import { Button, type ButtonProps } from "../ui/beui/button";
import { Checkbox } from "../ui/beui/checkbox";
import { MorphPopover, MorphPopoverContent, MorphPopoverTrigger } from "../ui/beui/morph-popover";
import { SPRING_LAYOUT } from "../ui/lib/ease";
import { projectDisplay, projectKey, projectTitle, type ProjectLike } from "./shared/projectDisplay";

type FilterControlsProps = {
  filters: DashboardFilters;
  options: FilterOptionsResponse | null;
  optionsLoading: boolean;
  optionsStale: boolean;
  optionsErrorCode?: string;
  anyFilterActive: boolean;
  onChange: (filters: DashboardFilters) => void;
  onClear: () => void;
  onRetryOptions: () => void;
};

type ModelGroup = {
  key: string;
  label: string;
  models: ModelFilterOption[];
};

function modelGroups(options: FilterOptionsResponse | null, selectedModels: DashboardFilters["models"]): ModelGroup[] {
  const optionModels = options?.models ?? [];
  const knownModels = new Set(optionModels.map(modelSelectionKey));
  const groups = new Map<string, ModelGroup>();
  for (const option of optionModels) {
    const group = groups.get(option.provider_id) ?? {
      key: option.provider_id,
      label: option.provider_name,
      models: [],
    };
    group.models.push(option);
    groups.set(option.provider_id, group);
  }
  const result = [...groups.values()];
  for (const group of result) group.models.sort((left, right) => left.model.localeCompare(right.model, "en", { numeric: true }));
  const selectedOrphans = selectedModels.filter((model) => !knownModels.has(modelSelectionKey(model)));
  if (selectedOrphans.length > 0) {
    result.push({
      key: "selected",
      label: "已选",
      models: selectedOrphans.map((model) => ({ ...model, provider_name: model.provider_id })),
    });
  }
  return result;
}

function agentSelections(options: FilterOptionsResponse | null): AgentFilterOption[] {
  return [...(options?.agents ?? [])];
}

function projectSelections(options: FilterOptionsResponse | null, selected: readonly ProjectSelection[]): ProjectLike[] {
  const values: ProjectLike[] = [...(options?.projects ?? [])];
  const present = new Set(values.map(projectKey));
  for (const selection of selected) if (!present.has(projectKey(selection))) values.push(selection);
  return values;
}

function OptionStatus({ loading, stale, error, hasOptions, onRetry }: { loading: boolean; stale: boolean; error?: string; hasOptions: boolean; onRetry: () => void }) {
  if (loading) return <div className="px-2 py-2 text-xs text-muted-foreground" role="status">选项加载中…</div>;
  if (error) return <div className="flex items-center justify-between gap-3 px-2 py-2 text-xs text-destructive" role="status"><span>选项加载失败</span><Button variant="ghost" size="sm" onClick={onRetry}>重试</Button></div>;
  if (stale) return <div className="flex items-center justify-between gap-3 px-2 py-2 text-xs text-warning" role="status"><span>{hasOptions ? "选项可能已更新" : "选项需要刷新"}</span><Button variant="ghost" size="sm" onClick={onRetry}>重试</Button></div>;
  return null;
}

type FilterTriggerProps = Omit<ButtonProps, "children"> & { label: string; count: number; icon: React.ReactNode };

const FilterTrigger = forwardRef<HTMLButtonElement, FilterTriggerProps>(function FilterTrigger({ label, count, icon, ...buttonProps }, ref) {
  const active = count > 0;
  return <Button ref={ref} {...buttonProps} variant={active ? "primary" : "secondary"} size="sm" aria-label={`${label}筛选${active ? `，已选${count}项` : "，全部"}`}><span className="inline-flex h-4 w-4 items-center justify-center">{icon}</span><span>{label} · {active ? `${count} 项` : "全部"}</span></Button>;
});

export function FilterControls({ filters, options, optionsLoading, optionsStale, optionsErrorCode, anyFilterActive, onChange, onClear, onRetryOptions }: FilterControlsProps) {
  const [expandedGroups, setExpandedGroups] = useState<Record<string, boolean>>({});
  const modelGroupsInitialized = useRef(false);
  const reduce = useReducedMotion();
  const showAgentFilter = (options?.agents?.length ?? 0) > 1;
  const agents = useMemo(() => agentSelections(options), [options]);
  const groups = useMemo(() => modelGroups(options, filters.models), [options, filters.models]);
  const projects = useMemo(() => projectSelections(options, filters.projects), [options, filters.projects]);

  useEffect(() => {
    if (modelGroupsInitialized.current || !options) return;
    const firstActive = groups.find((group) => group.key !== "selected");
    if (!firstActive) return;
    modelGroupsInitialized.current = true;
    setExpandedGroups((current) => ({ ...current, [firstActive.key]: true }));
  }, [groups, options]);

  const selectedAgents = new Set(filters.agents);
  const selectedModels = new Set(filters.models.map(modelSelectionKey));
  const selectedProjects = new Set(filters.projects.map(projectKey));

  const updateAgents = (nextAgents: string[]) => onChange({ ...filters, agents: nextAgents });
  const toggleAgent = (agent: string) =>
    updateAgents(
      selectedAgents.has(agent)
        ? filters.agents.filter((value) => value !== agent)
        : [...filters.agents, agent],
    );
  const toggleModel = (model: ModelFilterOption) => {
    const key = modelSelectionKey(model);
    onChange({
      ...filters,
      models: selectedModels.has(key)
        ? filters.models.filter((selection) => modelSelectionKey(selection) !== key)
        : [...filters.models, { provider_id: model.provider_id, model: model.model }],
    });
  };
  const toggleGroup = (groupModels: readonly ModelFilterOption[]) => {
    const groupKeys = new Set(groupModels.map(modelSelectionKey));
    const allSelected = groupModels.every((model) => selectedModels.has(modelSelectionKey(model)));
    onChange({
      ...filters,
      models: allSelected
        ? filters.models.filter((model) => !groupKeys.has(modelSelectionKey(model)))
        : [...filters.models, ...groupModels
            .filter((model) => !selectedModels.has(modelSelectionKey(model)))
            .map(({ provider_id, model }) => ({ provider_id, model }))],
    });
  };
  const toggleProject = (project: ProjectFilterOption | ProjectSelection) => {
    const key = projectKey(project);
    const addition: ProjectSelection = project.kind === "project" ? { kind: "project", path: project.path } : { kind: project.kind };
    onChange({ ...filters, projects: selectedProjects.has(key) ? filters.projects.filter((selection) => projectKey(selection) !== key) : [...filters.projects, addition] });
  };

  const rowClass = "flex min-h-9 w-full min-w-0 items-center gap-2 px-2 py-1.5";

  return (
    <div className="flex flex-wrap items-center gap-2">
      {showAgentFilter ? (
        <MorphPopover>
          <MorphPopoverTrigger>
            <FilterTrigger label="终端" count={filters.agents.length} icon={<SquareTerminal className="h-4 w-4" />} />
          </MorphPopoverTrigger>
          <MorphPopoverContent side="bottom" align="start" maxHeight={624} className="w-80">
            <div className="p-2">
            <OptionStatus
              loading={optionsLoading}
              stale={optionsStale}
              error={optionsErrorCode}
              hasOptions={agents.length > 0}
              onRetry={onRetryOptions}
            />
            {!optionsLoading && agents.length === 0 ? (
              <div className="px-2 py-3 text-xs text-muted-foreground">暂无终端</div>
            ) : null}
            {agents.map((item) => (
              <div key={item.agent} className={rowClass}>
                <Checkbox
                  checked={selectedAgents.has(item.agent)}
                  onCheckedChange={() => toggleAgent(item.agent)}
                  label={item.display_name}
                />
              </div>
            ))}
            </div>
          </MorphPopoverContent>
        </MorphPopover>
      ) : null}
      <MorphPopover>
        <MorphPopoverTrigger><FilterTrigger label="模型" count={filters.models.length} icon={<Cpu className="h-4 w-4" />} /></MorphPopoverTrigger>
        <MorphPopoverContent side="bottom" align="start" maxHeight={624} className="w-72">
          <div className="p-2">
          <OptionStatus loading={optionsLoading} stale={optionsStale} error={optionsErrorCode} hasOptions={groups.length > 0} onRetry={onRetryOptions} />
          {!optionsLoading && groups.length === 0 ? <div className="px-2 py-3 text-xs text-muted-foreground">暂无模型</div> : null}
          {groups.map((group) => {
            const expanded = Boolean(expandedGroups[group.key]);
            const allSelected = group.models.every((model) => selectedModels.has(modelSelectionKey(model)));
            const someSelected = group.models.some((model) => selectedModels.has(modelSelectionKey(model)));
            return <div key={group.key}>
              <div className={rowClass}>
                <Checkbox checked={allSelected} indeterminate={someSelected && !allSelected} aria-label={group.label} onCheckedChange={() => toggleGroup(group.models)} />
                <button type="button" className="flex min-w-0 flex-1 items-center gap-1.5 text-left" aria-expanded={expanded} onClick={() => setExpandedGroups((current) => ({ ...current, [group.key]: !current[group.key] }))}><span className="flex-1">{group.label}</span><motion.span animate={{ rotate: expanded ? 90 : 0 }} transition={reduce ? { duration: 0 } : SPRING_LAYOUT}><ChevronRight className="h-4 w-4 text-muted-foreground" /></motion.span></button>
              </div>
              {expanded ? <div className="pl-5">{group.models.map((model) => <div key={modelSelectionKey(model)} className={rowClass}><Checkbox checked={selectedModels.has(modelSelectionKey(model))} onCheckedChange={() => toggleModel(model)} label={model.model} /></div>)}</div> : null}
            </div>;
          })}
          </div>
        </MorphPopoverContent>
      </MorphPopover>

      <MorphPopover>
        <MorphPopoverTrigger><FilterTrigger label="项目" count={filters.projects.length} icon={<Folder className="h-4 w-4" />} /></MorphPopoverTrigger>
        <MorphPopoverContent side="bottom" align="start" maxHeight={624} className="w-80">
          <div className="p-2">
          <OptionStatus loading={optionsLoading} stale={optionsStale} error={optionsErrorCode} hasOptions={projects.length > 0} onRetry={onRetryOptions} />
          {!optionsLoading && projects.length === 0 ? <div className="px-2 py-3 text-xs text-muted-foreground">暂无项目</div> : null}
          {projects.map((project) => <div key={projectKey(project)} className={rowClass} title={projectTitle(project)}><Checkbox checked={selectedProjects.has(projectKey(project))} onCheckedChange={() => toggleProject(project)} label={projectDisplay(project)} /></div>)}
          </div>
        </MorphPopoverContent>
      </MorphPopover>

      {anyFilterActive ? <Button variant="ghost" size="sm" onClick={onClear}>清除筛选</Button> : null}
    </div>
  );
}
