import { act, render, screen } from "@testing-library/react";
import { useRef } from "react";
import { describe, expect, it, vi } from "vitest";

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

function TransformPositionHarness({ active }: { active: boolean }) {
  const motionRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const layout = usePopoverPortalPosition(triggerRef, contentRef, active);

  return (
    <div ref={motionRef} data-testid="motion-card">
      <button ref={triggerRef}>trigger</button>
      <div ref={contentRef}>content</div>
      <output data-testid="trigger-left">{layout?.trigger.left ?? "empty"}</output>
    </div>
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

  it("remeasures the trigger after its motion card transforms", () => {
    const frames: FrameRequestCallback[] = [];
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      frames.push(callback);
      return frames.length;
    });
    vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => undefined);

    const view = render(<TransformPositionHarness active />);
    const motionCard = screen.getByTestId("motion-card");
    const trigger = screen.getByRole("button", { name: "trigger" });
    vi.spyOn(trigger, "getBoundingClientRect").mockImplementation(() => {
      const left = motionCard.style.transform === "translateX(100px)" ? 120 : 20;
      return {
        x: left,
        y: 30,
        left,
        top: 30,
        right: left + 40,
        bottom: 46,
        width: 40,
        height: 16,
        toJSON: () => ({}),
      };
    });

    act(() => {
      motionCard.style.transform = "translateX(100px)";
      frames.shift()?.(16);
    });

    expect(screen.getByTestId("trigger-left")).toHaveTextContent("120");
    view.unmount();
  });
});
