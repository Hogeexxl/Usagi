import { describe, expect, it } from "vitest";
import { buildDistribution, type DistributionItem } from "./distribution";
import { chartSeriesColor } from "./chartPalette";

const items: DistributionItem[] = Array.from({ length: 7 }, (_, index) => ({
  id: `m${index}`,
  label: `m${index}`,
  totalTokens: 70 - index * 10,
  cost: index === 6 ? 0 : 7 - index,
  unpriced: index === 6 ? 1 : index === 5 ? 2 : 0,
}));

describe("distribution ranking", () => {
  it("uses Top 5 + Other for tokens", () => {
    const tokens = buildDistribution(items, "tokens");
    expect(tokens.segments).toHaveLength(6);
    expect(tokens.segments.at(-1)).toMatchObject({ id: "__other__", label: "其他", isOther: true });
    expect(tokens.total).toBe(items.reduce((sum, item) => sum + item.totalTokens, 0));
  });

  it("uses the priced cost total and carries unpriced counts into Other", () => {
    const cost = buildDistribution(items, "cost");
    expect(cost.segments.some((segment) => segment.id === "m6")).toBe(false);
    expect(cost.total).toBe(27);
    expect(cost.segments.at(-1)).toMatchObject({ id: "__other__", label: "其他", value: 2, cost: 2, unpriced: 3, isOther: true });
  });

  it("[T-S06-003] maps ranked series to the shared palette and keeps Other neutral", () => {
    expect(chartSeriesColor(0)).toBe("var(--chart-series-1)");
    expect(chartSeriesColor(4)).toBe("var(--chart-series-5)");
    expect(chartSeriesColor(5)).toBe("var(--chart-series-6)");
    expect(chartSeriesColor(9)).toBe("var(--chart-series-10)");
    expect(chartSeriesColor(0, true)).toBe("var(--chart-other)");
  });
});
