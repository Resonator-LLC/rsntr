import preact from "@preact/preset-vite";
import { defineConfig } from "vitest/config";

// The wave server the dev server and the preview proxy to.
const backend = process.env.RESONATOR_BACKEND ?? "http://127.0.0.1:9898";

// Every path of the server the client talks to; `/` itself is the client.
const API_PATHS = [
  "/auth",
  "/session",
  "/fetch",
  "/search",
  "/profile",
  "/directory",
  "/iniavatars",
  "/locale",
  "/attachment",
  "/thumbnail",
  "/attachmentsInfo",
  "/waveref",
  "/healthz",
  "/contacts",
  "/person",
  "/waves/host-status",
  "/waves/continue",
];

const proxy: Record<string, object> = {
  "/socket": { target: backend, ws: true, changeOrigin: false },
};
for (const path of API_PATHS) {
  proxy[path] = { target: backend, changeOrigin: false };
}

export default defineConfig({
  plugins: [preact()],
  server: { host: "0.0.0.0", proxy },
  preview: { host: "0.0.0.0", proxy },
  build: {
    target: "es2022",
    // The server's CSP is `default-src 'self'`: no inlined scripts or data: URLs.
    assetsInlineLimit: 0,
    modulePreload: { polyfill: false },
  },
  test: {
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    testTimeout: 120_000,
  },
});
