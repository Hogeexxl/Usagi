import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { SkillDayDto, SkillsUsageResponse } from "../../data/types";
import { SkillsUsageChart } from "./SkillsUsageChart";

function makeDay(date: string, skills: Array<[string, number]>): SkillDayDto {
  return {
    date,
    start_ms: 0,
    end_ms: 1,
    total: skills.reduce((sum, [, count]) => sum + count, 0),
    skills: skills.map(([skill_name, count]) => ({ skill_name, count })),
  };
}

function responseFromDays(
  days: SkillDayDto[],
  data_status: SkillsUsageResponse["data_status"] = "ready",
): SkillsUsageResponse {
  return { data_revision: 1, data_status, days };
}

function sevenDays(skillsByDay: Array<Array<[string, number]>>): SkillDayDto[] {
  return Array.from({ length: 7 }, (_, index) =>
    makeDay(`2026-08-${String(index + 1).padStart(2, "0")}`, skillsByDay[index] ?? []),
  );
}

const chartResponse = responseFromDays(
  sevenDays([
    [["zeta", 2], ["alpha", 2], ["beta", 1]],
    [["alpha", 2], ["zeta", 2], ["beta", 1]],
    [["alpha", 2], ["zeta", 2], ["beta", 1]],
    [["alpha", 2], ["zeta", 2], ["beta", 1]],
    [["alpha", 2], ["zeta", 2], ["beta", 1]],
    [["alpha", 2], ["zeta", 2], ["beta", 1]],
    [["alpha", 2], ["zeta", 2], ["beta", 1]],
  ]),
);

const paletteResponse = responseFromDays(
  sevenDays([
    [
      ["skill-11", 11],
      ["skill-10", 10],
      ["skill-09", 9],
      ["skill-08", 8],
      ["skill-07", 7],
      ["skill-06", 6],
      ["skill-05", 5],
      ["skill-04", 4],
      ["skill-03", 3],
      ["skill-02", 2],
      ["skill-01", 1],
    ],
    [],
    [],
    [],
    [],
    [],
    [],
  ]),
);

function dateButtons(container: HTMLElement): HTMLButtonElement[] {
  return Array.from(container.querySelectorAll<HTMLButtonElement>("button[aria-label^='2026-08-']"));
}

function guideLine(svg: SVGSVGElement): SVGLineElement | undefined {
  return Array.from(svg.querySelectorAll<SVGLineElement>("line")).find(
    (line) => line.getAttribute("stroke") === "var(--foreground)",
  );
}

function setSvgRect(svg: SVGSVGElement, left = 100, width = 900) {
  Object.defineProperty(svg, "getBoundingClientRect", {
    configurable: true,
    value: () => ({
      left,
      top: 0,
      width,
      height: 210,
      right: left + width,
      bottom: 210,
      x: left,
      y: 0,
      toJSON: () => ({}),
    }),
  });
}

function activePopoverDialog(): HTMLElement {
  const dialog = screen.getAllByRole("dialog").find(
    (candidate) =>
      !candidate.hasAttribute("inert") && candidate.parentElement?.style.pointerEvents === "auto",
  );
  if (!(dialog instanceof HTMLElement)) throw new Error("An open Skills date popover was not found");
  return dialog;
}

function motionOpacity(element: SVGPathElement): string {
  return element.style.opacity || element.getAttribute("opacity") || "";
}

