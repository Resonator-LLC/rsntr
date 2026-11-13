// Test doubles for the RPC channel: a socket, timers and a network status
// that the test drives by hand.

import type { NetworkStatus, SocketLike, TimerApi } from "..";

export class FakeSocket implements SocketLike {
  readonly sent: string[] = [];
  closeCalls = 0;
  /** Makes `send` throw. */
  failSend = false;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: ((event: { code: number; reason: string }) => void) | null = null;
  onerror: ((event: unknown) => void) | null = null;

  constructor(readonly url: string) {}

  send(data: string): void {
    if (this.failSend) {
      throw new Error("socket is not open");
    }
    this.sent.push(data);
  }

  close(): void {
    this.closeCalls += 1;
  }

  /** The server accepted the connection. */
  open(): void {
    this.onopen?.();
  }

  /** A frame from the server. */
  receive(data: unknown): void {
    this.onmessage?.({ data });
  }

  /** The connection ended, by the server or the network. */
  serverClose(code = 1006, reason = ""): void {
    this.onclose?.({ code, reason });
  }

  error(): void {
    this.onerror?.(new Error("socket error"));
  }

  /** Whether the channel still listens to this socket. */
  get attached(): boolean {
    return this.onmessage !== null;
  }
}

interface Task {
  readonly id: number;
  readonly at: number;
  readonly callback: () => void;
}

export class FakeTimers implements TimerApi {
  now = 0;
  private nextId = 1;
  private tasks: Task[] = [];

  setTimeout(callback: () => void, delayMs: number): unknown {
    const id = this.nextId++;
    this.tasks.push({ id, at: this.now + delayMs, callback });
    return id;
  }

  clearTimeout(handle: unknown): void {
    this.tasks = this.tasks.filter((task) => task.id !== handle);
  }

  /** The number of timers that are set. */
  get pending(): number {
    return this.tasks.length;
  }

  /** The delays, from now, of the timers that are set, in order. */
  get delays(): number[] {
    return this.tasks.map((task) => task.at - this.now).sort((a, b) => a - b);
  }

  /** Moves the clock, running the timers that come due, in order. */
  advance(ms: number): void {
    const end = this.now + ms;
    for (;;) {
      const due = this.tasks.filter((task) => task.at <= end).sort((a, b) => a.at - b.at)[0];
      if (due === undefined) {
        break;
      }
      this.tasks = this.tasks.filter((task) => task !== due);
      this.now = due.at;
      due.callback();
    }
    this.now = end;
  }
}

export class FakeNetwork implements NetworkStatus {
  online = true;
  private listeners: ((online: boolean) => void)[] = [];

  isOnline(): boolean {
    return this.online;
  }

  subscribe(listener: (online: boolean) => void): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  get listenerCount(): number {
    return this.listeners.length;
  }

  set(online: boolean): void {
    this.online = online;
    for (const listener of [...this.listeners]) {
      listener(online);
    }
  }
}

/** Lets pending promise reactions run. */
export async function flush(): Promise<void> {
  for (let i = 0; i < 20; i++) {
    await Promise.resolve();
  }
}
