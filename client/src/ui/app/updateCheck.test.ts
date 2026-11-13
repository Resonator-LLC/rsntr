import { describe, expect, test } from "vitest";

import { MIN_GAP_MS, UpdateCheck, type UpdateCheckEnv, buildOfScript } from "./updateCheck";

function harness(options: { own?: string | null; served?: string | null; unsaved?: boolean; hidden?: boolean } = {}) {
  const state = {
    served: options.served === undefined ? "index-new.js" : options.served,
    unsaved: options.unsaved ?? false,
    hidden: options.hidden ?? false,
    reloads: 0,
    fetches: 0,
    now: 1_000_000,
    intervals: 0,
  };
  const env: UpdateCheckEnv = {
    ownBuild: options.own === undefined ? "index-old.js" : options.own,
    fetchBuild: async () => {
      state.fetches++;
      return state.served;
    },
    hasUnsavedWork: () => state.unsaved,
    isHidden: () => state.hidden,
    reload: () => {
      state.reloads++;
    },
    setInterval: () => {
      state.intervals++;
      return 1;
    },
    clearInterval: () => {},
    now: () => state.now,
  };
  return { check: new UpdateCheck(env), state };
}

describe("buildOfScript", () => {
  test("names a built asset", () => {
    expect(buildOfScript("http://mothership:9898/assets/index-B3h3E6m_.js")).toBe("index-B3h3E6m_.js");
    expect(buildOfScript("https://x/assets/index-a.js?v=1")).toBe("index-a.js");
  });
  test("is null for a development server", () => {
    expect(buildOfScript("http://localhost:5173/src/ui/app/updateCheck.ts")).toBeNull();
  });
});

describe("UpdateCheck", () => {
  test("the same build changes nothing", async () => {
    const { check, state } = harness({ served: "index-old.js" });
    await check.check("reconnect");
    expect(check.available.value).toBe(false);
    expect(state.reloads).toBe(0);
  });

  test("a new build after a reconnect, page in view: offers to reload", async () => {
    const { check, state } = harness();
    await check.check("reconnect");
    expect(check.available.value).toBe(true);
    expect(state.reloads).toBe(0);
    check.reloadNow();
    expect(state.reloads).toBe(1);
  });

  test("a new build when the page is shown again: reloads at once", async () => {
    const { check, state } = harness();
    await check.check("shown");
    expect(state.reloads).toBe(1);
  });

  test("a new build found while hidden: reloads at once", async () => {
    const { check, state } = harness({ hidden: true });
    await check.check("timer");
    expect(state.reloads).toBe(1);
  });

  test("unsaved changes are never thrown away", async () => {
    const { check, state } = harness({ unsaved: true });
    await check.check("shown");
    expect(state.reloads).toBe(0);
    expect(check.available.value).toBe(true);
    check.onHidden();
    expect(state.reloads).toBe(0);
    state.unsaved = false;
    check.onHidden();
    expect(state.reloads).toBe(1);
  });

  test("showing the page often asks the server once per gap", async () => {
    const { check, state } = harness({ served: "index-old.js" });
    check.onShown();
    await Promise.resolve();
    check.onShown();
    expect(state.fetches).toBe(1);
    state.now += MIN_GAP_MS + 1;
    check.onShown();
    expect(state.fetches).toBe(2);
  });

  test("off on a development server", async () => {
    const { check, state } = harness({ own: null });
    check.start();
    await check.check("reconnect");
    expect(state.fetches).toBe(0);
    expect(state.intervals).toBe(0);
  });

  test("an unreachable or silent server changes nothing", async () => {
    const { check, state } = harness({ served: null });
    await check.check("shown");
    expect(state.reloads).toBe(0);
    expect(check.available.value).toBe(false);
  });
});