describe("SkillsUsageChart v0.2.0", () => {
  it("[T-S06-005] renders Skills Used, total, one ChartSurface, fixed plot geometry, and seven 12px date buttons", () => {
    const { container } = render(<SkillsUsageChart response={chartResponse} />);

    const heading = screen.getByRole("heading", { name: "Skills Used" });
    const surface = heading.closest("article");
    expect(surface).not.toBeNull();
    expect(surface).toHaveClass("rounded-[28px]", "border-border", "bg-[#fcfcfc]", "dark:bg-[#151515]", "p-5");
    expect(screen.getByTitle("35")).toBeInTheDocument();

    const svg = screen.getByRole("img", { name: "最近 7 个自然日 Skills 使用次数" });
    expect(svg).toHaveAttribute("viewBox", "0 0 900 210");
    expect(Array.from(svg.querySelectorAll("text")).every((text) => text.getAttribute("font-size") === "12")).toBe(true);
    expect(svg.querySelectorAll("rect")).toHaveLength(0);

    const buttons = dateButtons(container);
    expect(buttons).toHaveLength(7);
    expect(buttons.map((button) => button.textContent)).toEqual([
      "08-01",
      "08-02",
      "08-03",
      "08-04",
      "08-05",
      "08-06",
      "08-07",
    ]);
    for (const button of buttons) {
      expect(button).toHaveClass("text-xs", "leading-4", "text-muted-foreground");
    }

    const axisBand = buttons[0].parentElement?.parentElement?.parentElement;
    expect(axisBand).toBeInstanceOf(HTMLElement);
    expect(Number.parseFloat((axisBand as HTMLElement).style.left)).toBeCloseTo((44 / 900) * 100);
    expect(Number.parseFloat((axisBand as HTMLElement).style.right)).toBeCloseTo((12 / 900) * 100);
    expect(Number.parseFloat((axisBand as HTMLElement).style.top)).toBeCloseTo(((14 + 168) / 210) * 100);
    expect(Number.parseFloat((axisBand as HTMLElement).style.height)).toBeCloseTo((28 / 210) * 100);
  });

  it("[T-S06-005] maps SVG pointer coordinates with clamp and nearest-day guide geometry", () => {
    const { container } = render(<SkillsUsageChart response={chartResponse} />);
    const svg = container.querySelector("svg");
    if (!(svg instanceof SVGSVGElement)) throw new Error("Skills usage SVG was not found");
    setSvgRect(svg);

    fireEvent.pointerMove(svg, { clientX: 100 + 470 });
    expect(guideLine(svg)).toHaveAttribute("x1", "466");
    expect(guideLine(svg)).toHaveAttribute("y1", "14");
    expect(guideLine(svg)).toHaveAttribute("y2", "182");

    fireEvent.pointerMove(svg, { clientX: -100 });
    expect(guideLine(svg)).toHaveAttribute("x1", "44");
    fireEvent.pointerMove(svg, { clientX: 2_000 });
    expect(guideLine(svg)).toHaveAttribute("x1", "888");

    fireEvent.pointerLeave(svg);
    expect(guideLine(svg)).toBeUndefined();
  });

  it("[T-S06-006] uses the controlled Gooey date popover for keyboard and hover triggers with sorted rows and totals", async () => {
    const response = responseFromDays(
      sevenDays([
        [["zeta", 2], ["alpha", 2], ["beta", 1], ["zero", 0]],
        [],
        [],
        [],
        [],
        [],
        [],
      ]),
    );
    const { container } = render(<SkillsUsageChart response={response} />);
    const buttons = dateButtons(container);

    for (const button of buttons) expect(button).toHaveAttribute("aria-haspopup", "dialog");

    expect(document.querySelectorAll("[data-popover-portal]")).toHaveLength(0);

    fireEvent.focus(buttons[0]);
    await waitFor(() => expect(activePopoverDialog()).toHaveTextContent("2026-08-01"));
    expect(buttons[0]).toHaveAttribute("aria-expanded", "true");
    expect(document.querySelectorAll("[data-popover-portal]")).toHaveLength(1);

    const dialog = activePopoverDialog();
    expect(dialog.closest("[data-popover-portal]")).not.toBeNull();
    expect(Array.from(dialog.querySelectorAll("span.truncate")).map((node) => node.textContent)).toEqual([
      "Alpha",
      "Zeta",
      "Beta",
    ]);
    expect(
      Array.from(dialog.querySelectorAll("span"))
        .filter((node) => node.classList.contains("tabular-nums"))
        .map((node) => node.textContent),
    ).toEqual(["2", "2", "1", "5"]);

    const grid = Array.from(dialog.querySelectorAll("div")).find((node) =>
      node.className.includes("grid-cols-[minmax(0,1fr)_auto]"),
    );
    expect(grid).toHaveClass("gap-x-4", "gap-y-1");
    const swatches = Array.from(dialog.querySelectorAll("span")).filter(
      (node) => node.classList.contains("h-2.5") && node.classList.contains("w-2.5"),
    );
    expect(swatches).toHaveLength(3);
    expect(swatches.every((swatch) => swatch.classList.contains("rounded-full"))).toBe(true);

    const countNodes = Array.from(dialog.querySelectorAll("span")).filter(
      (node) => node.classList.contains("tabular-nums"),
    );
    for (const count of countNodes.slice(0, 3)) {
      expect(count).toHaveClass("text-xs", "font-normal", "text-muted-foreground");
    }
    expect(within(dialog).getByText("Total")).toHaveClass("text-xs", "font-semibold", "text-foreground");

    fireEvent.blur(buttons[0]);
    await waitFor(() => expect(document.querySelectorAll("[data-popover-portal]")).toHaveLength(0));

    fireEvent.pointerEnter(buttons[1], { pointerId: 1, pointerType: "mouse", buttons: 0 });
    await waitFor(() => expect(buttons[1]).toHaveAttribute("aria-expanded", "true"));
    expect(activePopoverDialog()).toHaveTextContent("2026-08-02");
  });

  it("[T-S06-003] maps ranked areas to palette slots with Other, keeps fill opacity, and changes only focus opacity", async () => {
    const { container } = render(<SkillsUsageChart response={paletteResponse} />);
    const svg = screen.getByRole("img", { name: "最近 7 个自然日 Skills 使用次数" });
    const paths = Array.from(svg.querySelectorAll<SVGPathElement>("path"));
    expect(paths).toHaveLength(11);
    expect(paths.map((path) => path.getAttribute("fill"))).toEqual([
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
      "var(--chart-other)",
    ]);
    expect(paths.every((path) => path.getAttribute("fill-opacity") === "0.72")).toBe(true);

    const beforeGeometry = paths.map((path) => path.getAttribute("d"));
    const legends = Array.from(container.querySelectorAll<HTMLButtonElement>("button:not([aria-label])"));
    expect(legends).toHaveLength(11);
    fireEvent.focus(legends[3]);
    await waitFor(() => expect(motionOpacity(paths[3])).toBe("1"));
    expect(paths.filter((_, index) => index !== 3).every((path) => motionOpacity(path) === "0.22")).toBe(true);
    expect(paths.map((path) => path.getAttribute("d"))).toEqual(beforeGeometry);

    fireEvent.pointerEnter(paths[6]);
    await waitFor(() => expect(motionOpacity(paths[6])).toBe("1"));
    expect(paths.filter((_, index) => index !== 6).every((path) => motionOpacity(path) === "0.22")).toBe(true);
    expect(paths.map((path) => path.getAttribute("d"))).toEqual(beforeGeometry);
  });

  it("[T-S06-007] shares one skill popover for area hover and legend focus, with series totals", async () => {
    const { container } = render(<SkillsUsageChart response={chartResponse} />);
    const svg = screen.getByRole("img", { name: "最近 7 个自然日 Skills 使用次数" });
    const paths = Array.from(svg.querySelectorAll<SVGPathElement>("path"));
    const legends = Array.from(container.querySelectorAll<HTMLButtonElement>("button:not([aria-label])"));

    expect(screen.getByTitle("35")).toBeInTheDocument();
    fireEvent.pointerMove(svg, { clientX: 100 + 44 });
    fireEvent.pointerEnter(paths[0], { clientX: 144, clientY: 40 });
    await waitFor(() => expect(activePopoverDialog()).toHaveTextContent("Alpha"));
    expect(document.querySelectorAll("[data-popover-portal]")).toHaveLength(1);
    expect(activePopoverDialog()).toHaveTextContent("2026-08-01");
    expect(activePopoverDialog()).toHaveTextContent("2");
    expect(activePopoverDialog()).toHaveTextContent("7日总数");
    expect(activePopoverDialog()).toHaveTextContent("14");
    expect(screen.getByTitle("14")).toBeInTheDocument();

    fireEvent.pointerLeave(paths[0]);
    fireEvent.pointerLeave(svg);
    await waitFor(() => expect(document.querySelectorAll("[data-popover-portal]")).toHaveLength(0));
    expect(screen.getByTitle("35")).toBeInTheDocument();

    fireEvent.focus(legends[0]);
    await waitFor(() => expect(activePopoverDialog()).toHaveTextContent("Alpha"));
    expect(activePopoverDialog()).toHaveTextContent("2026-08-01");
    expect(activePopoverDialog()).toHaveTextContent("2026-08-07");
    expect(activePopoverDialog()).toHaveTextContent("7日总数");
    expect(activePopoverDialog()).toHaveTextContent("14");
    expect(screen.getByTitle("14")).toBeInTheDocument();

    fireEvent.blur(legends[0]);
    await waitFor(() => expect(document.querySelectorAll("[data-popover-portal]")).toHaveLength(0));
    expect(screen.getByTitle("35")).toBeInTheDocument();
  });

  it("[T-S06-021] uses the ChartSurface skeleton for loading and keeps old data visible while rebuilding", () => {
    const rebuildingEmpty = responseFromDays(sevenDays([]), "rebuilding");
    const { container, rerender } = render(<SkillsUsageChart response={null} />);
    const surface = () => container.querySelector("article");

    expect(surface()).toHaveClass("rounded-[28px]", "border-border", "bg-[#fcfcfc]", "dark:bg-[#151515]", "p-5");
    expect(surface()?.querySelectorAll(".animate-pulse")).toHaveLength(3);
    rerender(<SkillsUsageChart response={rebuildingEmpty} />);
    expect(surface()?.querySelectorAll(".animate-pulse")).toHaveLength(3);

    rerender(<SkillsUsageChart response={{ ...chartResponse, data_status: "rebuilding" }} />);
    expect(screen.getByRole("heading", { name: "Skills Used" })).toBeInTheDocument();
    expect(screen.getByRole("img", { name: "最近 7 个自然日 Skills 使用次数" })).toBeInTheDocument();
    expect(surface()?.querySelectorAll(".animate-pulse")).toHaveLength(0);
  });
});
