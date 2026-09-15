/// <reference types="vitest/config" />
import { fileURLToPath, URL } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// `pnpm dev` talks to a local server's reverse-proxy port (see docs/development.md).
const server = process.env.UWUMAIL_DEV_SERVER ?? "http://127.0.0.1:18080";

export default defineConfig({
  plugins: [react(), tailwindcss()],
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
