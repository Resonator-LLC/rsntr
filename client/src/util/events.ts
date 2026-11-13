// A minimal typed event emitter, the base of every observable in the client.
//
// Stands in for the `Observable*` listener interfaces and `CopyOnWriteSet`
// listener sets of Apache Wave (`org.waveprotocol.wave.model.util`), licensed
// under the Apache License, Version 2.0: listeners may be added or removed
// while an event is being delivered; the change takes effect for the next
// event.

/** Maps event names to the payload type delivered to their listeners. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type EventMap = Record<string, any>;

export type Listener<T> = (event: T) => void;

/** Removes the listener it was returned for. Safe to call more than once. */
export type Unsubscribe = () => void;

/** The subscribing half of an {@link Emitter}, for public APIs. */
export interface Observable<E extends EventMap> {
  on<K extends keyof E>(type: K, listener: Listener<E[K]>): Unsubscribe;
  off<K extends keyof E>(type: K, listener: Listener<E[K]>): void;
  /** Subscribes for one delivery. */
  once<K extends keyof E>(type: K, listener: Listener<E[K]>): Unsubscribe;
}

/**
 * Synchronous typed event emitter.
 *
 * Listeners are called in subscription order. A listener that throws does
 * not stop delivery to the others: the first error is rethrown after all
 * listeners ran, unless an `onError` handler was given, which then receives
 * every error.
 */
export class Emitter<E extends EventMap> implements Observable<E> {
  private readonly listeners = new Map<keyof E, Listener<never>[]>();

  constructor(private readonly onError?: (error: unknown) => void) {}

  on<K extends keyof E>(type: K, listener: Listener<E[K]>): Unsubscribe {
    const current = this.listeners.get(type) ?? [];
    // Copy on write, so that a delivery in progress is not disturbed.
    this.listeners.set(type, [...current, listener as Listener<never>]);
    return () => this.off(type, listener);
  }

  off<K extends keyof E>(type: K, listener: Listener<E[K]>): void {
    const current = this.listeners.get(type);
    if (!current) {
      return;
    }
    const index = current.indexOf(listener as Listener<never>);
    if (index < 0) {
      return;
    }
    if (current.length === 1) {
      this.listeners.delete(type);
    } else {
      this.listeners.set(type, [...current.slice(0, index), ...current.slice(index + 1)]);
    }
  }

  once<K extends keyof E>(type: K, listener: Listener<E[K]>): Unsubscribe {
    const wrapper: Listener<E[K]> = (event) => {
      this.off(type, wrapper);
      listener(event);
    };
    return this.on(type, wrapper);
  }

  emit<K extends keyof E>(type: K, event: E[K]): void {
    const current = this.listeners.get(type);
    if (!current) {
      return;
    }
    let failed = false;
    let firstError: unknown;
    for (const listener of current) {
      try {
        (listener as Listener<E[K]>)(event);
      } catch (error) {
        if (this.onError) {
          this.onError(error);
        } else if (!failed) {
          failed = true;
          firstError = error;
        }
      }
    }
    if (failed) {
      throw firstError;
    }
  }

  listenerCount<K extends keyof E>(type: K): number {
    return this.listeners.get(type)?.length ?? 0;
  }

  /** Drops every listener. */
  clear(): void {
    this.listeners.clear();
  }
}
