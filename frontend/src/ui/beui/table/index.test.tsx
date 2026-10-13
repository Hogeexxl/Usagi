import { render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { Table, type TableColumn } from "./index";

vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count, estimateSize }: { count: number; estimateSize: () => number }) => {
    const size = estimateSize();
    return {
      getVirtualItems: () => Array.from({ length: count }, (_, index) => ({
        index,
        key: index,
        start: index * size,
        size,
        end: (index + 1) * size,
        lane: null,
      })),
      getTotalSize: () => count * size,
    };
  },
}));

type TestItem = { id: string; name: string };

describe("BeUI Table pt absolute width resolution", () => {
  it("TD-P5-PT-01 converts pt widths to px for minTableWidth while preserving pt string in DOM", () => {
    const columns: TableColumn<TestItem>[] = [
      { key: "col1", header: "Col 1", width: "98pt" },
      { key: "col2", header: "Col 2", width: "140pt" },
      { key: "col3", header: "Col 3", width: "80pt" },
    ];

    const data: TestItem[] = [{ id: "1", name: "item1" }];

    const { container } = render(
      <Table
        columns={columns}
        data={data}
        getRowId={(item) => item.id}
      />,
    );

    const table = container.querySelector("table");
    expect(table).toBeInTheDocument();

    // (98 + 140 + 80) * (96 / 72) = 318 * (4 / 3) = 424px
    expect(table?.style.minWidth).toBe("424px");

    const cols = container.querySelectorAll<HTMLTableColElement>("colgroup col");
    expect(cols[0]?.style.width).toBe("98pt");
    expect(cols[1]?.style.width).toBe("140pt");
    expect(cols[2]?.style.width).toBe("80pt");
  });

  it("defaults headerHeight to rowHeight and allows an independent header height", () => {
    const columns: TableColumn<TestItem>[] = [{ key: "name", header: "Name" }];
    const data: TestItem[] = [{ id: "1", name: "item1" }];
    const { container, rerender } = render(
      <Table columns={columns} data={data} getRowId={(item) => item.id} rowHeight={64} height={128} />,
    );
    expect(container.querySelector("thead tr")).toHaveStyle({ height: "64px" });
    expect(container.querySelector("tbody tr")).toHaveStyle({ height: "64px" });

    rerender(
      <Table columns={columns} data={data} getRowId={(item) => item.id} rowHeight={64} headerHeight={48} height={112} />,
    );
    expect(container.querySelector("thead tr")).toHaveStyle({ height: "48px" });
    expect(container.querySelector("tbody tr")).toHaveStyle({ height: "64px" });
  });
});
