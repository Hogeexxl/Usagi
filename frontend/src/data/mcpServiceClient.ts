import { UsagiClientError } from "./types";
import type { McpApp } from "./mcpUsagiClient";
import { callMcpTool } from "./mcpUsagiClient";
import type { ServiceClient, ServiceState } from "./serviceClient";
import { isJsonRecord } from "./usagiParsers";

function parseState(value: unknown): ServiceState {
  if (!isJsonRecord(value) || (value.state !== "running" && value.state !== "stopped")) {
    throw new UsagiClientError("HTTP_ERROR", 200);
  }
  return value.state;
}

export function createMcpServiceClient(app: McpApp, onRuntimeUnavailable: () => void): ServiceClient {
  return {
    async getState(signal?: AbortSignal) {
      const result = await callMcpTool(app, "usagi.dashboard.read", { operation: "service_state" }, onRuntimeUnavailable, signal);
      return result.status === "not_running" ? "stopped" : parseState(result.payload);
    },
    async stop(signal?: AbortSignal) {
      const result = await callMcpTool(app, "usagi.dashboard.stop_service", {}, onRuntimeUnavailable, signal);
      if (result.status === "not_running") return "stopped";
      const state = parseState(result.payload);
      if (state === "stopped") onRuntimeUnavailable();
      return state;
    },
  };
}
