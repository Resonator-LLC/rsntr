// Decides when the document of a blip gets rendered: at once when the blip
// comes near the viewport, otherwise in idle time, in the order the blips
// were mounted (document order). A large wave so shows its first screen
// quickly and still ends up fully rendered (for find-in-page and stable
// scroll positions).

type Activate = () => void;

interface IdleDeadlineLike {
  timeRemaining(): number;
}

export interface LazyActivatorOptions {
  /** The scrolling element the blips are in. */
  readonly root: Element | null;
  /** Activate everything at once, synchronously (tests). */
  readonly eager?: boolean;
  /** How far outside the viewport a blip is rendered ahead. */
  readonly margin?: string;
}

export class LazyActivator {
  private readonly pending = new Map<Element, Activate>();
  private observer: IntersectionObserver | null = null;
  private readonly eager: boolean;
  private readonly margin: string;
  private idleHandle: number | null = null;
  private disposed = false;

  constructor(options: LazyActivatorOptions) {
    this.eager = options.eager === true || typeof IntersectionObserver === "undefined";
    this.margin = options.margin ?? "800px 0px";
    if (options.root !== null) {
      this.setRoot(options.root);
    }
  }

  /** Sets the scrolling element; to be called before the first blip registers. */
  setRoot(root: Element | null): void {
    if (this.eager || this.disposed || this.observer !== null) {
      return;
    }
    this.observer = new IntersectionObserver((entries) => this.onIntersect(entries), {
      root,
      rootMargin: this.margin,
    });
    for (const element of this.pending.keys()) {
      this.observer.observe(element);
    }
  }

  /** The number of blips not rendered yet. */
  get remaining(): number {
    return this.pending.size;
  }

  /**
   * Registers a blip; `activate` is called once, when its document should
   * be rendered. Returns the function that withdraws the registration.
   */
  register(element: Element, activate: Activate): () => void {
    if (this.eager || this.disposed) {
      activate();
      return () => {};
    }
    if (this.observer === null) {
      this.setRoot(null);
    }
    this.pending.set(element, activate);
    this.observer?.observe(element);
    this.scheduleIdle();
    return () => {
      if (this.pending.delete(element)) {
        this.observer?.unobserve(element);
      }
    };
  }

  dispose(): void {
    this.disposed = true;
    this.observer?.disconnect();
    this.pending.clear();
    if (this.idleHandle !== null) {
      if (typeof cancelIdleCallback === "function") {
        cancelIdleCallback(this.idleHandle);
      } else {
        clearTimeout(this.idleHandle);
      }
      this.idleHandle = null;
    }
  }

  private run(element: Element): void {
    const activate = this.pending.get(element);
    if (activate !== undefined) {
      this.pending.delete(element);
      this.observer?.unobserve(element);
      activate();
    }
  }

  private onIntersect(entries: IntersectionObserverEntry[]): void {
    for (const entry of entries) {
      if (entry.isIntersecting) {
        this.run(entry.target);
      }
    }
  }

  private scheduleIdle(): void {
    if (this.idleHandle !== null || this.disposed) {
      return;
    }
    const work = (deadline: IdleDeadlineLike): void => {
      this.idleHandle = null;
      if (this.disposed) {
        return;
      }
      let done = 0;
      for (const element of this.pending.keys()) {
        // Always make progress, then go on while there is time.
        if (done > 0 && deadline.timeRemaining() < 3) {
          break;
        }
        this.run(element);
        done++;
      }
      if (this.pending.size > 0) {
        this.scheduleIdle();
      }
    };
    if (typeof requestIdleCallback === "function") {
      this.idleHandle = requestIdleCallback(work, { timeout: 1000 });
    } else {
      const start = (): void => {
        const began = Date.now();
        work({ timeRemaining: () => Math.max(0, 8 - (Date.now() - began)) });
      };
      this.idleHandle = setTimeout(start, 30) as unknown as number;
    }
  }
}
