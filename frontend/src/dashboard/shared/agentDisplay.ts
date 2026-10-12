import type { AgentFilterOption } from "../../data/types";

export type AgentDisplayLookup = (agent: string) => string;

export function createAgentDisplayLookup(
  agents: readonly AgentFilterOption[] | null | undefined,
): AgentDisplayLookup {
  const map = new Map<string, string>();
  if (agents) {
    for (const item of agents) {
      map.set(item.agent, item.display_name);
    }
  }
  return (agent: string) => map.get(agent) ?? agent;
}
