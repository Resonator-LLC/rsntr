// @vitest-environment happy-dom
// When app media tries WebRTC: only with RTCPeerConnection, and not when
// the page is told to keep it on the socket.

import { afterEach, describe, expect, it, vi } from "vitest";

import { MEDIA_PATH_KEY, rtcWanted } from "./rtc";

describe("rtcWanted", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    localStorage.clear();
  });

  it("is false without RTCPeerConnection", () => {
    vi.stubGlobal("RTCPeerConnection", undefined);
    expect(rtcWanted()).toBe(false);
  });

  it("is true with RTCPeerConnection, false when forced to the socket", () => {
    vi.stubGlobal("RTCPeerConnection", class {});
    expect(rtcWanted()).toBe(true);
    localStorage.setItem(MEDIA_PATH_KEY, "socket");
    expect(rtcWanted()).toBe(false);
  });
});
