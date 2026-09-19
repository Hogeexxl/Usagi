import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const BACKEND_DEFAULT_PORT = 3210;
const BACKEND_PORT_SCAN_COUNT = 32;
const BACKEND_PROBE_TIMEOUT_MS = 120;
const BACKEND_RECHECK_MS = 2_000;
let backendOrigin = `http://127.0.0.1:${BACKEND_DEFAULT_PORT}`;

async function isUsagiBackend(port: number): Promise<boolean> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), BACKEND_PROBE_TIMEOUT_MS);
  try {
    const response = await fetch(`http://127.0.0.1:${port}/api/health`, {
      method: "GET",
      signal: controller.signal,
    });
    return response.ok && response.headers.get("x-usagi-app") === "Usagi";
  } catch {
    return false;
  } finally {
    clearTimeout(timeout);
  }
}

async function discoverBackendOrigin(): Promise<string | null> {
  const currentPort = Number(new URL(backendOrigin).port);
  if (Number.isInteger(currentPort) && await isUsagiBackend(currentPort)) {
    return backendOrigin;
  }

  const ports = Array.from(
    { length: BACKEND_PORT_SCAN_COUNT },
    (_, index) => BACKEND_DEFAULT_PORT + index,
  );
  const matches = await Promise.all(ports.map((port) => isUsagiBackend(port)));
  const matchIndex = matches.findIndex(Boolean);
  return matchIndex >= 0 ? `http://127.0.0.1:${ports[matchIndex]}` : null;
}

export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (!id.includes("/node_modules/")) return undefined;
          if (
            id.includes("/node_modules/react/") ||
            id.includes("/node_modules/react-dom/") ||
            id.includes("/node_modules/scheduler/")
          ) {
            return "vendor-react";
          }
          if (id.includes("/node_modules/motion/")) return "vendor-motion";
          if (
            id.includes("/node_modules/@tanstack/react-virtual/") ||
            id.includes("/node_modules/@tanstack/virtual-core/")
          ) {
            return "vendor-virtual";
          }
          return undefined;
        },
      },
    },
  },
  server: {
    host: "localhost",
    port: 5173,
    strictPort: false,
    headers: {
      "X-Usagi-Frontend": "1",
    },
    proxy: {
      "/api": {
        target: backendOrigin,
        changeOrigin: true,
        configure: (proxy, options) => {
          let refreshInFlight = false;
          const refreshTarget = async () => {
            if (refreshInFlight) return;
            refreshInFlight = true;
            try {
              const discovered = await discoverBackendOrigin();
              if (discovered !== null) {
                backendOrigin = discovered;
                options.target = discovered;
              }
            } finally {
              refreshInFlight = false;
            }
          };

          void refreshTarget();
          const timer = setInterval(() => {
            void refreshTarget();
          }, BACKEND_RECHECK_MS);
          timer.unref();

          proxy.on("proxyReq", (proxyReq) => {
            proxyReq.setHeader("Origin", backendOrigin);
          });
        },
      },
    }
  }
});
