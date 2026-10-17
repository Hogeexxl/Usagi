import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";
import { viteSingleFile } from "vite-plugin-singlefile";

export default defineConfig({
  plugins: [react(), tailwindcss(), viteSingleFile()],
  publicDir: false,
  build: {
    outDir: "dist-codex",
    emptyOutDir: true,
    sourcemap: false,
    rollupOptions: {
      input: "usagi-dashboard.html",
    },
  },
});
