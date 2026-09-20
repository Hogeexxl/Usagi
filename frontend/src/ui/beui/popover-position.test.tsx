import { render, screen } from "@testing-library/react";
import { useRef } from "react";
import { describe, expect, it } from "vitest";

import { usePopoverPortalPosition } from "./popover-position";

function PositionHarness({ active }: { active: boolean }) {
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const layout = usePopoverPortalPosition(triggerRef, contentRef, active);

  return (
    <>
      <button ref={triggerRef}>trigger</button>
      <div ref={contentRef}>content</div>
      <output data-testid="layout-state">{layout ? "ready" : "empty"}</output>
    </>
  );
}

describe("usePopoverPortalPosition", () => {
  it("drops cached geometry while inactive so a reopened portal cannot paint stale coordinates", () => {
    const view = render(<PositionHarness active />);

    expect(screen.getByTestId("layout-state")).toHaveTextContent("ready");

    view.rerender(<PositionHarness active={false} />);

    expect(screen.getByTestId("layout-state")).toHaveTextContent("empty");

    view.rerender(<PositionHarness active />);

    expect(screen.getByTestId("layout-state")).toHaveTextContent("ready");
  });
});
