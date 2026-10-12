import type { ProjectFilterOption, ProjectSelection } from "../../data/types";

export type ProjectLike = ProjectFilterOption | ProjectSelection;

export function projectKey(project: ProjectLike): string {
  return project.kind === "project" ? `project:${project.path}` : project.kind;
}

export function projectDisplay(project: ProjectLike): string {
  if (project.kind === "project") return "name" in project ? project.name : project.path;
  return project.kind === "projectless" ? "无项目会话" : "未识别项目";
}

export function projectTitle(project: ProjectLike): string | undefined {
  return project.kind === "project" ? project.path : undefined;
}
