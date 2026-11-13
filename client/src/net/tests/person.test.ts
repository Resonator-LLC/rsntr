// The person API of device nodes against a fake `fetch`.

import { describe, expect, test } from "vitest";

import { NetworkError, PersonApiError, WaveHttpApi, isPairLink, parseOverview, personErrorText } from "..";

interface Call {
  readonly url: string;
  readonly method: string;
  readonly body: unknown;
}

function api(...replies: { status?: number; body?: unknown }[]): { api: WaveHttpApi; calls: Call[] } {
  const calls: Call[] = [];
  const queue = [...replies];
  const fetchFunction: typeof fetch = async (input, init) => {
    calls.push({
      url: String(input),
      method: init?.method ?? "GET",
      body: typeof init?.body === "string" ? JSON.parse(init.body) : undefined,
    });
    const reply = queue.shift() ?? {};
    return new Response(reply.body === undefined ? "{}" : JSON.stringify(reply.body), {
      status: reply.status ?? 200,
    });
  };
  return { api: new WaveHttpApi({ baseUrl: "http://node", fetch: fetchFunction }), calls };
}

const OVERVIEW = {
  person: "pp",
  domain: "pp",
  device: "dd",
  device_name: "phone",
  admin: true,
  anchor: false,
  devices: [
    { key: "dd", name: "phone", admin: true, revoked: false, this: true, online: false, last_seen: null, holder: true, anchor: false },
    { key: "ee", name: "", admin: false, revoked: true, this: false, online: false, last_seen: 5, holder: false, anchor: false },
    { name: "no key" },
  ],
  lease: { holder: "dd", term: 3, expires_at: 99, held_here: true, expired: false, online: true, co_host: null },
  co_hosts: [{ domain: "friend.example", until: 7, active: true }],
  revocation_seq: 1,
};

