// A minimal logger for the concurrency control code.
//
// Replaces `org.waveprotocol.wave.common.logging.LoggerBundle` of Apache Wave
// (Apache License, Version 2.0); only the two levels the port uses.

export interface Logger {
  /** Whether {@link Logger.trace} output is wanted (guards costly messages). */
  shouldLogTrace(): boolean;
  trace(...messages: unknown[]): void;
  error(...messages: unknown[]): void;
}

/** A logger that discards everything. */
export const NO_LOGGER: Logger = {
  shouldLogTrace: () => false,
  trace: () => {},
  error: () => {},
};

/** A logger that writes to the console. */
export function consoleLogger(prefix: string, traceEnabled = true): Logger {
  return {
    shouldLogTrace: () => traceEnabled,
    trace: (...messages) => {
      if (traceEnabled) {
        console.debug(prefix, ...messages);
      }
    },
    error: (...messages) => console.error(prefix, ...messages),
  };
}
