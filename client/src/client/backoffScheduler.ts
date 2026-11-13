// The reconnection scheduler of the operation channel multiplexer: runs
// the first task at once (asynchronously) and each further one after a
// growing, jittered delay, until reset.
//
// Stands in for the `FuzzingBackOffScheduler` the Apache Wave web client
// gives its multiplexer (`org.waveprotocol.wave.model.util`), licensed under
// the Apache License, Version 2.0.

import type { Scheduler, SchedulerCommand } from "../concurrencycontrol/channel";

/** `setTimeout`/`clearTimeout`, injectable for tests. */
export interface Timers {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

export const DEFAULT_TIMERS: Timers = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export interface BackoffSchedulerOptions {
  timers?: Timers;
  random?: () => number;
  /** Delay of the second attempt in a row. Default 500. */
  initialMs?: number;
  /** Largest delay. Default 10000. */
  maxMs?: number;
  /** Growth per attempt. Default 2. */
  factor?: number;
  /** Random spread as a fraction of the delay. Default 0.3. */
  jitter?: number;
}

export class BackoffScheduler implements Scheduler {
  private readonly timers: Timers;
  private readonly random: () => number;
  private readonly initialMs: number;
  private readonly maxMs: number;
  private readonly factor: number;
  private readonly jitter: number;
  /** Tasks scheduled since the last reset. */
  private attempts = 0;
  private readonly pending = new Set<unknown>();
  private cancelled = false;

  constructor(options: BackoffSchedulerOptions = {}) {
    this.timers = options.timers ?? DEFAULT_TIMERS;
    this.random = options.random ?? Math.random;
    this.initialMs = options.initialMs ?? 500;
    this.maxMs = options.maxMs ?? 10_000;
    this.factor = options.factor ?? 2;
    this.jitter = options.jitter ?? 0.3;
  }

  /** The next task runs without delay again. */
  reset(): void {
    this.attempts = 0;
  }

  /** The delay the next task would get, before jitter. */
  nextDelayMs(): number {
    if (this.attempts === 0) {
      return 0;
    }
    return Math.min(this.maxMs, this.initialMs * this.factor ** (this.attempts - 1));
  }

  schedule(task: SchedulerCommand): boolean {
    if (this.cancelled) {
      return false;
    }
    const base = this.nextDelayMs();
    const delay = Math.max(0, Math.round(base * (1 + this.jitter * (2 * this.random() - 1))));
    this.attempts += 1;
    const handle: { value: unknown } = { value: undefined };
    handle.value = this.timers.setTimeout(() => {
      this.pending.delete(handle);
      if (!this.cancelled) {
        task.execute();
      }
    }, delay);
    this.pending.add(handle);
    return true;
  }

  /** Drops every scheduled task and refuses new ones. */
  cancel(): void {
    this.cancelled = true;
    for (const handle of this.pending) {
      this.timers.clearTimeout((handle as { value: unknown }).value);
    }
    this.pending.clear();
  }
}
