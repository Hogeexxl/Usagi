import { UsagiClientError } from "./types";

export type ServiceState = "running" | "stopped";

export type ServiceClient = {
  getState(signal?: AbortSignal): Promise<ServiceState>;
  stop(signal?: AbortSignal): Promise<ServiceState>;
};

function parseState(value: unknown, status: number): ServiceState {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new UsagiClientError("HTTP_ERROR", status);
  }
  const state = (value as Record<string, unknown>).state;
  if (state !== "running" && state !== "stopped") {
    throw new UsagiClientError("HTTP_ERROR", status);
  }
  return state;
}

async function request(path: string, method: "GET" | "POST", signal?: AbortSignal): Promise<ServiceState> {
  let response: Response;
  try {
    response = await fetch(path, {
      method,
      signal,
      headers: method === "POST" ? { Accept: "application/json", "X-Usagi-Request": "1" } : { Accept: "application/json" },
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") throw error;
    throw new UsagiClientError("HTTP_ERROR", 0);
  }
  if (!response.ok) throw new UsagiClientError("HTTP_ERROR", response.status);
  try {
    return parseState(await response.json(), response.status);
  } catch (error) {
    if (error instanceof UsagiClientError) throw error;
    throw new UsagiClientError("HTTP_ERROR", response.status);
  }
}

export const serviceClient: ServiceClient = {
  getState: (signal) => request("/api/service", "GET", signal),
  stop: (signal) => request("/api/service/stop", "POST", signal),
};
