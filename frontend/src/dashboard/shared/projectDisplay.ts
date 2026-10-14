import type { ProjectFilterOption, ProjectSelection } from "../../data/types";

export type ProjectLike = ProjectFilterOption | ProjectSelection;

export function projectKey(project: ProjectLike): string {
  return project.kind === "project"
    ? `project:${project.path}`
    : project.kind;
}

export function projectDisplay(project: ProjectLike): string {
  if (project.kind === "project") return "name" in project ? project.name : project.path;
  if (project.kind === "workspace") return "临时工作区";
  return project.kind === "projectless" ? "无项目会话" : "未知会话";
}

export function projectTitle(project: ProjectLike): string | undefined {
  return project.kind === "project" ? project.path : undefined;
}
