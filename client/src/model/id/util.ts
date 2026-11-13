// Helpers for classifying and building ids (`IdUtil`).
//
// Derived from the `org.waveprotocol.wave.model.id` package of Apache Wave,
// licensed under the Apache License, Version 2.0.

import {
  ATTACHMENT_METADATA_PREFIX,
  BLIP_PREFIX,
  CONVERSATION_ROOT_WAVELET,
  CONVERSATION_WAVELET_PREFIX,
  GHOST_BLIP_PREFIX,
  MANIFEST_DOCUMENT_ID,
  ROBOT_PREFIX,
  TAGS_DOC_ID,
  TOKEN_SEPARATOR,
  USER_DATA_WAVELET_PREFIX,
} from "./constants";
import { DEFAULT_ESCAPER, EscapeError } from "./escaper";
import type { ParticipantId } from "./participant";
import { WaveletId } from "./waveId";

export function isConversationRootWaveletId(waveletId: WaveletId): boolean {
  return waveletId.id === CONVERSATION_ROOT_WAVELET || waveletId.id === "conversation/root";
}

/**
 * Whether the wavelet id is that of a conversational wavelet. Legacy ids
 * without a prefix token count as conversational. Throws
 * {@link EscapeError} if the id is not properly escaped (the original
 * throws `IllegalArgumentException`).
 */
export function isConversationalId(waveletId: WaveletId): boolean {
  const token = getInitialToken(waveletId.id);
  return token === CONVERSATION_WAVELET_PREFIX || token === "";
}

export function isBlipId(id: string): boolean {
  return hasInitialTokenUnescaped(id, BLIP_PREFIX) || isGhostBlipId(id) || isLegacyBlipId(id);
}

function isLegacyBlipId(id: string): boolean {
  // Legacy blips have random ids containing "*"; data documents from that
  // time have a few well-known prefixes.
  return (
    id.includes("*") &&
    !id.startsWith("m/") &&
    !id.startsWith("attach+") &&
    !id.startsWith("spell+")
  );
}

export function isGhostBlipId(id: string): boolean {
  return hasInitialTokenUnescaped(id, GHOST_BLIP_PREFIX);
}

export function isAttachmentDataDocument(id: string): boolean {
  return hasInitialTokenUnescaped(id, ATTACHMENT_METADATA_PREFIX) || id.startsWith("m/attachment/");
}

export function isManifestDocument(id: string): boolean {
  return id === MANIFEST_DOCUMENT_ID;
}

export function isTagsDataDocument(id: string): boolean {
  return id === TAGS_DOC_ID;
}

export function isRobotDocId(id: string): boolean {
  return hasInitialTokenUnescaped(id, ROBOT_PREFIX);
}

/**
 * The id of the user-data wavelet of `participant`, in the participant's
 * domain. Throws `InvalidIdError`.
 */
export function buildUserDataWaveletId(participant: ParticipantId): WaveletId {
  return WaveletId.of(participant.domain(), join([USER_DATA_WAVELET_PREFIX, participant.address]));
}

/** The address a user-data wavelet id belongs to, if it is one. */
export function getUserDataWaveletAddress(waveletId: WaveletId): string | null {
  const tokens = split(waveletId.id);
  if (tokens !== null && tokens.length === 2 && tokens[0] === USER_DATA_WAVELET_PREFIX) {
    return tokens[1] ?? null;
  }
  return null;
}

export function isUserDataWavelet(waveletId: WaveletId): boolean {
  return hasInitialTokenUnescaped(waveletId.id, USER_DATA_WAVELET_PREFIX);
}

export function isUserDataWaveletOf(address: string, waveletId: WaveletId): boolean {
  return getUserDataWaveletAddress(waveletId) === address;
}

/**
 * The first token of an id with more than one token, otherwise "". Throws
 * {@link EscapeError} if the id is not properly escaped.
 */
export function getInitialToken(id: string): string {
  const tokens = DEFAULT_ESCAPER.split(TOKEN_SEPARATOR, id);
  return tokens.length > 1 ? (tokens[0] ?? "") : "";
}

/**
 * Whether `id` starts with `token` followed by the token separator. `token`
 * must not contain the separator.
 */
export function hasInitialTokenUnescaped(id: string, token: string): boolean {
  return id.startsWith(token) && id.startsWith(TOKEN_SEPARATOR, token.length);
}

/**
 * Splits an id into its unescaped tokens; `null` if it is not properly
 * escaped.
 */
export function split(id: string): string[] | null {
  try {
    return DEFAULT_ESCAPER.split(TOKEN_SEPARATOR, id);
  } catch (e) {
    if (e instanceof EscapeError) {
      return null;
    }
    throw e;
  }
}

/** Escapes and joins tokens into an id. An empty token list gives "". */
export function join(tokens: readonly string[]): string {
  return tokens.length === 0 ? "" : DEFAULT_ESCAPER.join(TOKEN_SEPARATOR, tokens);
}
