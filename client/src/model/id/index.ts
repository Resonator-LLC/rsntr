// Wave, wavelet and participant identifiers, their validity rules and their
// serialised forms.
//
// Derived from the `org.waveprotocol.wave.model.id` and
// `org.waveprotocol.wave.model.waveref` packages and
// `org.waveprotocol.wave.model.wave.ParticipantId` of Apache Wave, licensed
// under the Apache License, Version 2.0.
//
// Strings are ordered by UTF-16 code units (native `<`), as Java
// `String.compareTo` does. Where the original throws
// `IllegalArgumentException` or a checked exception on bad input, the
// functions here throw a typed error.

export * as constants from "./constants";
export * from "./constants";
export { DEFAULT_ESCAPER, EscapeError, SimplePrefixEscaper } from "./escaper";
export { IdFilter } from "./filter";
export { IdGeneratorImpl, TokenGenerator, base64EncodeInt } from "./generator";
export type { IdGenerator } from "./generator";
export { isValidDomain, isValidIdentifier } from "./identifiers";
export { DOMAIN_PREFIX, InvalidParticipantAddress, ParticipantId } from "./participant";
export { DualIdSerialiser, LegacyIdSerialiser, ModernIdSerialiser } from "./serialiser";
export type { IdSerialiser } from "./serialiser";
export { EncodingError, IdUriCodec } from "./uri";
export type { EncodingErrorDetail } from "./uri";
export * as util from "./util";
export { InvalidIdError, WaveId, WaveletId, WaveletName } from "./waveId";
export * as waveref from "./waveref";
export {
  InvalidWaveRefError,
  WaveRef,
  encodeToUriPathSegment,
  encodeToUriQueryString,
} from "./waveref";
