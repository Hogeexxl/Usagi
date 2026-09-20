import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { RangeSelector } from "./RangeSelector";

describe("RangeSelector", () => {
  it("renders only the requested preset ranges when custom is hidden", () => {
    const onChange = vi.fn();
    render(
      <RangeSelector
        value={{ key: "today" }}
        onChange={onChange}
        ranges={["today", "yesterday", "7d"]}
        showCustom={false}
      />,
    );
    expect(screen.getAllByRole("tab")).toHaveLength(3);
    for (const label of ["今天", "昨天", "7d"]) {
      expect(screen.getByRole("tab", { name: label })).toBeInTheDocument();
    }
    expect(screen.queryByRole("tab", { name: "自定义" })).not.toBeInTheDocument();
  });

  it("exposes the range tablist and selected state", () => {
    const onChange = vi.fn();
    render(<RangeSelector value={{ key: "today" }} onChange={onChange} />);
    expect(screen.getByRole("tablist")).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(6);
    for (const label of ["今天", "昨天", "7d", "30d", "今年", "自定义"]) {
      expect(screen.getByRole("tab", { name: label })).toBeInTheDocument();
    }
    expect(screen.getByRole("tab", { name: "今天" })).toHaveAttribute("aria-selected", "true");
    fireEvent.click(screen.getByRole("tab", { name: "30d" }));
    expect(onChange).toHaveBeenCalledWith({ key: "30d" });
  });

  it("T-022-A3 applies a complete range immediately and has no confirmation controls", () => {
    const onChange = vi.fn();
    render(<RangeSelector value={{ key: "today" }} onChange={onChange} />);
    fireEvent.click(screen.getByRole("tab", { name: "自定义" }));
    expect(screen.getByRole("dialog", { name: "自定义日期范围" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /应用|确定|取消/ })).not.toBeInTheDocument();

    const firstDate = new Date();
    const secondDate = new Date(firstDate);
    secondDate.setDate(secondDate.getDate() + 1);
    const key = (date: Date) => `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
    const findDay = (date: Date) => {
      const expected = date.toLocaleDateString();
      return Array.from(document.querySelectorAll<HTMLButtonElement>("button[data-day]")).find(
        (button) => button.dataset.day === expected,
      );
    };
    const first = findDay(firstDate);
    const second = findDay(secondDate);
    if (!first || !second) throw new Error("Calendar test dates were not rendered");
    fireEvent.click(first);
    expect(onChange).not.toHaveBeenCalled();
    const secondAfterDraft = findDay(secondDate);
    if (!secondAfterDraft) throw new Error("Calendar test end date was not rendered");
    fireEvent.click(secondAfterDraft);
    expect(onChange).toHaveBeenCalledWith({ key: "custom", from: key(firstDate), to: key(secondDate) });
    expect(screen.queryByRole("dialog", { name: "自定义日期范围" })).not.toBeInTheDocument();
  });

  it("discards an incomplete range when the standard popover closes with Escape", () => {
    const onChange = vi.fn();
    const key = (date: Date) => `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
    const fromDate = new Date();
    fromDate.setDate(fromDate.getDate() - 2);
    const toDate = new Date();
    toDate.setDate(toDate.getDate() - 1);
    render(<RangeSelector value={{ key: "custom", from: key(fromDate), to: key(toDate) }} onChange={onChange} />);

    const customTab = screen.getByRole("tab", { name: "自定义" });
    expect(customTab).toHaveAttribute("aria-selected", "true");
    fireEvent.click(customTab);
    expect(screen.getByRole("dialog", { name: "自定义日期范围" })).toBeInTheDocument();

    const findDay = (date: Date) => {
      const expected = date.toLocaleDateString();
      return Array.from(document.querySelectorAll<HTMLButtonElement>("button[data-day]")).find(
        (button) => button.dataset.day === expected,
      );
    };
    const selectedFrom = findDay(fromDate);
    const selectedTo = findDay(toDate);
    if (!selectedFrom || !selectedTo) throw new Error("Calendar test custom range was not rendered");
    expect(selectedFrom).toHaveAttribute("data-range-start", "true");
    expect(selectedTo).toHaveAttribute("data-range-end", "true");

    const draftDate = new Date();
    draftDate.setDate(draftDate.getDate() + 1);
    const draftStart = findDay(draftDate);
    if (!draftStart) throw new Error("Calendar test draft date was not rendered");
    fireEvent.click(draftStart);
    expect(onChange).not.toHaveBeenCalled();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "自定义日期范围" })).not.toBeInTheDocument();

    fireEvent.click(customTab);
    expect(screen.getByRole("dialog", { name: "自定义日期范围" })).toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();
  });
});
