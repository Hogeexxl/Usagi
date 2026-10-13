import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { DistributionItem } from "./distribution";
import { chartSeriesColor } from "./chartPalette";
import { DistributionDonutCard } from "./DistributionDonutCard";

function item(id: string, totalTokens: number, cost = totalTokens / 100): DistributionItem {
  return {
    id,
    label: id,
    totalTokens,
    cost,
    unpriced: 0,
  };
}

const twoItems = [item("Alpha", 900, 9), item("Beta", 100, 1)];
const sixItems = [
  item("Alpha", 600, 6),
  item("Beta", 500, 5),
  item("Gamma", 400, 4),
  item("Delta", 300, 3),
  item("Epsilon", 200, 2),
  item("Zeta", 100, 1),
];

function cardFrom(container: HTMLElement): HTMLElement {
  const card = container.querySelector("article");
  if (!card) throw new Error("Distribution surface not found");
  return card as HTMLElement;
}

function donutFrom(card: HTMLElement): SVGSVGElement {
  const svg = card.querySelector("svg");
  if (!svg) throw new Error("Donut SVG not found");
  return svg as SVGSVGElement;
}

function centerFrom(card: HTMLElement): HTMLElement {
  const center = card.querySelector<HTMLElement>(".pointer-events-none.absolute.inset-0");
  if (!center) throw new Error("Donut center not found");
  return center;
}

function legendButtons(card: HTMLElement): HTMLButtonElement[] {
  return Array.from(card.querySelectorAll<HTMLButtonElement>("button")).filter(
    (button) => button.closest('[role="tablist"]') === null,
  );
}

function motionOpacity(element: HTMLElement | SVGElement): string {
  return element.style.opacity || element.getAttribute("opacity") || "";
}

function segmentGeometry(svg: SVGSVGElement) {
  return Array.from(svg.querySelectorAll("circle")).map((circle) => ({
    cx: circle.getAttribute("cx"),
    cy: circle.getAttribute("cy"),
    r: circle.getAttribute("r"),
    strokeDasharray: circle.getAttribute("stroke-dasharray"),
    strokeDashoffset: circle.getAttribute("stroke-dashoffset"),
  }));
}

