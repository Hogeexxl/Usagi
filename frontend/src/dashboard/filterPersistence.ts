import type {
  AgentFilterOption,
  DashboardFilters,
  FilterOptionsResponse,
  ModelFilterOption,
  ProjectFilterOption,
} from "../data/types";
import { canonicalDashboardFilters, modelSelectionKey } from "../data/usagiClient";
import { projectKey } from "./shared/projectDisplay";

export const DASHBOARD_FILTERS_STORAGE_KEY = "usagi.dashboard.filters.v1";

export type PersistedFilterOptions = {
  agents: AgentFilterOption[];
  models: ModelFilterOption[];
  projects: ProjectFilterOption[];
};

export type PersistedFilterState = {
  filters: DashboardFilters;
  options: PersistedFilterOptions;
};

export const EMPTY_DASHBOARD_FILTERS: DashboardFilters = {
  agents: [],
  models: [],
  projects: [],
};

const EMPTY_FILTER_OPTIONS: PersistedFilterOptions = {
  agents: [],
  models: [],
  projects: [],
};

function emptyState(): PersistedFilterState {
  return {
    filters: { ...EMPTY_DASHBOARD_FILTERS },
    options: { ...EMPTY_FILTER_OPTIONS },
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function parseAgents(value: unknown): AgentFilterOption[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((item) =>
    isRecord(item) && typeof item.agent === "string" && typeof item.display_name === "string"
      ? [{ agent: item.agent, display_name: item.display_name }]
      : [],
  );
}

function parseModels(value: unknown): ModelFilterOption[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((item) =>
    isRecord(item) &&
    typeof item.model === "string" &&
    typeof item.provider_id === "string" &&
    typeof item.provider_name === "string"
      ? [{ model: item.model, provider_id: item.provider_id, provider_name: item.provider_name }]
      : [],
  );
}

function parseProject(value: unknown): ProjectFilterOption | null {
  if (!isRecord(value)) return null;
  if (value.kind === "project" && typeof value.path === "string" && typeof value.name === "string") {
    return { kind: "project", path: value.path, name: value.name };
  }
  if (value.kind === "workspace" || value.kind === "projectless" || value.kind === "unknown") {
    return { kind: value.kind };
  }
  return null;
}

function parseProjects(value: unknown): ProjectFilterOption[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((item) => {
    const project = parseProject(item);
    return project ? [project] : [];
  });
}

function parseOptions(value: unknown): PersistedFilterOptions {
  if (!isRecord(value)) return { ...EMPTY_FILTER_OPTIONS };
  return {
    agents: parseAgents(value.agents),
    models: parseModels(value.models),
    projects: parseProjects(value.projects),
  };
}

function parseFilters(value: unknown): DashboardFilters | null {
  if (!isRecord(value) || !Array.isArray(value.agents) || !Array.isArray(value.models) || !Array.isArray(value.projects)) {
    return null;
  }
  try {
    return canonicalDashboardFilters({
      agents: value.agents as string[],
      models: value.models as DashboardFilters["models"],
      projects: value.projects as DashboardFilters["projects"],
    });
  } catch {
    return null;
  }
}

function browserStorage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function storageOrNull(storage?: Storage | null): Storage | null {
  return storage === undefined ? browserStorage() : storage;
}

export function hasActiveDashboardFilters(filters: DashboardFilters): boolean {
  return filters.agents.length > 0 || filters.models.length > 0 || filters.projects.length > 0;
}

export function readDashboardFilterPersistence(storage?: Storage | null): PersistedFilterState {
  const target = storageOrNull(storage);
  if (!target) return emptyState();
  try {
    const raw = target.getItem(DASHBOARD_FILTERS_STORAGE_KEY);
    if (!raw) return emptyState();
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed) || parsed.version !== 1) {
      target.removeItem(DASHBOARD_FILTERS_STORAGE_KEY);
      return emptyState();
    }
    const filters = parseFilters(parsed.filters);
    if (!filters) {
      target.removeItem(DASHBOARD_FILTERS_STORAGE_KEY);
      return emptyState();
    }
    if (!hasActiveDashboardFilters(filters)) {
      target.removeItem(DASHBOARD_FILTERS_STORAGE_KEY);
      return emptyState();
    }
    return { filters, options: parseOptions(parsed.options) };
  } catch {
    try {
      target.removeItem(DASHBOARD_FILTERS_STORAGE_KEY);
    } catch {
      // Storage can be unavailable or read-only; in-memory filtering still works.
    }
    return emptyState();
  }
}

export function writeDashboardFilterPersistence(state: PersistedFilterState, storage?: Storage | null): void {
  const target = storageOrNull(storage);
  if (!target) return;
  try {
    if (!hasActiveDashboardFilters(state.filters)) {
      target.removeItem(DASHBOARD_FILTERS_STORAGE_KEY);
      return;
    }
    target.setItem(DASHBOARD_FILTERS_STORAGE_KEY, JSON.stringify({
      version: 1,
      filters: canonicalDashboardFilters(state.filters),
      options: state.options,
    }));
  } catch {
    // Filtering remains usable even when browser storage is unavailable.
  }
}

export function selectedFilterOptions(
  filters: DashboardFilters,
  options: Pick<FilterOptionsResponse, "agents" | "models" | "projects"> | null,
  previous: PersistedFilterOptions,
): PersistedFilterOptions {
  const canonical = canonicalDashboardFilters(filters);
  const previousAgents = new Map(previous.agents.map((option) => [option.agent, option]));
  const previousModels = new Map(previous.models.map((option) => [modelSelectionKey(option), option]));
  const previousProjects = new Map(previous.projects.map((option) => [projectKey(option), option]));
  const currentAgents = new Map((options?.agents ?? []).map((option) => [option.agent, option]));
  const currentModels = new Map((options?.models ?? []).map((option) => [modelSelectionKey(option), option]));
  const currentProjects = new Map((options?.projects ?? []).map((option) => [projectKey(option), option]));

  return {
    agents: canonical.agents.map((agent) =>
      currentAgents.get(agent) ?? previousAgents.get(agent) ?? { agent, display_name: agent },
    ),
    models: canonical.models.map((model) => {
      const key = modelSelectionKey(model);
      return currentModels.get(key) ?? previousModels.get(key) ?? {
        ...model,
        provider_name: model.provider_id,
      };
    }),
    projects: canonical.projects.map((project) => {
      const key = projectKey(project);
      return currentProjects.get(key) ?? previousProjects.get(key) ?? (
        project.kind === "project"
          ? { kind: "project", path: project.path, name: project.path }
          : project
      );
    }),
  };
}
