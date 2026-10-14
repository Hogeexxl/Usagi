import { describe, expect, it } from "vitest";

import { projectDisplay, projectKey, projectTitle } from "./projectDisplay";

describe("projectDisplay shared utilities", () => {
  it("formats project kinds with names or fallbacks", () => {
    expect(projectDisplay({ kind: "project", name: "Usagi", path: "/work/Usagi" })).toBe("Usagi");
    expect(projectDisplay({ kind: "project", path: "/work/Usagi" })).toBe("/work/Usagi");
  });

  it("uses one label for every workspace", () => {
    expect(projectDisplay({ kind: "workspace" })).toBe("临时工作区");
  });

  it("formats projectless and unknown kinds", () => {
    expect(projectDisplay({ kind: "projectless" })).toBe("无项目会话");
    expect(projectDisplay({ kind: "unknown" })).toBe("未知会话");
  });

  it("extracts projectTitle only for concrete project filters", () => {
    expect(projectTitle({ kind: "project", path: "/work/Usagi" })).toBe("/work/Usagi");
    expect(projectTitle({ kind: "workspace" })).toBeUndefined();
    expect(projectTitle({ kind: "projectless" })).toBeUndefined();
    expect(projectTitle({ kind: "unknown" })).toBeUndefined();
  });

  it("generates projectKey for project, workspace, and category selections", () => {
    expect(projectKey({ kind: "project", path: "/work/Usagi" })).toBe("project:/work/Usagi");
    expect(projectKey({ kind: "workspace" })).toBe("workspace");
    expect(projectKey({ kind: "projectless" })).toBe("projectless");
    expect(projectKey({ kind: "unknown" })).toBe("unknown");
  });
});