describe("DistributionDonutCard", () => {
  it("[T-S06-002] keeps a 140px Donut geometry and top anchor with one or six segments", () => {
    const { container, rerender } = render(<DistributionDonutCard modelItems={[item("Only", 100)]} projectItems={[]} />);
    const oneCard = cardFrom(container);
    const oneSvg = donutFrom(oneCard);
    const oneWrapper = oneSvg.parentElement as HTMLElement;

    expect(oneCard).toHaveClass("h-[264px]");
    expect(oneWrapper).toHaveClass("relative", "h-[140px]", "w-[140px]");
    expect(oneSvg).toHaveAttribute("width", "140");
    expect(oneSvg).toHaveAttribute("height", "140");
    expect(oneSvg).toHaveAttribute("viewBox", "0 0 140 140");
    expect(oneSvg).toHaveClass("h-[140px]", "w-[140px]");
    expect(oneSvg.parentElement?.parentElement).toHaveClass(
      "grid-cols-[140px_minmax(0,1fr)]",
      "items-start",
      "gap-8",
    );
    expect(oneSvg.querySelectorAll("circle")).toHaveLength(1);
    for (const circle of oneSvg.querySelectorAll("circle")) {
      expect(circle).toHaveAttribute("cx", "70");
      expect(circle).toHaveAttribute("cy", "70");
      expect(circle).toHaveAttribute("r", "65.5");
      expect(circle).toHaveAttribute("pathLength", "100");
      expect(circle).toHaveAttribute("stroke-width", "8");
    }
    expect(oneSvg.querySelectorAll("circle")[0]).toHaveAttribute("stroke-linecap", "round");
    const oneTop = oneWrapper.getBoundingClientRect().top;

    rerender(<DistributionDonutCard modelItems={sixItems} projectItems={[]} />);
    const sixCard = cardFrom(container);
    const sixSvg = donutFrom(sixCard);
    const sixWrapper = sixSvg.parentElement as HTMLElement;
    expect(sixSvg.querySelectorAll("circle")).toHaveLength(6);
    expect(sixWrapper.getBoundingClientRect().top).toBeCloseTo(oneTop, 0);
    expect(sixSvg.querySelectorAll("circle")).toHaveLength(6);
    for (const circle of sixSvg.querySelectorAll("circle")) {
      expect(circle).toHaveAttribute("cx", "70");
      expect(circle).toHaveAttribute("cy", "70");
      expect(circle).toHaveAttribute("r", "65.5");
      expect(circle).toHaveAttribute("pathLength", "100");
      expect(circle).toHaveAttribute("stroke-width", "8");
      expect(circle).toHaveAttribute("stroke-linecap", "round");
    }
    expect(sixSvg.querySelector('circle[stroke="var(--border-strong)"]')).not.toBeInTheDocument();
    expect(Array.from(sixSvg.querySelectorAll("circle")).every((circle) => {
      const [dashLength] = (circle.getAttribute("stroke-dasharray") ?? "").split(" ").map(Number);
      return dashLength > 0 && dashLength < 100;
    })).toBe(true);
  });

  it("maps every ranked series to a fixed local token and keeps Other neutral", () => {
    expect(Array.from({ length: 10 }, (_, index) => chartSeriesColor(index))).toEqual([
      "var(--chart-series-1)",
      "var(--chart-series-2)",
      "var(--chart-series-3)",
      "var(--chart-series-4)",
      "var(--chart-series-5)",
      "var(--chart-series-6)",
      "var(--chart-series-7)",
      "var(--chart-series-8)",
      "var(--chart-series-9)",
      "var(--chart-series-10)",
    ]);
    expect(chartSeriesColor(10)).toBe("var(--chart-series-1)");
    expect(chartSeriesColor(0, true)).toBe("var(--chart-other)");
  });

  it("[T-S06-002] uses the official Pill tabs and compact muted legend without a hover surface", () => {
    const { container } = render(<DistributionDonutCard modelItems={sixItems} projectItems={[]} />);
    const card = cardFrom(container);
    const tablists = within(card).getAllByRole("tablist");
    expect(tablists).toHaveLength(2);
    const tabs = tablists.flatMap((tablist) => within(tablist).getAllByRole("tab"));
    for (const tablist of tablists) expect(tablist).toHaveClass("inline-flex", "rounded-full", "bg-card", "p-1");
    expect(tabs).toHaveLength(4);
    for (const tab of tabs) {
      expect(tab).toHaveClass("px-3.5", "py-1.5", "text-sm");
      expect(tab).not.toHaveClass("!p-0.5", "!px-2.5", "!py-1", "text-xs");
    }

    const legends = legendButtons(card);
    expect(legends).toHaveLength(6);
    for (const legend of legends) {
      expect(legend).toHaveClass("h-5", "text-xs", "leading-4", "text-muted-foreground");
      expect(legend).not.toHaveClass(
        "rounded-lg",
        "px-1.5",
        "py-1",
        "hover:bg-primary/5",
        "focus-visible:bg-primary/5",
      );
      expect(legend.firstElementChild).toHaveClass("h-[10px]", "w-[10px]", "rounded-full");
      for (const cell of Array.from(legend.children).slice(1)) {
        expect(cell).not.toHaveClass("text-foreground");
      }
    }
    expect(legends.map((legend) => (legend.firstElementChild as HTMLElement).style.background)).toEqual([
      "var(--chart-series-1)",
      "var(--chart-series-2)",
      "var(--chart-series-3)",
      "var(--chart-series-4)",
      "var(--chart-series-5)",
      "var(--chart-other)",
    ]);
  });

  it("[T-S06-008] keeps center semantics on NumberTicker and switches Token/Cost", () => {
    const { container } = render(<DistributionDonutCard modelItems={twoItems} projectItems={[]} />);
    const card = cardFrom(container);
    const center = centerFrom(card);
    const ticker = center.querySelector(".text-lg.font-semibold.leading-6.text-foreground");
    if (!ticker) throw new Error("Donut NumberTicker not found");

    expect(within(center).getByText("Token")).toBeInTheDocument();
    expect(ticker.querySelector(".sr-only")).toHaveTextContent("1K");
    const costTab = within(card).getByRole("tab", { name: "费用" });
    fireEvent.click(costTab);
    expect(costTab).toHaveAttribute("aria-selected", "true");
    expect(within(center).getByText("Cost")).toBeInTheDocument();
    expect(ticker.querySelector(".sr-only")).toHaveTextContent("$10.00");

    fireEvent.click(within(card).getByRole("tab", { name: "Token" }));
    const firstLegend = legendButtons(card)[0];
    fireEvent.focus(firstLegend);
    expect(within(center).getByText("90.0%")).toBeInTheDocument();
    expect(within(center).queryByText("Alpha 90.0%")).not.toBeInTheDocument();
    expect(ticker.querySelector(".sr-only")).toHaveTextContent("900");
  });

  it("[T-S06-004] dims only focused segment and legend opacity while preserving geometry", async () => {
    const { container } = render(<DistributionDonutCard modelItems={twoItems} projectItems={[]} />);
    const card = cardFrom(container);
    const svg = donutFrom(card);
    const segments = Array.from(svg.querySelectorAll("circle")) as SVGCircleElement[];
    const legends = legendButtons(card);
    const geometry = segmentGeometry(svg);

    fireEvent.focus(legends[0]);
    await waitFor(() => {
      expect(motionOpacity(segments[0])).toBe("1");
      expect(motionOpacity(segments[1])).toBe("0.22");
      expect(motionOpacity(legends[0])).toBe("1");
      expect(motionOpacity(legends[1])).toBe("0.22");
    });
    expect(segmentGeometry(svg)).toEqual(geometry);

    fireEvent.pointerEnter(segments[1]);
    await waitFor(() => {
      expect(motionOpacity(segments[0])).toBe("0.22");
      expect(motionOpacity(segments[1])).toBe("1");
      expect(motionOpacity(legends[0])).toBe("0.22");
      expect(motionOpacity(legends[1])).toBe("1");
    });
    expect(segmentGeometry(svg)).toEqual(geometry);
  });

  it("[T-S06-002] keeps an empty distribution as an explicit empty state", () => {
    const { container } = render(<DistributionDonutCard modelItems={[]} projectItems={[]} />);
    const card = cardFrom(container);
    expect(within(card).getByText("暂无数据")).toBeInTheDocument();
    expect(legendButtons(card)).toHaveLength(0);
    expect(within(centerFrom(card)).getByText("Token")).toBeInTheDocument();
    const emptySvg = donutFrom(card);
    expect(emptySvg.querySelectorAll("circle")).toHaveLength(1);
    expect(emptySvg.querySelector("circle")).toHaveAttribute("stroke", "var(--border-strong)");
  });
});
