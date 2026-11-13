// The servers behind the application end-to-end tests: the real resonator
// binary with a fresh database, and in front of it a small static server
// for the built client (`dist/`) that proxies the API paths and the
// WebSocket. The static server sends the same Content-Security-Policy as
// resonator does, so the tests also prove that the build obeys it.

import { type ChildProcess, spawn } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import http, { type IncomingMessage, type ServerResponse } from "node:http";
import net, { type AddressInfo } from "node:net";
import type { Duplex } from "node:stream";
import { tmpdir } from "node:os";
import { extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const CLIENT_ROOT = resolve(HERE, "../../..");
export const DIST = join(CLIENT_ROOT, "dist");
export const BINARY =
  process.env.RESONATOR_BINARY ??
  resolve(CLIENT_ROOT, "../target/release/resonator");
/**
 * The binary for person mode, which needs federation: `resonator` (the
 * node), where {@link BINARY} may be `resonator-server` (no federation,
 * but the end-to-end hooks).
 */
export const NODE_BINARY =
  process.env.RESONATOR_NODE_BINARY ??
  resolve(CLIENT_ROOT, "../target/release/resonator");
export const DOMAIN = "example.com";

export const CSP =
  "default-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'";

const API_PREFIXES = [
  "/auth",
  "/session",
  "/socket",
  "/fetch",
  "/search",
  "/profile",
  "/directory",
  "/contacts",
  "/iniavatars",
  "/locale",
  "/attachment",
  "/thumbnail",
  "/attachmentsInfo",
  "/waveref",
  "/healthz",
  "/person",
  "/waves/host-status",
  "/waves/continue",
];

const MIME: Record<string, string> = {
  ".html": "text/html;charset=utf-8",
  ".js": "text/javascript;charset=utf-8",
  ".css": "text/css;charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".json": "application/json",
  ".map": "application/json",
  ".webmanifest": "application/manifest+json",
};

function freePort(): Promise<number> {
  return new Promise((done, fail) => {
    const server = net.createServer();
    server.once("error", fail);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address() as AddressInfo;
      server.close(() => done(port));
    });
  });
}

function isApi(path: string, url: string): boolean {
  // Device mode: `/?token=` is the server's (it signs in and redirects).
  if (path === "/" && /[?&]token=/.test(url)) {
    return true;
  }
  return API_PREFIXES.some(
    (p) => path === p || path.startsWith(`${p}/`) || path.startsWith(`${p}?`),
  );
}

/**
 * A pkarr relay in miniature (what pairing codes are published on):
 * `PUT /pkarr/<key>` keeps the signed packet, `GET /pkarr/<key>` gives it
 * back, or 404. Signatures are checked by the node, not here.
 */
async function startPkarrRelay(): Promise<http.Server> {
  const packets = new Map<string, Buffer>();
  const relay = http.createServer((req, res) => {
    const key = req.url ?? "";
    if (req.method === "PUT") {
      const chunks: Buffer[] = [];
      req.on("data", (chunk: Buffer) => chunks.push(chunk));
      req.on("end", () => {
        packets.set(key, Buffer.concat(chunks));
        res.writeHead(204).end();
      });
      return;
    }
    const packet = packets.get(key);
    if (req.method !== "GET" || packet === undefined) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { "content-type": "application/pkarr.org/relays#payload" }).end(packet);
  });
  await new Promise<void>((done) => relay.listen(0, "127.0.0.1", done));
  return relay;
}

export class TestServers {
  private constructor(
    /** The origin the browser uses: the static server. */
    readonly baseUrl: string,
    /** The resonator server itself. */
    readonly backendUrl: string,
    private readonly child: ChildProcess,
    private readonly front: http.Server,
    private readonly dir: string,
    private readonly sockets: Set<net.Socket>,
    private readonly uploads: Uploads,
    /**
     * Device mode: the page that signs the device's account in, through
     * the static server (`<baseUrl>/?token=...`); null otherwise.
     */
    readonly deviceUrl: string | null = null,
    /** Person mode: the pkarr relay of pairing codes (in memory). */
    private readonly relay: http.Server | null = null,
  ) {}

