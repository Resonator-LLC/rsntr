// Accessibility states for a wavelet.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.channel.Accessibility`
// of Apache Wave, licensed under the Apache License, Version 2.0.

/** An enum-like class: the members are the static fields. */
export class Accessibility {
  /** The wavelet is inaccessible beyond a provided snapshot. */
  static readonly INACCESSIBLE = new Accessibility(
    "INACCESSIBLE",
    false,
    false,
  );
  /** The wavelet is readable but not writable. */
  static readonly READ_ONLY = new Accessibility("READ_ONLY", true, false);
  /** The wavelet is readable and writable. */
  static readonly READ_WRITE = new Accessibility("READ_WRITE", true, true);

  private constructor(
    /** The Java enum constant name. */
    readonly name: string,
    private readonly readable: boolean,
    private readonly writable: boolean,
  ) {}

  /** Whether updates should be expected from a channel. */
  isReadable(): boolean {
    return this.readable;
  }

  /** Whether changes may be submitted to a channel. */
  isWritable(): boolean {
    return this.writable;
  }

  toString(): string {
    return this.name;
  }
}