describe("PersonApi", () => {
  test("reads the overview, skipping what is not a device", async () => {
    const { api: http, calls } = api({ body: OVERVIEW });
    const overview = await http.person.overview();
    expect(calls[0]).toMatchObject({ url: "http://node/person", method: "GET" });
    expect(overview?.domain).toBe("pp");
    expect(overview?.devices.map((d) => [d.key, d.self, d.revoked, d.lastSeen])).toEqual([
      ["dd", true, false, null],
      ["ee", false, true, 5],
    ]);
    expect(overview?.lease).toEqual({ holder: "dd", term: 3, expiresAt: 99, heldHere: true, expired: false, coHost: null });
    expect(overview?.coHosts).toEqual([{ domain: "friend.example", until: 7, active: true }]);
  });

  test("is null outside person mode", async () => {
    const { api: http } = api({ status: 404, body: { status: "FAILED", message: "Person mode is not enabled." } });
    expect(await http.person.overview()).toBeNull();
    expect(parseOverview({ nothing: 1 })).toBeNull();
  });

  test("sends each action where the server expects it", async () => {
    const { api: http, calls } = api(
      {},
      {},
      { body: { phrase: "a b c" } },
      { body: { person: "pp", restart: true } },
      {},
      { body: { id: "i1", admin: false, expires_at: 9, state: "open", link: "resonator:pair/x", qr: "<svg/>" } },
      { body: { invitations: [{ id: "i1", state: "requested", joiner_name: "tablet", admin: true }] } },
      {},
      {},
      { body: { person: "pp", admin: false, inviter: "dd" } },
    );
    const person = http.person;
    await person.revoke("ee");
    await person.setAnchor(true);
    expect(await person.revealPhrase()).toBe("a b c");
    expect(await person.recover("a b c", true, ["k@1.2.3.4:5"])).toBe("pp");
    await person.coHost("friend.example", 0);
    const invitation = await person.invite(false);
    expect(invitation).toMatchObject({ id: "i1", link: "resonator:pair/x", qrSvg: "<svg/>", state: "open" });
    expect(await person.pairings()).toEqual([
      { id: "i1", admin: true, expiresAt: 0, state: "requested", joinerName: "tablet" },
    ]);
    await person.confirmPairing("i 1");
    await person.cancelPairing("i1");
    expect(await person.join("  resonator:pair/x ", "tablet")).toEqual({ person: "pp", admin: false });
    expect(calls.map((c) => [c.method, c.url.slice("http://node".length), c.body])).toEqual([
      ["POST", "/person/revoke", { device: "ee" }],
      ["POST", "/person/anchor", { anchor: true }],
      ["POST", "/person/phrase", { confirm: true }],
      ["POST", "/person/recover", { phrase: "a b c", revoke_old: true, peers: ["k@1.2.3.4:5"] }],
      ["POST", "/person/cohost", { domain: "friend.example", until: 0 }],
      ["POST", "/person/pair", { admin: false }],
      ["GET", "/person/pair", undefined],
      ["POST", "/person/pair/i%201/confirm", undefined],
      ["DELETE", "/person/pair/i1", undefined],
      ["POST", "/person/pair/join", { link: "resonator:pair/x", name: "tablet" }],
    ]);
  });

  test("pairing by code: the invitation's code, joining by a code, the join's progress", async () => {
    const { api: http, calls } = api(
      { body: { id: "i1", admin: false, expires_at: 9, state: "open", link: "resonator:pair/x", qr: "", code: "482913-ocean-tiger" } },
      { body: { id: "i2", admin: false, expires_at: 9, state: "open", link: "resonator:pair/y", qr: "", code: null } },
      { body: { person: "pp", admin: true } },
      { body: { stage: "waiting" } },
      { body: { stage: null } },
      { body: { stage: "nonsense" } },
      { status: 404, body: { status: "FAILED", message: "code not found or expired" } },
    );
    const person = http.person;
    expect((await person.invite(false)).code).toBe("482913-ocean-tiger");
    expect((await person.invite(false)).code).toBeNull();
    expect(await person.join(" 482913-ocean-tiger ", "tablet")).toEqual({ person: "pp", admin: true });
    expect(await person.joinProgress()).toBe("waiting");
    expect(await person.joinProgress()).toBeNull();
    expect(await person.joinProgress()).toBeNull();
    const refused = await person.join("482913-ocean-tiger", "").catch((e: unknown) => e);
    expect(refused).toBeInstanceOf(PersonApiError);
    expect((refused as PersonApiError).status).toBe(404);
    expect(calls[2]?.body).toEqual({ code: "482913-ocean-tiger", name: "tablet" });
    expect(calls[3]?.method).toBe("GET");
    expect(calls[3]?.url).toBe("http://node/person/pair/join");
  });

  test("contacts: list, add by address or link, remove", async () => {
    const { api: http, calls } = api(
      { body: { accounts: [{ address: "zoe@a", name: "Zoe", added: true }, { address: "yuri@b", added: false }, { name: "x" }] } },
      { status: 201, body: { address: "zoe@a" } },
      { status: 201, body: { address: "yuri@b" } },
      {},
      { status: 404 },
    );
    expect(await http.person.contacts()).toEqual([
      { address: "zoe@a", name: "Zoe", added: true },
      { address: "yuri@b", name: "yuri@b", added: false },
    ]);
    expect(await http.person.addContact(" zoe@a ")).toBe("zoe@a");
    expect(await http.person.addContact("resonator:contact/yuri@b")).toBe("yuri@b");
    await http.person.removeContact("zoe@a");
    expect(await http.person.contacts()).toEqual([]);
    expect(calls.slice(1, 4).map((c) => [c.method, c.url.slice(11), c.body])).toEqual([
      ["POST", "/contacts", { address: "zoe@a" }],
      ["POST", "/contacts", { link: "resonator:contact/yuri@b" }],
      ["DELETE", "/contacts/zoe%40a", undefined],
    ]);
  });

  test("host status and continuing a wave", async () => {
    const { api: http, calls } = api(
      { body: { host: "pp", local: false, frozen: true, reason: "gone" } },
      { body: { wave: "qq/w+new" } },
      { status: 409, body: { status: "FAILED", message: "The wave's host is reachable." } },
    );
    expect(await http.person.hostStatus("pp/w+old")).toEqual({ host: "pp", local: false, frozen: true, reason: "gone" });
    expect(calls[0]?.url).toBe("http://node/waves/host-status?wave=pp%2Fw%2Bold");
    expect(await http.person.continueWave("pp/w+old")).toBe("qq/w+new");
    const refused = await http.person.continueWave("pp/w+old").catch((e: unknown) => e);
    expect(refused).toBeInstanceOf(PersonApiError);
    expect((refused as PersonApiError).status).toBe(409);
    expect(personErrorText(refused)).toBe("The wave's host is reachable.");
  });

  test("error texts and pairing links", () => {
    expect(personErrorText(new NetworkError("down"))).toBe("The device could not be reached.");
    expect(personErrorText(new Error("x"))).toBe("Something went wrong.");
    expect(isPairLink(" resonator:pair/abc ")).toBe(true);
    expect(isPairLink("resonator:pair/")).toBe(false);
    expect(isPairLink("https://example.com")).toBe(false);
  });
});