  /**
   * Starts the servers. `hooks` turns on the server's end-to-end test
   * hooks (`POST /e2e/...`, on `backendUrl`). `device` runs the server in
   * device mode (`--mode device`: one account, `me@example.com`, signed
   * in by {@link deviceUrl}, contacts instead of the directory).
   */
  static async start(
    options: { hooks?: boolean; device?: boolean; person?: boolean } = {},
  ): Promise<TestServers> {
    const person = options.device === true && options.person === true;
    const binary = person ? NODE_BINARY : BINARY;
    if (!existsSync(binary)) {
      throw new Error(
        `no server binary at ${binary}: cargo build --release -p resonator`,
      );
    }
    if (!existsSync(join(DIST, "index.html"))) {
      throw new Error(`no built client at ${DIST}: npm run build`);
    }
    const dir = mkdtempSync(
      join(process.env.RESONATOR_TEST_TMP ?? tmpdir(), "resonator-e2e-"),
    );
    const backendPort = await freePort();
    const env: NodeJS.ProcessEnv = {};
    for (const [key, value] of Object.entries(process.env)) {
      if (!key.startsWith("RESONATOR_")) {
        env[key] = value;
      }
    }
    if (options.hooks === true) {
      env.RESONATOR_E2E_HOOKS = "1";
    }
    // Person mode (with `device`): the device is the first, admin device
    // of a new person, which federation needs; nothing reaches other
    // machines (no relay, no discovery, loopback only). Pairing codes go
    // to a pkarr relay in miniature here.
    let relay: http.Server | null = null;
    if (person) {
      relay = await startPkarrRelay();
      const relayPort = (relay.address() as AddressInfo).port;
      writeFileSync(
        join(dir, "resonator.toml"),
        [
          'mode = "device"',
          "[federation]",
          "enabled = true",
          'relay = "off"',
          'discovery = "off"',
          'listen = "127.0.0.1:0"',
          `pairing_rendezvous = "http://127.0.0.1:${relayPort}/pkarr"`,
          "[person]",
          "enabled = true",
          "admin = true",
          'device_name = "e2e phone"',
          "",
        ].join("\n"),
      );
    }
    const child = spawn(
      binary,
      [
        ...(person ? ["--config", join(dir, "resonator.toml")] : ["--domain", DOMAIN]),
        "--listen",
        `127.0.0.1:${backendPort}`,
        "--database-path",
        join(dir, "resonator.db"),
        "--register-max-per-hour",
        "0",
        "--login-max-failures",
        "0",
        ...(options.device === true ? ["--mode", "device"] : []),
        "serve",
      ],
      { cwd: dir, env, stdio: ["ignore", "ignore", "pipe"] },
    );
    let stderr = "";
    child.stderr?.on("data", (chunk: Buffer) => {
      stderr = (stderr + chunk.toString()).slice(-4000);
    });
    const backendUrl = `http://127.0.0.1:${backendPort}`;
    const deadline = Date.now() + 20_000;
    for (;;) {
      if (child.exitCode !== null) {
        throw new Error(`resonator exited with ${child.exitCode}: ${stderr}`);
      }
      try {
        if ((await fetch(`${backendUrl}/healthz`)).ok) {
          break;
        }
      } catch {
        // not up yet
      }
      if (Date.now() > deadline) {
        child.kill("SIGKILL");
        throw new Error(`resonator did not start: ${stderr}`);
      }
      await new Promise((r) => setTimeout(r, 30));
    }

    // Device mode: the sign-in URL (with its token) is in a file the
    // server writes, readable by its owner only.
    let token: string | null = null;
    if (options.device === true) {
      const url = new URL(readFileSync(join(dir, "device.url"), "utf8").trim());
      token = url.searchParams.get("token");
    }

    const sockets = new Set<net.Socket>();
    const uploads: Uploads = { held: false, all: [] };
    const front = http.createServer((req, res) => serve(req, res, backendPort));
    front.on("connection", (socket) => {
      sockets.add(socket);
      socket.on("close", () => sockets.delete(socket));
    });
    front.on("upgrade", (req, socket, head) => {
      // The WebSocket: replay the request to the backend and splice the streams.
      const upstream = net.connect(backendPort, "127.0.0.1", () => {
        let raw = `${req.method} ${req.url} HTTP/1.1\r\n`;
        for (let i = 0; i < req.rawHeaders.length; i += 2) {
          raw += `${req.rawHeaders[i]}: ${req.rawHeaders[i + 1]}\r\n`;
        }
        upstream.write(`${raw}\r\n`);
        if (head.length > 0) {
          upstream.write(head);
        }
        const upload = new Upload(socket, upstream, uploads.held);
        uploads.all.push(upload);
        upstream.on("close", () =>
          uploads.all.splice(uploads.all.indexOf(upload), 1),
        );
        upstream.pipe(socket);
      });
      sockets.add(upstream);
      upstream.on("close", () => sockets.delete(upstream));
      const end = (): void => {
        upstream.destroy();
        socket.destroy();
      };
      upstream.on("error", end);
      socket.on("error", end);
    });
    await new Promise<void>((done) => front.listen(0, "127.0.0.1", done));
    const { port } = front.address() as AddressInfo;
    return new TestServers(
      `http://127.0.0.1:${port}`,
      backendUrl,
      child,
      front,
      dir,
      sockets,
      uploads,
      token === null ? null : `http://127.0.0.1:${port}/?token=${encodeURIComponent(token)}`,
      relay,
    );
  }

