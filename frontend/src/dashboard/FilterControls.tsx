import { ChevronRight, Cpu, Folder, Layers } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";
import { forwardRef, useMemo, useState } from "react";

import type {
  DashboardFilters,
  FilterOptionsResponse,
  ModelFilterProvider,
  ProjectFilterOption,
  ProjectSelection,
  SourceFilterOption,
} from "../data/types";
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
  provider: ModelFilterProvider;
  label: "OpenAI" | "Route-models";
  models: string[];
};

const MODEL_GROUPS: readonly { provider: ModelFilterProvider; label: ModelGroup["label"] }[] = [
  { provider: "openai", label: "OpenAI" },
  { provider: "route-models", label: "Route-models" },
];

function modelGroups(options: FilterOptionsResponse | null, selected: readonly string[]): ModelGroup[] {
  const optionModels = options?.models ?? [];
  const knownModels = new Set(optionModels.map(({ model }) => model));
  const modelsByProvider = new Map<ModelFilterProvider, Set<string>>();
  for (const { model, provider } of optionModels) {
    const models = modelsByProvider.get(provider) ?? new Set<string>();
    models.add(model);
    modelsByProvider.set(provider, models);
  }
  const selectedOrphans = selected.filter((model) => !knownModels.has(model));
  if (selectedOrphans.length > 0) {
    const routeModels = modelsByProvider.get("route-models") ?? new Set<string>();
    for (const model of selectedOrphans) routeModels.add(model);
    modelsByProvider.set("route-models", routeModels);
  }
  return MODEL_GROUPS.flatMap(({ provider, label }) => {
    const models = modelsByProvider.get(provider);
    if (!models || models.size === 0) return [];
    return [{ provider, label, models: [...models].sort((left, right) => left.localeCompare(right)) }];
  });
}

