import { afterEach, describe, expect, it, vi } from "vitest";

import { usagiClient } from "./usagiClient";

const status = {
  current_version: "0.1.0",
  latest_version: "0.1.1",
  update_available: true,
  release_url: "https://github.com/Hogeexxl/Usagi/releases/tag/v0.1.1",
  last_checked_at_ms: 1234,
  checking: false,
};

afterEach(() => vi.restoreAllMocks());

describe("update API client", () => {
  it("uses relative same-origin status/check/open-release endpoints and the action header", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch");
    fetchMock
      .mockResolvedValueOnce(new Response(JSON.stringify(status), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(status), { status: 200 }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));

    await expect(usagiClient.getUpdateStatus()).resolves.toEqual(status);
    await expect(usagiClient.checkUpdate()).resolves.toEqual(status);
    await expect(usagiClient.openRelease()).resolves.toBeUndefined();

    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      "/api/update/status",
      expect.objectContaining({ method: "GET", credentials: "same-origin" }),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "/api/update/check",
      expect.objectContaining({
        method: "POST",
        credentials: "same-origin",
        headers: expect.objectContaining({ "X-Usagi-Request": "1" }),
      }),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      3,
      "/api/update/open-release",
      expect.objectContaining({
        method: "POST",
        credentials: "same-origin",
        headers: expect.objectContaining({ "X-Usagi-Request": "1" }),
      }),
    );
    expect(fetchMock.mock.calls.every(([url]) => String(url).startsWith("/api/update/"))).toBe(true);
  });
});
