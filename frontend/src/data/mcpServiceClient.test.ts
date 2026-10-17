import { describe, expect, it, vi } from "vitest";

import { createMcpServiceClient } from "./mcpServiceClient";
import type { McpApp } from "./mcpUsagiClient";

describe("McpServiceClient", () => {
  it("maps state reads and stops to their MCP tools and notifies after stop", async () => {
    const callServerTool = vi.fn()
      .mockResolvedValueOnce({ structuredContent: { status: "ready", operation: "service_state", payload: { state: "running" } } })
      .mockResolvedValueOnce({ structuredContent: { status: "ready", operation: "stop_service", payload: { state: "stopped" } } });
    const app = { callServerTool } satisfies McpApp;
    const onRuntimeUnavailable = vi.fn();
    const client = createMcpServiceClient(app, onRuntimeUnavailable);

    await expect(client.getState()).resolves.toBe("running");
    expect(onRuntimeUnavailable).not.toHaveBeenCalled();
    await expect(client.stop()).resolves.toBe("stopped");

    expect(callServerTool).toHaveBeenCalledTimes(2);
    expect(callServerTool).toHaveBeenNthCalledWith(1, { name: "usagi.dashboard.read", arguments: { operation: "service_state" } });
    expect(callServerTool).toHaveBeenNthCalledWith(2, { name: "usagi.dashboard.stop_service", arguments: {} });
    expect(onRuntimeUnavailable).toHaveBeenCalledTimes(1);
  });

  it("reports an unavailable service as stopped", async () => {
    const callServerTool = vi.fn(async () => ({ structuredContent: { status: "not_running", operation: "service_state" } }));
    const onRuntimeUnavailable = vi.fn();
    const client = createMcpServiceClient({ callServerTool }, onRuntimeUnavailable);

    await expect(client.getState()).resolves.toBe("stopped");
    expect(onRuntimeUnavailable).toHaveBeenCalledTimes(1);
  });
});
