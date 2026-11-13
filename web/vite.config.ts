/// <reference types="vitest/config" />
import { fileURLToPath, URL } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import { bootScript } from "@uwusuite/design";
import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vite";

// `pnpm dev` talks to a local server's reverse-proxy port (see docs/development.md).
const server = process.env.UWUMAIL_DEV_SERVER ?? "http://127.0.0.1:18080";

/**
 * Theme, contrast and motion go onto <html> before the first paint, so a dark page never flashes
 * white. As the file /boot.js, not inline, because the server's CSP allows only script-src 'self'.
 * The key is STORAGE_KEY in src/state/prefs.ts.
 */
function boot(): Plugin {
  const source = bootScript("uwumail-portal-prefs");
  return {
    name: "uwumail-boot",
    configureServer(dev) {
      dev.middlewares.use("/boot.js", (_request, response) => {
        response.setHeader("content-type", "text/javascript");
        response.end(source);
      });
    },
    generateBundle() {
      this.emitFile({ type: "asset", fileName: "boot.js", source });
    },
  };
}

export default defineConfig({
  plugins: [react(), tailwindcss(), boot()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  server: {
    port: 5173,
    proxy: {
      "/api": { target: server, changeOrigin: false },
      "/.well-known/jmap": { target: server, changeOrigin: false },
    },
  },
  build: {
    target: "es2022",
    // The server embeds every file; keep the build small and without source maps.
    sourcemap: false,
    assetsInlineLimit: 0,
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