  /**
   * Holds back what browsers send over their WebSockets (the server's
   * messages still arrive) until `releaseUploads`.
   */
  holdUploads(): void {
    this.uploads.held = true;
    for (const upload of this.uploads.all) {
      upload.hold();
    }
  }

  releaseUploads(): void {
    this.uploads.held = false;
    for (const upload of this.uploads.all) {
      upload.release();
    }
  }

  /** Cuts every connection through the front server (WebSockets included). */
  dropConnections(): void {
    for (const socket of this.sockets) {
      socket.destroy();
    }
  }

  async stop(): Promise<void> {
    this.dropConnections();
    await new Promise<void>((done) => this.front.close(() => done()));
    if (this.relay !== null) {
      this.relay.closeAllConnections();
      await new Promise<void>((done) => this.relay!.close(() => done()));
    }
    if (this.child.exitCode === null) {
      this.child.kill("SIGTERM");
      await new Promise<void>((done) => {
        const timer = setTimeout(() => {
          this.child.kill("SIGKILL");
          done();
        }, 5_000);
        this.child.once("exit", () => {
          clearTimeout(timer);
          done();
        });
      });
    }
    rmSync(this.dir, { recursive: true, force: true });
  }
}

/** The browser-to-server direction of a proxied WebSocket. */
interface Uploads {
  held: boolean;
  readonly all: Upload[];
}

class Upload {
  constructor(
    private readonly from: Duplex,
    private readonly to: Duplex,
    held: boolean,
  ) {
    if (held) {
      from.pause();
    } else {
      from.pipe(to);
    }
  }

  hold(): void {
    this.from.unpipe(this.to);
    this.from.pause();
  }

  release(): void {
    this.from.pipe(this.to);
    this.from.resume();
  }
}

function serve(
  req: IncomingMessage,
  res: ServerResponse,
  backendPort: number,
): void {
  const url = req.url ?? "/";
  const path = url.split("?")[0] ?? "/";
  if (isApi(path, url)) {
    const upstream = http.request(
      {
        host: "127.0.0.1",
        port: backendPort,
        method: req.method,
        path: url,
        headers: req.headers,
      },
      (answer) => {
        res.writeHead(answer.statusCode ?? 502, answer.headers);
        answer.pipe(res);
      },
    );
    upstream.on("error", () => {
      res.writeHead(502).end();
    });
    req.pipe(upstream);
    return;
  }
  let file = normalize(join(DIST, decodeURIComponent(path)));
  if (!file.startsWith(DIST)) {
    res.writeHead(403).end();
    return;
  }
  if (path === "/" || !existsSync(file) || !statSync(file).isFile()) {
    if (path !== "/" && extname(path) !== "") {
      res.writeHead(404).end();
      return;
    }
    file = join(DIST, "index.html");
  }
  res.writeHead(200, {
    "content-type": MIME[extname(file)] ?? "application/octet-stream",
    "content-security-policy": CSP,
    "x-content-type-options": "nosniff",
    "cache-control": "no-store",
  });
  res.end(readFileSync(file));
}
