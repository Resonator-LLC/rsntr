// Starts the Vite dev server on an ephemeral port of 127.0.0.1 for the
// editor harness (e2e/editor/harness/).

import { createServer, type ViteDevServer } from "vite";

export async function startVite(): Promise<{ url: string; close: () => Promise<void> }> {
  const server: ViteDevServer = await createServer({
    configFile: new URL("../../vite.config.ts", import.meta.url).pathname,
    root: new URL("../..", import.meta.url).pathname,
    server: { host: "127.0.0.1", port: 0, strictPort: false, hmr: false },
    logLevel: "error",
  });
  await server.listen();
  const address = server.httpServer?.address();
  if (address === null || address === undefined || typeof address === "string") {
    throw new Error("vite did not listen");
  }
  return { url: `http://127.0.0.1:${address.port}`, close: () => server.close() };
}
