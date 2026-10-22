import { describe, expect, it } from "vitest";

import {
  DASHBOARD_FILTERS_STORAGE_KEY,
  EMPTY_DASHBOARD_FILTERS,
  readDashboardFilterPersistence,
  selectedFilterOptions,
  writeDashboardFilterPersistence,
  type PersistedFilterOptions,
} from "./filterPersistence";

function memoryStorage(initial: Record<string, string> = {}): Storage {
  const values = new Map(Object.entries(initial));
  return {
    get length() {
      return values.size;
    },
    clear() {
      values.clear();
    },
    getItem(key) {
      return values.get(key) ?? null;
    },
    key(index) {
      return [...values.keys()][index] ?? null;
    },
    removeItem(key) {
      values.delete(key);
    },
    setItem(key, value) {
      values.set(key, value);
    },
  };
}

const savedOptions: PersistedFilterOptions = {
  agents: [{ agent: "codex", display_name: "Codex" }],
  models: [{ provider_id: "openai", model: "gpt-5", provider_name: "OpenAI" }],
  projects: [{ kind: "project", path: "/work/Usagi", name: "Usagi" }],
};

describe("dashboard filter persistence", () => {
  it("round-trips canonical filters with readable fallback labels", () => {
    const storage = memoryStorage();
    writeDashboardFilterPersistence({
      filters: {
        agents: ["codex", "codex"],
        models: [{ provider_id: "openai", model: "gpt-5" }],
        projects: [{ kind: "project", path: "/work/Usagi" }],
      },
      options: savedOptions,
    }, storage);

    expect(readDashboardFilterPersistence(storage)).toEqual({
      filters: {
        agents: ["codex"],
        models: [{ provider_id: "openai", model: "gpt-5" }],
        projects: [{ kind: "project", path: "/work/Usagi" }],
      },
      options: savedOptions,
    });
  });

  it("removes the key when filters return to the default", () => {
    const storage = memoryStorage({ [DASHBOARD_FILTERS_STORAGE_KEY]: "stale" });

    writeDashboardFilterPersistence({
      filters: EMPTY_DASHBOARD_FILTERS,
      options: savedOptions,
    }, storage);

    expect(storage.getItem(DASHBOARD_FILTERS_STORAGE_KEY)).toBeNull();
  });

  it("drops malformed persisted state instead of crashing the dashboard", () => {
    const storage = memoryStorage({ [DASHBOARD_FILTERS_STORAGE_KEY]: "{not-json" });

    expect(readDashboardFilterPersistence(storage)).toEqual({
      filters: EMPTY_DASHBOARD_FILTERS,
      options: { agents: [], models: [], projects: [] },
    });
    expect(storage.getItem(DASHBOARD_FILTERS_STORAGE_KEY)).toBeNull();
  });

  it("prefers current option labels, then previous labels, then stable IDs", () => {
    const options = selectedFilterOptions(
      {
        agents: ["codex", "claude"],
        models: [
          { provider_id: "openai", model: "gpt-5" },
          { provider_id: "route", model: "orphan" },
        ],
        projects: [
          { kind: "project", path: "/work/Usagi" },
          { kind: "project", path: "/work/old" },
        ],
      },
      {
        agents: [{ agent: "codex", display_name: "Codex Current" }],
        models: [],
        projects: [],
      },
      savedOptions,
    );

    expect(options.agents).toEqual([
      { agent: "claude", display_name: "claude" },
      { agent: "codex", display_name: "Codex Current" },
    ]);
    expect(options.models).toEqual([
      { provider_id: "openai", model: "gpt-5", provider_name: "OpenAI" },
      { provider_id: "route", model: "orphan", provider_name: "route" },
    ]);
    expect(options.projects).toEqual([
      { kind: "project", path: "/work/Usagi", name: "Usagi" },
      { kind: "project", path: "/work/old", name: "/work/old" },
    ]);
  });
});
