import { useCallback, useEffect, useMemo, useState } from "react";
import packageJson from "../../package.json";
import { useApp, useHostStyles } from "@modelcontextprotocol/ext-apps/react";

import { createMcpServiceClient } from "../data/mcpServiceClient";
import { callMcpTool, createMcpUsagiClient } from "../data/mcpUsagiClient";
import { DashboardPage } from "../dashboard/DashboardPage";

type ShellState = "connecting" | "probing" | "ready" | "not_running" | "bridge_error";

function McpDashboardShell({ retryBridge }: { retryBridge: () => void }) {
  const { app, isConnected, error } = useApp({
    appInfo: { name: "usagi-dashboard", version: packageJson.version },
    capabilities: { availableDisplayModes: ["fullscreen"] },
  });
  useHostStyles(app, app?.getHostContext());

  const [state, setState] = useState<ShellState>("connecting");
  const [probeAttempt, setProbeAttempt] = useState(0);
  const onRuntimeUnavailable = useCallback(() => setState("not_running"), []);
  const client = useMemo(
    () => app ? createMcpUsagiClient(app, onRuntimeUnavailable) : null,
    [app, onRuntimeUnavailable],
  );
  const serviceClient = useMemo(
    () => app ? createMcpServiceClient(app, onRuntimeUnavailable) : null,
    [app, onRuntimeUnavailable],
  );

  useEffect(() => {
    if (!app) return;
    const syncTheme = ({ theme }: { theme?: string }) => {
      if (theme === undefined) return;
      document.documentElement.classList.toggle("dark", theme === "dark");
    };
    syncTheme(app.getHostContext() ?? {});
    app.addEventListener("hostcontextchanged", syncTheme);
    return () => app.removeEventListener("hostcontextchanged", syncTheme);
  }, [app]);

  useEffect(() => {
    if (error) {
      setState("bridge_error");
      return;
    }
    if (!app || !isConnected) {
      setState("connecting");
      return;
    }

    let active = true;
    setState("probing");
    void callMcpTool(app, "usagi.dashboard.read", { operation: "probe" }, onRuntimeUnavailable).then(
      (result) => {
        if (active) setState(result.status === "ready" ? "ready" : "not_running");
      },
      () => {
        if (active) setState("bridge_error");
      },
    );
    return () => {
      active = false;
    };
  }, [app, error, isConnected, onRuntimeUnavailable, probeAttempt]);

  const retry = () => {
    if (!app || error) {
      retryBridge();
      return;
    }
    setProbeAttempt((attempt) => attempt + 1);
  };

  if (state === "ready" && client && serviceClient) {
    return (
      <DashboardPage
        options={{
          client,
          serviceClient,
          revisionMode: "poll",
          pollIntervalMs: 5000,
          showThemeToggle: false,
        }}
      />
    );
  }

  if (state === "not_running") {
    return (
      <main role="status" className="flex min-h-screen flex-col items-center justify-center gap-3 p-6 text-center">
        <h1>Usagi 未启动</h1>
        <p>请先启动 Usagi，然后重新检测。</p>
        <button type="button" onClick={retry}>重新检测</button>
      </main>
    );
  }

  if (state === "bridge_error") {
    return (
      <main role="alert" className="flex min-h-screen flex-col items-center justify-center gap-3 p-6 text-center">
        <p>无法连接到 Usagi。</p>
        <button type="button" onClick={retry}>重试</button>
      </main>
    );
  }

  return (
    <main role="status" className="flex min-h-screen items-center justify-center p-6 text-center">
      {state === "probing" ? "正在检测 Usagi…" : "正在连接 Usagi…"}
    </main>
  );
}

export function McpDashboardApp() {
  const [bridgeAttempt, setBridgeAttempt] = useState(0);
  const retryBridge = useCallback(() => setBridgeAttempt((attempt) => attempt + 1), []);

  return <McpDashboardShell key={bridgeAttempt} retryBridge={retryBridge} />;
}
