// @vitest-environment happy-dom
// The registration screen names the other servers one may register on.

import { signal } from "@preact/signals";
import { render } from "preact";
import { afterEach, describe, expect, test } from "vitest";

import type { SessionInfo } from "../../net/http";
import type { AppStore } from "./appStore";
import { AuthScreen } from "./AuthScreen";

function fakeStore(session: Partial<SessionInfo>): AppStore {
  return {
    session: signal({
      domain: "rsntr.net",
      address: null,
      idSeed: "seed",
      websocketAddress: "x:1",
      registrationEnabled: true,
      socketToken: null,
      ...session,
    }),
    authScreen: signal("register"),
    authNotice: signal(null),
  } as unknown as AppStore;
}

describe("AuthScreen", () => {
  afterEach(() => {
    render(null, document.body);
  });

  test("the registration screen links to the other servers", () => {
    render(<AuthScreen store={fakeStore({ otherServers: ["resonator.network"] })} />, document.body);
    const link = document.querySelector<HTMLAnchorElement>(".auth-other-servers a");
    expect(link?.href).toBe("https://resonator.network/");
    expect(link?.textContent).toBe("Register @resonator.network");
  });

  test("without other servers there is no such line", () => {
    render(<AuthScreen store={fakeStore({})} />, document.body);
    expect(document.querySelector(".auth-other-servers")).toBe(null);
    expect(document.querySelector("h1")?.textContent).toBe("Register a new account");
  });
});
