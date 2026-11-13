// The route of the application and its form in the URL fragment.
//
// As in the Apache Wave web client (`HistoryChangeListener`,
// `WebClient.openWave`, `GwtWaverefEncoder`; Apache License 2.0) the
// fragment is a wave reference encoded as a URI path: `#example.com/w+abc`
// for a wave, `#example.com/w+abc/~/conv+root/b+xyz` for a blip of it. The
// server's `/waveref/<ref>` redirects to `/#<ref>`.
//
// Pure functions: the shell binds them to `location` and `history`.

import { ModernIdSerialiser, WaveId, WaveRef, WaveletId } from "../../model/id";
import type { Route } from "../contracts";

/** Wavelet ids in routes are in the modern form, `example.com/conv+root`. */
const serialiser = ModernIdSerialiser.INSTANCE;

/** No wave is open. */
export const EMPTY_ROUTE: Route = { waveId: null, waveletId: null, blipId: null };

/** The route to a wave. */
export function waveRoute(waveId: string): Route {
  return { waveId, waveletId: null, blipId: null };
}

/**
 * The route a fragment names (with or without the leading `#`). An empty or
 * invalid fragment is the empty route.
 */
export function parseFragment(fragment: string): Route {
  let path = fragment.startsWith("#") ? fragment.slice(1) : fragment;
  // Tolerate `#/example.com/w+abc` and `#!example.com/w+abc`.
  path = path.replace(/^[!/]+/, "");
  if (path === "") {
    return EMPTY_ROUTE;
  }
  let ref: WaveRef;
  try {
    ref = WaveRef.decodeFromPath(path);
  } catch {
    // `InvalidWaveRefError`, or an id the model refuses: whatever a
    // fragment says, it must not break the application.
    return EMPTY_ROUTE;
  }
  try {
    return {
      waveId: ref.waveId.serialise(),
      waveletId: ref.waveletId === null ? null : serialiser.serialiseWaveletId(ref.waveletId),
      blipId: ref.waveletId === null || ref.documentId === null || ref.documentId === "" ? null : ref.documentId,
    };
  } catch {
    return EMPTY_ROUTE;
  }
}

/**
 * The fragment of a route, without the leading `#`; empty for the empty
 * route. Throws if the route's ids are not valid.
 */
export function formatFragment(route: Route): string {
  if (route.waveId === null) {
    return "";
  }
  const waveId = WaveId.deserialise(route.waveId);
  if (route.blipId !== null) {
    // A blip is named within a wavelet; the default is the root conversation.
    const waveletId =
      route.waveletId !== null
        ? serialiser.deserialiseWaveletId(route.waveletId)
        : WaveletId.of(waveId.domain, "conv+root");
    return WaveRef.ofDocument(waveId, waveletId, route.blipId).encodeToUriPathSegment();
  }
  if (route.waveletId !== null) {
    return WaveRef.ofWavelet(waveId, serialiser.deserialiseWaveletId(route.waveletId)).encodeToUriPathSegment();
  }
  return WaveRef.of(waveId).encodeToUriPathSegment();
}

/** `formatFragment` for a wave id; null if the id is not valid. */
export function fragmentForWave(waveId: string): string | null {
  try {
    return formatFragment(waveRoute(waveId));
  } catch {
    return null;
  }
}

export function sameRoute(a: Route, b: Route): boolean {
  return a.waveId === b.waveId && a.waveletId === b.waveletId && a.blipId === b.blipId;
}

/**
 * The absolute URL of a route, given the URL of the page (whose fragment is
 * replaced).
 */
export function urlForRoute(route: Route, pageUrl: string): string {
  const hash = pageUrl.indexOf("#");
  const base = hash < 0 ? pageUrl : pageUrl.slice(0, hash);
  const fragment = formatFragment(route);
  return fragment === "" ? base : `${base}#${fragment}`;
}
