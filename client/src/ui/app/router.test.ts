import { describe, expect, test } from "vitest";

import {
  EMPTY_ROUTE,
  formatFragment,
  fragmentForWave,
  parseFragment,
  sameRoute,
  urlForRoute,
  waveRoute,
} from "./router";

describe("router", () => {
  test("a wave fragment", () => {
    expect(parseFragment("#example.com/w+abc")).toEqual({
      waveId: "example.com/w+abc",
      waveletId: null,
      blipId: null,
    });
    expect(parseFragment("example.com/w+abc")).toEqual(waveRoute("example.com/w+abc"));
  });

  test("a deep link to a blip", () => {
    const route = parseFragment("#example.com/w+abc/~/conv+root/b+xyz");
    expect(route).toEqual({
      waveId: "example.com/w+abc",
      waveletId: "example.com/conv+root",
      blipId: "b+xyz",
    });
    expect(formatFragment(route)).toBe("example.com/w+abc/~/conv+root/b+xyz");
  });

  test("a wavelet of another domain", () => {
    const route = parseFragment("#example.com/w+abc/other.org/conv+x");
    expect(route).toEqual({ waveId: "example.com/w+abc", waveletId: "other.org/conv+x", blipId: null });
    expect(formatFragment(route)).toBe("example.com/w+abc/other.org/conv+x");
  });

  test("empty and invalid fragments are the empty route", () => {
    for (const fragment of [
      "",
      "#",
      "#/",
      "#nonsense",
      "#example.com",
      "#example.com/",
      "#/w+abc",
      "#example.com/w+abc/~",
      "#example.com/w+abc/~/",
      "#exa mple.com/w+abc",
      "#example.com/w+a%ZZ",
      "#%",
      "#example.com/w+abc/~//b+1",
      "#a/b/c/d/e/f/g",
      "#access_token=abc&x=1",
    ]) {
      const route = parseFragment(fragment);
      // Whatever it is, it is a route that formats again.
      expect(() => formatFragment(route), fragment).not.toThrow();
      if (fragment !== "#a/b/c/d/e/f/g") {
        expect(route, fragment).toEqual(EMPTY_ROUTE);
      }
    }
  });

  test("tolerates a leading slash or bang", () => {
    expect(parseFragment("#/example.com/w+abc").waveId).toBe("example.com/w+abc");
    expect(parseFragment("#!example.com/w+abc").waveId).toBe("example.com/w+abc");
  });

  test("the wave domain is lower-cased, as in the original", () => {
    expect(parseFragment("#Example.COM/w+abc").waveId).toBe("example.com/w+abc");
  });

  test("formatting", () => {
    expect(formatFragment(EMPTY_ROUTE)).toBe("");
    expect(formatFragment(waveRoute("example.com/w+abc"))).toBe("example.com/w+abc");
    // A blip without a wavelet is in the root conversation.
    expect(formatFragment({ waveId: "example.com/w+abc", waveletId: null, blipId: "b+1" })).toBe(
      "example.com/w+abc/~/conv+root/b+1",
    );
    expect(fragmentForWave("example.com/w+abc")).toBe("example.com/w+abc");
    expect(fragmentForWave("not a wave id")).toBeNull();
  });

  test("what is formatted parses to the same route", () => {
    const routes = [
      waveRoute("example.com/w+abc"),
      { waveId: "example.com/w+a_b-c", waveletId: "example.com/conv+root", blipId: null },
      { waveId: "example.com/w+abc", waveletId: "example.com/conv+root", blipId: "b+x1" },
      { waveId: "example.com/w+abc", waveletId: "other.org/conv+p", blipId: "b+x1" },
    ];
    for (const route of routes) {
      expect(sameRoute(parseFragment(`#${formatFragment(route)}`), route)).toBe(true);
    }
  });

  test("urls", () => {
    expect(urlForRoute(waveRoute("example.com/w+abc"), "http://host:1/?x=1#old")).toBe(
      "http://host:1/?x=1#example.com/w+abc",
    );
    expect(urlForRoute(EMPTY_ROUTE, "http://host:1/#old")).toBe("http://host:1/");
  });
});
