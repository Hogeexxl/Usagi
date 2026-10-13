import { ChevronRight, Cpu, Folder, SquareTerminal } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";
import { forwardRef, useEffect, useId, useMemo, useRef, useState } from "react";

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
import { cn } from "../ui/lib/cn";
import { SPRING_LAYOUT } from "../ui/lib/ease";
import { projectDisplay, projectKey, projectTitle, type ProjectLike } from "./shared/projectDisplay";

type FilterControlsProps = {
  filters: DashboardFilters;
  options: FilterOptionsResponse | null;
  optionsLoading: boolean;
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

// Options only cover the current range. Selections the range has no data for
// stay listed (and selected) so they can be seen and cleared.
type FilterMenuItems<T> = { available: T[]; unavailable: T[] };

function splitSelected<T, S>(options: readonly T[], selected: readonly S[], optionKey: (value: T | S) => string, fallback: (selection: S) => T): FilterMenuItems<T> {
  const present = new Set(options.map(optionKey));
  return {
    available: [...options],
    unavailable: selected.filter((selection) => !present.has(optionKey(selection))).map(fallback),
  };
}

function modelGroups(models: readonly ModelFilterOption[]): ModelGroup[] {
  const groups = new Map<string, ModelGroup>();
  for (const option of models) {
    const group = groups.get(option.provider_id) ?? { key: option.provider_id, label: option.provider_name, models: [] };
    group.models.push(option);
    groups.set(option.provider_id, group);
  }
  const result = [...groups.values()];
  for (const group of result) group.models.sort((left, right) => left.model.localeCompare(right.model, "en", { numeric: true }));
  return result;
}

// Remembers labels of options seen in earlier ranges, for selections the
// current range no longer lists.
function useSeenLabels(options: FilterOptionsResponse | null) {
  const seen = useRef({
    agents: new Map<string, AgentFilterOption>(),
    models: new Map<string, ModelFilterOption>(),
    projects: new Map<string, ProjectFilterOption>(),
  });
  if (options) {
    for (const agent of options.agents) seen.current.agents.set(agent.agent, agent);
    for (const model of options.models) seen.current.models.set(modelSelectionKey(model), model);
    for (const project of options.projects) seen.current.projects.set(projectKey(project), project);
  }
  return seen.current;
}

function OptionStatus({ loading, error, onRetry }: { loading: boolean; error?: string; onRetry: () => void }) {
  if (loading) return <div className="px-2 py-2 text-xs text-muted-foreground" role="status">选项加载中…</div>;
  if (error) return <div className="flex items-center justify-between gap-3 px-2 py-2 text-xs text-destructive" role="status"><span>选项加载失败</span><Button variant="ghost" size="sm" onClick={onRetry}>重试</Button></div>;
  return null;
}

// Until the range's options arrive it is unknown whether a selection has data,
// so it is listed as a plain selected row.
function UnavailableRow({ label, title, known, onUncheck }: { label: string; title?: string; known: boolean; onUncheck: () => void }) {
  const hintId = useId();
  return <div className={cn("flex min-h-9 w-full min-w-0 items-center gap-2 px-2 py-1.5", known && "text-muted-foreground")} title={title}>
    <Checkbox checked onCheckedChange={onUncheck} label={label} aria-describedby={known ? hintId : undefined} />
    {known ? <span id={hintId} className="ml-auto shrink-0 text-xs">当前范围无数据</span> : null}
  </div>;
}

type FilterTriggerProps = Omit<ButtonProps, "children"> & { label: string; count: number; icon: React.ReactNode };

const FilterTrigger = forwardRef<HTMLButtonElement, FilterTriggerProps>(function FilterTrigger({ label, count, icon, ...buttonProps }, ref) {
  const active = count > 0;
  return <Button ref={ref} {...buttonProps} variant={active ? "primary" : "secondary"} size="sm" aria-label={`${label}筛选${active ? `，已选${count}项` : "，全部"}`}><span className="inline-flex h-4 w-4 items-center justify-center">{icon}</span><span>{label} · {active ? `${count} 项` : "全部"}</span></Button>;
});

export function FilterControls({ filters, options, optionsLoading, optionsErrorCode, anyFilterActive, onChange, onClear, onRetryOptions }: FilterControlsProps) {
  const [expandedGroups, setExpandedGroups] = useState<Record<string, boolean>>({});
  const modelGroupsInitialized = useRef(false);
  const reduce = useReducedMotion();
  const seen = useSeenLabels(options);
  const agents = useMemo(() => splitSelected<AgentFilterOption, string>(
    options?.agents ?? [], filters.agents,
    (value) => typeof value === "string" ? value : value.agent,
    (agent) => seen.agents.get(agent) ?? { agent, display_name: agent },
  ), [options, filters.agents, seen]);
  const models = useMemo(() => splitSelected<ModelFilterOption, DashboardFilters["models"][number]>(
    options?.models ?? [], filters.models, modelSelectionKey,
    (model) => seen.models.get(modelSelectionKey(model)) ?? { ...model, provider_name: model.provider_id },
  ), [options, filters.models, seen]);
  const groups = useMemo(() => modelGroups(models.available), [models]);
  const projects = useMemo(() => splitSelected<ProjectLike, ProjectSelection>(
    options?.projects ?? [], filters.projects, projectKey,
    (project) => seen.projects.get(projectKey(project)) ?? project,
  ), [options, filters.projects, seen]);

  useEffect(() => {
    if (modelGroupsInitialized.current || groups.length === 0) return;
    modelGroupsInitialized.current = true;
    setExpandedGroups((current) => ({ ...current, [groups[0].key]: true }));
  }, [groups]);

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
  const toggleProject = (project: ProjectLike) => {
    const key = projectKey(project);
    const addition: ProjectSelection = project.kind === "project" ? { kind: "project", path: project.path } : { kind: project.kind };
    onChange({ ...filters, projects: selectedProjects.has(key) ? filters.projects.filter((selection) => projectKey(selection) !== key) : [...filters.projects, addition] });
  };

  const rowClass = "flex min-h-9 w-full min-w-0 items-center gap-2 px-2 py-1.5";

  return (
    <div className="flex flex-wrap items-center gap-2">
      <MorphPopover>
          <MorphPopoverTrigger>
            <FilterTrigger label="Agent" count={filters.agents.length} icon={<SquareTerminal className="h-4 w-4" />} />
          </MorphPopoverTrigger>
          <MorphPopoverContent side="bottom" align="start" maxHeight={624} className="w-80">
            <div className="p-2">
            <OptionStatus loading={optionsLoading} error={optionsErrorCode} onRetry={onRetryOptions} />
            {agents.unavailable.map((item) => <UnavailableRow known={options !== null} key={item.agent} label={item.display_name} onUncheck={() => toggleAgent(item.agent)} />)}
            {!optionsLoading && !optionsErrorCode && agents.available.length === 0 ? (
              <div className="px-2 py-3 text-xs text-muted-foreground">当前范围暂无 Agent</div>
            ) : null}
            {agents.available.map((item) => (
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
      <MorphPopover>
        <MorphPopoverTrigger><FilterTrigger label="模型" count={filters.models.length} icon={<Cpu className="h-4 w-4" />} /></MorphPopoverTrigger>
        <MorphPopoverContent side="bottom" align="start" maxHeight={624} className="w-72">
          <div className="p-2">
          <OptionStatus loading={optionsLoading} error={optionsErrorCode} onRetry={onRetryOptions} />
          {models.unavailable.map((model) => <UnavailableRow known={options !== null} key={modelSelectionKey(model)} label={`${model.model} · ${model.provider_name}`} onUncheck={() => toggleModel(model)} />)}
          {!optionsLoading && !optionsErrorCode && groups.length === 0 ? <div className="px-2 py-3 text-xs text-muted-foreground">当前范围暂无模型</div> : null}
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
          <OptionStatus loading={optionsLoading} error={optionsErrorCode} onRetry={onRetryOptions} />
          {projects.unavailable.map((project) => <UnavailableRow known={options !== null} key={projectKey(project)} label={projectDisplay(project)} title={projectTitle(project)} onUncheck={() => toggleProject(project)} />)}
          {!optionsLoading && !optionsErrorCode && projects.available.length === 0 ? <div className="px-2 py-3 text-xs text-muted-foreground">当前范围暂无项目</div> : null}
          {projects.available.map((project) => <div key={projectKey(project)} className={rowClass} title={projectTitle(project)}><Checkbox checked={selectedProjects.has(projectKey(project))} onCheckedChange={() => toggleProject(project)} label={projectDisplay(project)} /></div>)}
          </div>
        </MorphPopoverContent>
      </MorphPopover>

      {anyFilterActive ? <Button variant="ghost" size="sm" onClick={onClear}>清除筛选</Button> : null}
    </div>
  );
}
