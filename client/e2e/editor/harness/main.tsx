// A page with two editors over one simulated collaborative document: each
// editor has its own copy, and a tiny OT server (one operation in flight per
// client, the rest buffered and composed, as the real concurrency control)
// relays operations with an adjustable delay. Driven by e2e/editor/*.spec.ts
// through `window.harness`.

import "../../../src/ui/styles/tokens.css";
import "../../../src/ui/styles/base.css";
import "../../../src/ui/styles/editor.css";

import { render } from "preact";

import { MutableDocument } from "../../../src/model/doc";
import { type DocOp, compose, transform } from "../../../src/model/docop";
import { EditToolbar, createBlipView } from "../../../src/ui/editor";
import type { BlipView } from "../../../src/ui/editor/api";
import { type FakeDocument, createFakeDocument } from "../../../src/ui/editor/fakeDocument";

const initial = new URLSearchParams(location.search).get("xml") ?? "<body><line/></body>";

interface Message {
  readonly op: DocOp;
  readonly from: number;
}

class Server {
  readonly doc: FakeDocument = createFakeDocument(initial);
  readonly history: Message[] = [];
  readonly clients: Client[] = [];
  receive(op: DocOp, base: number, from: number): void {
    let o = op;
    for (let v = base; v < this.history.length; v++) {
      o = transform(o, this.history[v]!.op).clientOp;
    }
    this.doc.applyRemote(o);
    const message = { op: o, from };
    this.history.push(message);
    for (const client of this.clients) {
      client.deliver(message);
    }
  }
}

class Client {
  readonly doc: FakeDocument = createFakeDocument(initial);
  private inflight: DocOp | null = null;
  private buffer: DocOp | null = null;
  private version = 0;
  private applyingRemote = false;
  private readonly toServer: { op: DocOp; base: number }[] = [];
  private readonly fromServer: Message[] = [];
  paused = false;
  delay = 0;
  view!: BlipView;

  constructor(
    private readonly server: Server,
    readonly index: number,
  ) {
    this.doc.onOp((e) => {
      if (!this.applyingRemote && e.local) {
        this.local(e.op);
      }
    });
  }

  private local(op: DocOp): void {
    if (this.inflight === null) {
      this.send(op);
    } else {
      this.buffer = this.buffer === null ? op : compose(this.buffer, op);
    }
  }

  private send(op: DocOp): void {
    this.inflight = op;
    this.toServer.push({ op, base: this.version });
    this.pump();
  }

  deliver(message: Message): void {
    this.fromServer.push(message);
    this.pump();
  }

  private scheduled = false;
  pump(): void {
    if (this.paused || this.scheduled) {
      return;
    }
    this.scheduled = true;
    setTimeout(() => {
      this.scheduled = false;
      if (this.paused) {
        return;
      }
      const up = this.toServer.shift();
      if (up !== undefined) {
        this.server.receive(up.op, up.base, this.index);
      }
      const down = this.fromServer.shift();
      if (down !== undefined) {
        this.receive(down);
      }
      if (this.toServer.length > 0 || this.fromServer.length > 0) {
        this.pump();
      }
    }, this.delay);
  }

  private receive(message: Message): void {
    this.version++;
    if (message.from === this.index) {
      this.inflight = null;
      if (this.buffer !== null) {
        const b = this.buffer;
        this.buffer = null;
        this.send(b);
      }
      return;
    }
    let s = message.op;
    if (this.inflight !== null) {
      const p = transform(this.inflight, s);
      this.inflight = p.clientOp;
      s = p.serverOp;
    }
    if (this.buffer !== null) {
      const p = transform(this.buffer, s);
      this.buffer = p.clientOp;
      s = p.serverOp;
    }
    this.applyingRemote = true;
    try {
      this.doc.applyRemote(s);
    } finally {
      this.applyingRemote = false;
    }
  }

  idle(): boolean {
    return this.inflight === null && this.buffer === null && this.toServer.length === 0 && this.fromServer.length === 0;
  }
}

const server = new Server();
const clients = [new Client(server, 0), new Client(server, 1)];
server.clients.push(...clients);

const attachments = {
  attachmentUrl: (id: string) => `/attachment/${id}`,
  thumbnailUrl: (id: string) => `/thumbnail/${id}`,
  info: async () => null,
};

for (const [i, name] of ["a", "b"].entries()) {
  const client = clients[i]!;
  const view = createBlipView({
    host: document.getElementById(`editor-${name}`)!,
    doc: client.doc,
    attachments,
    ariaLabel: `Editor ${name.toUpperCase()}`,
  });
  view.setEditable(true);
  client.view = view;
  render(<EditToolbar view={view} />, document.getElementById(`toolbar-${name}`)!);
}

const harness = {
  clients,
  server,
  xml: (i: number) => clients[i]!.doc.xml(),
  serverXml: () => server.doc.xml(),
  idle: () => clients.every((c) => c.idle()),
  setDelay(ms: number) {
    for (const c of clients) {
      c.delay = ms;
    }
  },
  setPaused(i: number, paused: boolean) {
    clients[i]!.paused = paused;
    if (!paused) {
      clients[i]!.pump();
    }
  },
  /** A third party's edit, made at the server and broadcast. */
  serverEdit(edit: (doc: MutableDocument) => void) {
    let captured: DocOp | null = null;
    const sink = {
      document: () => server.doc.document(),
      consume: (op: DocOp) => {
        captured = op;
      },
    };
    edit(new MutableDocument(sink));
    if (captured !== null) {
      server.receive(captured, server.history.length, -1);
    }
  },
  insertAtServer(location: number, text: string) {
    harness.serverEdit((d) => d.insertText(location, text));
  },
  selection: (i: number) => clients[i]!.view.selection(),
  state: (i: number) => clients[i]!.view.state(),
  focus: (i: number, at?: number | "start" | "end") => clients[i]!.view.focus(at),
  setSelection: (i: number, start: number, end: number) => clients[i]!.view.setSelection({ start, end }),
  exec: (i: number, command: Parameters<BlipView["exec"]>[0]) => clients[i]!.view.exec(command),
  errors: [] as string[],
};
window.addEventListener("error", (e) => harness.errors.push(String(e.message)));
(window as unknown as { harness: typeof harness }).harness = harness;
document.body.dataset.ready = "true";
