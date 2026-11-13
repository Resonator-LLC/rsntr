// Prefix escaping of separator characters in ids (`SimplePrefixEscaper`).
//
// Derived from the `org.waveprotocol.wave.model.id` package of Apache Wave,
// licensed under the Apache License, Version 2.0.

/**
 * Failure to escape, unescape, join or split (an `IllegalArgumentException`
 * in the original).
 */
export class EscapeError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "EscapeError";
  }
}

/**
 * Escapes a set of characters by prefixing them with a prefix character.
 * The prefix, the escaped characters and separators are single UTF-16 code
 * units.
 */
export class SimplePrefixEscaper {
  readonly prefix: string;
  /** Always contains the prefix itself. */
  private readonly escaped: string;

  constructor(prefix: string, needsEscaping: Iterable<string>) {
    this.prefix = prefix;
    this.escaped = prefix + [...needsEscaping].join("");
  }

  private needsEscaping(c: string): boolean {
    return this.escaped.includes(c);
  }

  escape(toEscape: string): string {
    let out = "";
    for (let i = 0; i < toEscape.length; i++) {
      const c = toEscape.charAt(i);
      if (this.needsEscaping(c)) {
        out += this.prefix;
      }
      out += c;
    }
    return out;
  }

  /** Throws {@link EscapeError} if the value is not properly escaped. */
  unescape(toUnescape: string): string {
    let out = "";
    for (let i = 0; i < toUnescape.length; i++) {
      const c = toUnescape.charAt(i);
      if (c === this.prefix) {
        i += 1;
        if (i >= toUnescape.length) {
          throw new EscapeError(
            `The value to unescape cannot be terminated with the prefix: ${this.prefix}`,
          );
        }
        const next = toUnescape.charAt(i);
        if (!this.needsEscaping(next)) {
          throw new EscapeError(
            "The value to unescape is not a properly escaped value. The prefix " +
              `charater is not followed by a character at needs prefixing: ${toUnescape}`,
          );
        }
        out += next;
      } else if (this.needsEscaping(c)) {
        throw new EscapeError(
          "The value to unescape is not a properly escaped value. Some chars are " +
            `found unescaped: ${toUnescape}`,
        );
      } else {
        out += c;
      }
    }
    return out;
  }

  /**
   * Escapes the tokens and joins them with `separator`, which must be an
   * escaped character other than the prefix.
   */
  join(separator: string, tokens: readonly string[]): string {
    if (separator === this.prefix) {
      throw new EscapeError("It's unsafe to join strings together using the prefixchar.");
    }
    // "" is then unambiguously the join of a single empty token.
    if (tokens.length === 0) {
      throw new EscapeError("Must have at least 1 token to use join.");
    }
    if (!this.needsEscaping(separator)) {
      throw new EscapeError(
        `It's unsafe to join strings together using a [separator:${separator}] that is ` +
          "not in the characters that are escaped.",
      );
    }
    return tokens.map((token) => this.escape(token)).join(separator);
  }

  /** Splits on unescaped separators and unescapes each token. */
  split(separator: string, toSplit: string): string[] {
    return this.splitWithoutUnescaping(separator, toSplit).map((token) => this.unescape(token));
  }

  /** Splits on unescaped separators, leaving the tokens escaped. */
  splitWithoutUnescaping(separator: string, toSplit: string): string[] {
    if (separator === this.prefix) {
      throw new EscapeError("It's unsafe to split strings together the prefix char.");
    }
    const out: string[] = [];
    let start = 0;
    for (let i = 0; i < toSplit.length; i++) {
      const c = toSplit.charAt(i);
      if (c === separator) {
        out.push(toSplit.slice(start, i));
        start = i + 1;
      } else if (c === this.prefix) {
        // Skip over the escaped char.
        i += 1;
      }
    }
    out.push(toSplit.slice(start));
    return out;
  }

  /**
   * Whether every escaped character other than `separator` is preceded by
   * the prefix.
   */
  isEscapedProperly(separator: string, escapedValue: string): boolean {
    for (let i = 0; i < escapedValue.length; i++) {
      const c = escapedValue.charAt(i);
      if (c === this.prefix) {
        i += 1;
        if (i >= escapedValue.length || !this.needsEscaping(escapedValue.charAt(i))) {
          return false;
        }
      } else if (c !== separator && this.needsEscaping(c)) {
        return false;
      }
    }
    return true;
  }

  /** Whether `unescaped` contains a character that would be escaped. */
  hasEscapeCharacters(unescaped: string): boolean {
    for (let i = 0; i < unescaped.length; i++) {
      if (this.needsEscaping(unescaped.charAt(i))) {
        return true;
      }
    }
    return false;
  }
}

/** The escaper used for ids: prefix `~`, escaping `~`, `+` and `!`. */
export const DEFAULT_ESCAPER = new SimplePrefixEscaper("~", ["+", "!"]);