function sourceSelections(options: FilterOptionsResponse | null, selected: readonly string[]): SourceFilterOption[] {
  const values: SourceFilterOption[] = [...(options?.sources ?? [])];
  const present = new Set(values.map((item) => item.source));
  for (const source of selected) {
    if (!present.has(source)) {
      values.push({ source, display_name: source });
      present.add(source);
    }
  }
  return values;
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
  const [expandedGroups, setExpandedGroups] = useState<Record<ModelFilterProvider, boolean>>({
    openai: true,
    "route-models": true,
  });
  const reduce = useReducedMotion();
  const showSourceFilter = (options?.sources?.length ?? 0) > 1;
  const sources = useMemo(() => sourceSelections(options, filters.sources), [options, filters.sources]);
  const groups = useMemo(() => modelGroups(options, filters.models), [options, filters.models]);
  const projects = useMemo(() => projectSelections(options, filters.projects), [options, filters.projects]);
  const selectedSources = new Set(filters.sources);
  const selectedModels = new Set(filters.models);
  const selectedProjects = new Set(filters.projects.map(projectKey));

  const updateSources = (nextSources: string[]) => onChange({ ...filters, sources: nextSources });
  const toggleSource = (source: string) =>
    updateSources(
      selectedSources.has(source)
        ? filters.sources.filter((value) => value !== source)
        : [...filters.sources, source],
    );
  const updateModels = (nextModels: string[]) => onChange({ ...filters, models: nextModels });
  const toggleModel = (model: string) => updateModels(selectedModels.has(model) ? filters.models.filter((value) => value !== model) : [...filters.models, model]);
  const toggleGroup = (groupModels: readonly string[]) => {
    const allSelected = groupModels.every((model) => selectedModels.has(model));
    updateModels(allSelected ? filters.models.filter((model) => !groupModels.includes(model)) : [...new Set([...filters.models, ...groupModels])]);
  };
  const toggleProject = (project: ProjectFilterOption | ProjectSelection) => {
    const key = projectKey(project);
    const addition: ProjectSelection = project.kind === "project" ? { kind: "project", project_path: project.project_path } : { kind: project.kind };
    onChange({ ...filters, projects: selectedProjects.has(key) ? filters.projects.filter((selection) => projectKey(selection) !== key) : [...filters.projects, addition] });
  };

  const rowClass = "flex min-h-9 w-full min-w-0 items-center gap-2 px-2 py-1.5";

  return (
    <div className="flex flex-wrap items-center gap-2">
      {showSourceFilter ? (
        <MorphPopover>
          <MorphPopoverTrigger>
            <FilterTrigger label="来源" count={filters.sources.length} icon={<Layers className="h-4 w-4" />} />
          </MorphPopoverTrigger>
          <MorphPopoverContent side="bottom" align="start" animated={false} className="w-64 p-2">
            <OptionStatus
              loading={optionsLoading}
              stale={optionsStale}
              error={optionsErrorCode}
              hasOptions={sources.length > 0}
              onRetry={onRetryOptions}
            />
            {!optionsLoading && sources.length === 0 ? (
              <div className="px-2 py-3 text-xs text-muted-foreground">暂无来源</div>
            ) : null}
            {sources.map((item) => (
              <div key={item.source} className={rowClass}>
                <Checkbox
                  checked={selectedSources.has(item.source)}
                  onCheckedChange={() => toggleSource(item.source)}
                  label={item.display_name}
                />
              </div>
            ))}
          </MorphPopoverContent>
        </MorphPopover>
      ) : null}
      <MorphPopover>
        <MorphPopoverTrigger><FilterTrigger label="模型" count={filters.models.length} icon={<Cpu className="h-4 w-4" />} /></MorphPopoverTrigger>
        <MorphPopoverContent side="bottom" align="start" animated={false} className="w-72 p-2">
          <OptionStatus loading={optionsLoading} stale={optionsStale} error={optionsErrorCode} hasOptions={groups.length > 0} onRetry={onRetryOptions} />
          {!optionsLoading && groups.length === 0 ? <div className="px-2 py-3 text-xs text-muted-foreground">暂无模型</div> : null}
          {groups.map((group) => {
            const expanded = expandedGroups[group.provider];
            const allSelected = group.models.every((model) => selectedModels.has(model));
            const someSelected = group.models.some((model) => selectedModels.has(model));
            return <div key={group.provider}>
              <div className={rowClass}>
                <Checkbox checked={allSelected} indeterminate={someSelected && !allSelected} aria-label={group.label} onCheckedChange={() => toggleGroup(group.models)} />
                <button type="button" className="flex min-w-0 flex-1 items-center gap-1.5 text-left" aria-expanded={expanded} onClick={() => setExpandedGroups((current) => ({ ...current, [group.provider]: !current[group.provider] }))}><span className="flex-1">{group.label}</span><motion.span animate={{ rotate: expanded ? 90 : 0 }} transition={reduce ? { duration: 0 } : SPRING_LAYOUT}><ChevronRight className="h-4 w-4 text-muted-foreground" /></motion.span></button>
              </div>
              {expanded ? <div className="pl-5">{group.models.map((model) => <div key={model} className={rowClass}><Checkbox checked={selectedModels.has(model)} onCheckedChange={() => toggleModel(model)} label={model} /></div>)}</div> : null}
            </div>;
          })}
        </MorphPopoverContent>
      </MorphPopover>

      <MorphPopover>
        <MorphPopoverTrigger><FilterTrigger label="项目" count={filters.projects.length} icon={<Folder className="h-4 w-4" />} /></MorphPopoverTrigger>
        <MorphPopoverContent side="bottom" align="start" animated={false} className="w-80 p-2">
          <OptionStatus loading={optionsLoading} stale={optionsStale} error={optionsErrorCode} hasOptions={projects.length > 0} onRetry={onRetryOptions} />
          {!optionsLoading && projects.length === 0 ? <div className="px-2 py-3 text-xs text-muted-foreground">暂无项目</div> : null}
          {projects.map((project) => <div key={projectKey(project)} className={rowClass} title={projectTitle(project)}><Checkbox checked={selectedProjects.has(projectKey(project))} onCheckedChange={() => toggleProject(project)} label={projectDisplay(project)} /></div>)}
        </MorphPopoverContent>
      </MorphPopover>

      {anyFilterActive ? <Button variant="ghost" size="sm" onClick={onClear}>清除筛选</Button> : null}
    </div>
  );
}
