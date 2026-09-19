import { afterEach, describe, expect, it, vi } from "vitest";

import { serviceClient } from "./serviceClient";

afterEach(() => vi.restoreAllMocks());

describe("serviceClient", () => {
  it("reads state and protects full process shutdown with the local-action header", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(new Response(JSON.stringify({ state: "running" }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ state: "stopped" }), { status: 200 }));

    await expect(serviceClient.getState()).resolves.toBe("running");
    await expect(serviceClient.stop()).resolves.toBe("stopped");

    expect(fetchMock).toHaveBeenNthCalledWith(1, "/api/service", expect.objectContaining({ method: "GET" }));
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "/api/service/stop",
      expect.objectContaining({ method: "POST", headers: expect.objectContaining({ "X-Usagi-Request": "1" }) }),
    );
  });
});
