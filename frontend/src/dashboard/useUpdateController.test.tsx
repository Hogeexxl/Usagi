import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "../data/usagiClient";
import { UpdateButton } from "./UpdateButton";

const latest = (update_available: boolean) => ({
  current_version: "0.1.0",
  latest_version: update_available ? "0.1.1" : "0.1.0",
  update_available,
  release_url: "https://github.com/Hogeexxl/Usagi/releases/tag/v0.1.1",
  last_checked_at_ms: 1234,
  checking: false,
});

function clientWith(overrides: Partial<UsagiClient> = {}): UsagiClient {
  return {
    getUpdateStatus: vi.fn(async () => latest(false)),
    openRelease: vi.fn(async () => undefined),
    ...overrides,
  } as UsagiClient;
}

afterEach(() => {
  vi.useRealTimers();
});

describe("UpdateButton / useUpdateController", () => {
  it("stays hidden until polling finds an update, then opens its release", async () => {
    vi.useFakeTimers();
    const getUpdateStatus = vi.fn()
      .mockResolvedValueOnce(latest(false))
      .mockResolvedValueOnce(latest(true));
    const openRelease = vi.fn(async () => undefined);
    const client = clientWith({ getUpdateStatus, openRelease });

    render(<UpdateButton client={client} />);
    expect(screen.queryByRole("button", { name: "检测到新版本" })).not.toBeInTheDocument();

    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(getUpdateStatus).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "检测到新版本" })).not.toBeInTheDocument();

    await act(async () => {
      vi.advanceTimersByTime(60_000);
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(getUpdateStatus).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("button", { name: "检测到新版本" })).toBeEnabled();

    fireEvent.click(screen.getByRole("button", { name: "检测到新版本" }));
    expect(openRelease).toHaveBeenCalledTimes(1);
  });
});
