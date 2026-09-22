import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Table, type TableColumn } from "./index";

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
});
