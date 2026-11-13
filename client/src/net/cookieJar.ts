// A minimal cookie jar for `fetch` outside the browser (Node: headless
// clients, agents and integration tests), where nothing keeps the session
// cookie between requests.
//
// Deliberately small: one jar serves ONE origin. Cookies are kept by name
// only; `Domain`, `Path`, `Secure`, `SameSite` and `HttpOnly` are ignored,
// and every cookie is sent with every request made through the wrapped
// `fetch`. Do not share a jar between servers. Cookies set by a response
// that `fetch` follows a redirect from are not seen.

/** The cookies of one origin, by name. */
export class CookieJar {
  private readonly cookies = new Map<string, string>();

  get(name: string): string | null {
    return this.cookies.get(name) ?? null;
  }

  set(name: string, value: string): void {
    this.cookies.set(name, value);
  }

  delete(name: string): void {
    this.cookies.delete(name);
  }

  clear(): void {
    this.cookies.clear();
  }

  get size(): number {
    return this.cookies.size;
  }

  /** The value of the `Cookie` request header, or null without cookies. */
  header(): string | null {
    if (this.cookies.size === 0) {
      return null;
    }
    return Array.from(this.cookies, ([name, value]) => `${name}=${value}`).join("; ");
  }

  /**
   * Applies one `Set-Cookie` header value. A cookie with `Max-Age` of zero
   * or less, or (without `Max-Age`) with `Expires` in the past, is removed.
   */
  setCookie(headerValue: string): void {
    const [pair = "", ...attributes] = headerValue.split(";");
    const equals = pair.indexOf("=");
    if (equals < 0) {
      return;
    }
    const name = pair.slice(0, equals).trim();
    const value = pair.slice(equals + 1).trim();
    if (name === "") {
      return;
    }
    let maxAge: number | null = null;
    let expires: number | null = null;
    for (const attribute of attributes) {
      const separator = attribute.indexOf("=");
      if (separator < 0) {
        continue;
      }
      const key = attribute.slice(0, separator).trim().toLowerCase();
      const text = attribute.slice(separator + 1).trim();
      if (key === "max-age" && /^-?[0-9]+$/.test(text)) {
        maxAge = Number(text);
      } else if (key === "expires") {
        const time = Date.parse(text);
        expires = Number.isNaN(time) ? null : time;
      }
    }
    const expired = maxAge !== null ? maxAge <= 0 : expires !== null && expires <= Date.now();
    if (expired) {
      this.cookies.delete(name);
    } else {
      this.cookies.set(name, value);
    }
  }
}

/**
 * Wraps `fetch` so that cookies set by responses are sent back with later
 * requests: `Set-Cookie` name/value pairs are stored in `jar` (read with
 * `Headers.getSetCookie()`), and sent as the `Cookie` header, added to a
 * `Cookie` header the request already carries. Single origin; see the
 * header of this file for what is not implemented.
 */
export function withCookieJar(
  fetchFunction: typeof fetch,
  jar: CookieJar = new CookieJar(),
): typeof fetch {
  return async (input, init) => {
    const headers = new Headers(
      init?.headers ?? (input instanceof Request ? input.headers : undefined),
    );
    const stored = jar.header();
    if (stored !== null) {
      const own = headers.get("cookie");
      headers.set("cookie", own === null || own === "" ? stored : `${own}; ${stored}`);
    }
    const response = await fetchFunction(input, { ...init, headers });
    for (const value of response.headers.getSetCookie()) {
      jar.setCookie(value);
    }
    return response;
  };
}
