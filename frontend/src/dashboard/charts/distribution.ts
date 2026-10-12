export type DistributionMetric = "tokens" | "cost";
export type DistributionItem = {
  id: string;
  label: string;
  totalTokens: number;
  cost: number;
  unpriced: number;
  title?: string;
};
export type DistributionSegment = DistributionItem & { value: number; percentage: number; isOther?: boolean };

export function buildDistribution(items: DistributionItem[], metric: DistributionMetric) {
  const eligible = items;
  const valueOf = (item: DistributionItem) => metric === "tokens" ? item.totalTokens : item.cost;
  const sorted = [...eligible].sort((a, b) => valueOf(b) - valueOf(a) || a.label.localeCompare(b.label));
  const total = sorted.reduce((sum, item) => sum + valueOf(item), 0);
  const top = sorted.slice(0, 5);
  const rest = sorted.slice(5);
  const visible: Array<DistributionItem & { value: number; isOther?: boolean }> = top.map((item) => ({ ...item, value: valueOf(item) }));
  if (rest.length > 0) {
    visible.push({
      id: "__other__",
      label: "其他",
      totalTokens: rest.reduce((sum, item) => sum + item.totalTokens, 0),
      cost: rest.reduce((sum, item) => sum + item.cost, 0),
      unpriced: rest.reduce((sum, item) => sum + item.unpriced, 0),
      value: rest.reduce((sum, item) => sum + valueOf(item), 0),
      isOther: true,
    });
  }
  const segments: DistributionSegment[] = visible.map((item) => ({ ...item, percentage: total > 0 ? item.value / total : 0 }));
  return { total, segments };
}
