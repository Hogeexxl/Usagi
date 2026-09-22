import type { SourceFilterOption } from "../../data/types";

export type SourceDisplayLookup = (source: string) => string;

export function createSourceDisplayLookup(
  sources: readonly SourceFilterOption[] | null | undefined,
): SourceDisplayLookup {
  const map = new Map<string, string>();
  if (sources) {
    for (const item of sources) {
      map.set(item.source, item.display_name);
    }
  }
  return (source: string) => map.get(source) ?? source;
}
