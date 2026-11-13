// Runs the real `resonator` server for the integration tests: one process
// per test file, on an ephemeral port of 127.0.0.1, with a database in a
// temporary directory, killed afterwards.
//
// The binary is built with `cargo build --release -p resonator` in the
// repository root. Without it the tests are skipped (or fail, when
// `RESONATOR_INTEGRATION=require`, as `npm run test:integration` sets).

import { type ChildProcess, spawn } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { WaveClient } from "../../client";
import { withCookieJar } from "../../net";

export const DOMAIN = "example.com";

export const BINARY =
  process.env.RESONATOR_BIN ??
  fileURLToPath(new URL("../../../../target/release/resonator", import.meta.url));

export const BINARY_PRESENT = existsSync(BINARY);

export const REQUIRED = process.env.RESONATOR_INTEGRATION === "require";

if (!BINARY_PRESENT) {
  const message =
    `resonator binary not found at ${BINARY}: integration tests skipped. ` +
    "Build it with `cargo build --release -p resonator` in the repository root.";
  if (REQUIRED) {
    throw new Error(message);
  }
  console.warn(message);
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address !== null ? address.port : 0;
      server.close(() => resolve(port));
    });
  });
}

export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Polls until `condition` holds; fails with `what` after the timeout. */
export async function waitFor(
  what: string,
  condition: () => boolean | Promise<boolean>,
  timeoutMs = 10_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (await condition()) {
      return;
    }
    if (Date.now() > deadline) {
      throw new Error(`timed out waiting for ${what}`);
    }
    await sleep(20);
  }
}

export class TestServer {
  private constructor(
    readonly baseUrl: string,
    private readonly child: ChildProcess,
    private readonly dir: string,
    private readonly clients: WaveClient[] = [],
  ) {}

  static async start(): Promise<TestServer> {
    const dir = mkdtempSync(join(process.env.RESONATOR_TEST_TMP ?? tmpdir(), "resonator-it-"));
    const port = await freePort();
    const env: NodeJS.ProcessEnv = {};
    for (const [key, value] of Object.entries(process.env)) {
      // The tests must not pick up settings of the environment they run in.
      if (!key.startsWith("RESONATOR_")) {
        env[key] = value;
      }
    }
    const child = spawn(
      BINARY,
      [
        "--domain",
        DOMAIN,
        "--listen",
        `127.0.0.1:${port}`,
        "--database-path",
        join(dir, "resonator.db"),
        "--register-max-per-hour",
        "0",
        "--login-max-failures",
        "0",
        "serve",
      ],
      { cwd: dir, env, stdio: ["ignore", "ignore", "pipe"] },
    );
    let stderr = "";
    child.stderr?.on("data", (chunk: Buffer) => {
      stderr = (stderr + chunk.toString()).slice(-4000);
    });
    const baseUrl = `http://127.0.0.1:${port}`;
    const server = new TestServer(baseUrl, child, dir);
    try {
      await waitFor(
        "the server to start",
        async () => {
          if (child.exitCode !== null) {
            throw new Error(`resonator exited with ${child.exitCode}: ${stderr}`);
          }
          try {
            return (await fetch(`${baseUrl}/healthz`)).ok;
          } catch {
            return false;
          }
        },
        20_000,
      );
    } catch (error) {
      await server.stop();
      throw error;
    }
    return server;
  }

  /** A client with its own cookie jar (its own browser, so to speak). */
  client(): WaveClient {
    const client = new WaveClient({
      baseUrl: this.baseUrl,
      fetch: withCookieJar(fetch),
      backoff: { initialMs: 50, maxMs: 500 },
    });
    this.clients.push(client);
    return client;
  }

  /** A client signed in as `name`, registered first if need be; connected. */
  async user(name: string, register = true): Promise<WaveClient> {
    const client = this.client();
    const address = `${name}@${DOMAIN}`;
    if (register) {
      await client.register(address, `password-${name}`);
    }
    await client.signIn(address, `password-${name}`);
    await client.whenConnected();
    return client;
  }

  async stop(): Promise<void> {
    for (const client of this.clients) {
      client.dispose();
    }
    if (this.child.exitCode === null) {
      const exited = new Promise<void>((resolve) => this.child.once("exit", () => resolve()));
      this.child.kill("SIGTERM");
      const timer = setTimeout(() => this.child.kill("SIGKILL"), 5000);
      await exited;
      clearTimeout(timer);
    }
    rmSync(this.dir, { recursive: true, force: true });
  }
}
