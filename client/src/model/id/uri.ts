// The `wave://` URI form of wavelet names (`IdURIEncoderDecoder` with the
// server's `JavaUrlCodec`). The version-zero history hash of a wavelet is
// the UTF-8 encoding of this URI.
//
// Derived from the `org.waveprotocol.wave.model.id` package of Apache Wave,
// licensed under the Apache License, Version 2.0.

import { WAVE_URI_SCHEME } from "./constants";
import { DEFAULT_ESCAPER } from "./escaper";
import { isAsciiAlphanumeric, split, urlDecodeBytes, urlEncode, utf8DecodeStrict } from "./java";
import { InvalidIdError, WaveId, WaveletId, WaveletName } from "./waveId";

/** The reason for an {@link EncodingError}. */
export type EncodingErrorDetail =
  /** A percent-escape is malformed or does not decode to UTF-8. */
  | { readonly kind: "decode"; readonly value: string }
  /** The URI does not have the expected shape. */
  | { readonly kind: "format"; readonly message: string }
  | { readonly kind: "invalidId"; readonly error: InvalidIdError };

/** Failure to decode a wavelet name URI. */
export class EncodingError extends Error {
  readonly detail: EncodingErrorDetail;

  constructor(detail: EncodingErrorDetail) {
    super(
      detail.kind === "decode"
        ? `Unable to decode value ${detail.value}`
        : detail.kind === "format"
          ? detail.message
          : detail.error.message,
      detail.kind === "invalidId" ? { cause: detail.error } : undefined,
    );
    this.name = "EncodingError";
    this.detail = detail;
  }

  get kind(): EncodingErrorDetail["kind"] {
    return this.detail.kind;
  }
}

/**
 * Characters of an id that are written to a URI path as they are, besides
 * ASCII letters and digits.
 */
const NOT_ESCAPED_SYMBOLS = ":@!$&'()*+,;=-._~";

function isNotEscaped(unit: number): boolean {
  return (
    isAsciiAlphanumeric(unit) ||
    (unit < 0x80 && NOT_ESCAPED_SYMBOLS.includes(String.fromCharCode(unit)))
  );
}

function legacyId<T>(f: () => T): T {
  try {
    return f();
  } catch (e) {
    if (e instanceof InvalidIdError) {
      throw new EncodingError({ kind: "invalidId", error: e });
    }
    throw e;
  }
}

/** Converts wavelet names to and from `wave://` URIs. */
export const IdUriCodec = {
  /**
   * Percent-encodes an id for use in a URI path segment. Runs of other
   * characters go through `URLEncoder`, so a space becomes `+`.
   */
  encode(decoded: string): string {
    let out = "";
    let i = 0;
    while (i < decoded.length) {
      if (isNotEscaped(decoded.charCodeAt(i))) {
        out += decoded.charAt(i);
        i += 1;
      } else {
        let end = i + 1;
        while (end < decoded.length && !isNotEscaped(decoded.charCodeAt(end))) {
          end += 1;
        }
        out += urlEncode(decoded.slice(i, end));
        i = end;
      }
    }
    return out;
  },

  /**
   * Reverses {@link IdUriCodec.encode}. A literal `+` stands for itself.
   * Throws {@link EncodingError} on malformed escapes, on escapes that are
   * not UTF-8, and (as in the original) on any value containing U+FFFD.
   */
  decode(encoded: string): string {
    const bytes = urlDecodeBytes(encoded.replaceAll("+", "%2B"));
    const decoded = bytes === null ? null : utf8DecodeStrict(bytes);
    if (decoded === null || decoded.includes("�")) {
      throw new EncodingError({ kind: "decode", value: encoded });
    }
    return decoded;
  },

  /** `wave://` followed by {@link IdUriCodec.waveletNameToUriPath}. */
  waveletNameToUri(name: WaveletName): string {
    return `${WAVE_URI_SCHEME}://${IdUriCodec.waveletNameToUriPath(name)}`;
  },

  /**
   * `<wavelet domain>/[<wave domain>!]<wave id>/<wavelet id>`, with the
   * wave domain omitted when it equals the wavelet domain.
   */
  waveletNameToUriPath(name: WaveletName): string {
    const waveDomain =
      name.waveletId.domain === name.waveId.domain ? "" : `${name.waveId.domain}!`;
    return (
      `${name.waveletId.domain}/${waveDomain}${IdUriCodec.encode(name.waveId.id)}` +
      `/${IdUriCodec.encode(name.waveletId.id)}`
    );
  },

  /** Throws {@link EncodingError}. */
  uriToWaveletName(uri: string): WaveletName {
    const scheme = `${WAVE_URI_SCHEME}://`;
    if (!uri.startsWith(scheme)) {
      throw new EncodingError({
        kind: "format",
        message: `Invalid scheme for the wavelet name URI: ${uri}`,
      });
    }
    return IdUriCodec.uriPathToWaveletName(uri.slice(scheme.length));
  },

  /**
   * Parses a URI path into a wavelet name. Ids are created under the legacy
   * rules. Throws {@link EncodingError}.
   */
  uriPathToWaveletName(path: string): WaveletName {
    const tokens = split(path, "/");
    const [waveletDomain, wavePart, waveletPart] = tokens;
    if (
      tokens.length !== 3 ||
      waveletDomain === undefined ||
      wavePart === undefined ||
      waveletPart === undefined
    ) {
      throw new EncodingError({
        kind: "format",
        message:
          "Wavelet name URI path expected in the format of <wavelet domain>/[<wave " +
          `domain>!]<wave id>/<wavelet id> but got: ${path}`,
      });
    }
    const decodedWaveletPart = IdUriCodec.decode(waveletPart);
    const waveletId = legacyId(() => WaveletId.ofLegacy(waveletDomain, decodedWaveletPart));
    const waveIdParts = DEFAULT_ESCAPER.splitWithoutUnescaping("!", wavePart);
    const [first, second] = waveIdParts;
    let waveId: WaveId;
    if (waveIdParts.length === 1 && first !== undefined) {
      const id = IdUriCodec.decode(first);
      waveId = legacyId(() => WaveId.ofLegacy(waveletId.domain, id));
    } else if (waveIdParts.length === 2 && first !== undefined && second !== undefined) {
      const id = IdUriCodec.decode(second);
      waveId = legacyId(() => WaveId.ofLegacy(first, id));
    } else {
      throw new EncodingError({
        kind: "format",
        message:
          "Wave id in URI path is invalid. Expected the format [<wave domain>!]<wave " +
          `id> but got: ${wavePart}`,
      });
    }
    return WaveletName.of(waveId, waveletId);
  },
} as const;
