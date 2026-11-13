// Schemas of the documents of a user-data wavelet (`UserDataSchemas`). The
// document shapes are described at `WaveletBasedSupplement`.
//
// TypeScript port of the Rust crate `resonator-model` (module `schema`), itself
// derived from `org.waveprotocol.wave.model.schema.supplement`
// (`UserDataSchemas`) of Apache Wave, licensed under the Apache License,
// Version 2.0.

import type { DocumentSchema } from "../docop";
import { util as idUtil, type WaveletId } from "../id";
import * as c from "../supplement/constants";
import {
  BOOLEAN_VALUES,
  XmlSchemaConstraints,
  isDouble,
  isLong,
  isNonNegativeInteger,
  isValidInteger,
  isWaveletId,
  type SchemaProvider,
  type ValueCheck,
} from "./constraints";

function schema(build: (s: XmlSchemaConstraints) => void): XmlSchemaConstraints {
  const s = new XmlSchemaConstraints();
  build(s);
  return s;
}

const versionIsNonNegative: ValueCheck = (_tag, attr, value) =>
  attr === c.VERSION_ATTR ? isNonNegativeInteger(value) : null;

/**
 * Schema of the folder document (`UDW_FOLDER`). Any `i` attribute must be
 * an integer.
 */
export const UDW_FOLDER = schema((s) => {
  s.addChildren(null, [c.FOLDER_TAG]);
  s.addAttrs(c.FOLDER_TAG, [c.ID_ATTR]);
  s.setValueCheck((_tag, attr, value) =>
    attr === c.ID_ATTR ? isValidInteger(value, -2147483648) : null,
  );
});

/**
 * Schema of the archiving document (`UDW_ARCHIVE`). Any `v` attribute must
 * be a non-negative integer.
 */
export const UDW_ARCHIVE = schema((s) => {
  s.addChildren(null, [c.ARCHIVE_TAG]);
  s.addAttrs(c.ARCHIVE_TAG, [c.ID_ATTR, c.VERSION_ATTR]);
  s.setValueCheck(versionIsNonNegative);
});

/** Schema of the muted document (`UDW_MUTE`). */
export const UDW_MUTE = schema((s) => {
  s.addChildren(null, [c.MUTED_TAG]);
  s.addAttrWithValues(c.MUTED_TAG, c.MUTED_ATTR, BOOLEAN_VALUES);
});

/** Schema of the cleared document (`UDW_CLEARED`). */
export const UDW_CLEARED = schema((s) => {
  s.addChildren(null, [c.CLEARED_TAG]);
  s.addAttrWithValues(c.CLEARED_TAG, c.CLEARED_ATTR, BOOLEAN_VALUES);
});

/** Schema of the presentation document (`UDW_PRESENTATION`). */
export const UDW_PRESENTATION = schema((s) => {
  s.addChildren(null, [c.CONVERSATION_TAG]);
  s.addChildren(c.CONVERSATION_TAG, [c.THREAD_TAG]);
  s.addAttrs(c.CONVERSATION_TAG, [c.ID_ATTR]);
  s.addAttrs(c.THREAD_TAG, [c.ID_ATTR]);
  s.addAttrs(c.THREAD_TAG, [c.STATE_ATTR]);
});

/**
 * Schema of the read-state document (`UDW_READ`). Any `v` attribute must
 * be a non-negative integer.
 */
export const UDW_READ = schema((s) => {
  s.addChildren(null, [c.WAVELET_TAG]);
  s.addChildren(c.WAVELET_TAG, [c.BLIP_READ_TAG]);
  s.addChildren(c.WAVELET_TAG, [c.PARTICIPANTS_READ_TAG]);
  s.addChildren(c.WAVELET_TAG, [c.WAVELET_READ_TAG]);
  s.addChildren(c.WAVELET_TAG, [c.TAGS_READ_TAG]);
  s.addAttrs(c.WAVELET_TAG, [c.ID_ATTR]);
  s.addAttrs(c.BLIP_READ_TAG, [c.ID_ATTR]);
  s.addAttrs(c.BLIP_READ_TAG, [c.VERSION_ATTR]);
  s.addAttrs(c.PARTICIPANTS_READ_TAG, [c.VERSION_ATTR]);
  s.addAttrs(c.WAVELET_READ_TAG, [c.VERSION_ATTR]);
  s.addAttrs(c.TAGS_READ_TAG, [c.VERSION_ATTR]);
  s.setValueCheck(versionIsNonNegative);
});

/** Schema of the seen document (`UDW_SEEN`). */
export const UDW_SEEN = schema((s) => {
  s.addChildren(null, [c.SEEN_VERSION_TAG]);
  s.addAttrs(c.SEEN_VERSION_TAG, [c.ID_ATTR, c.SIGNATURE_ATTR]);
  s.addChildren(null, [c.NOTIFIED_VERSION_TAG]);
  s.addAttrs(c.NOTIFIED_VERSION_TAG, [c.ID_ATTR, c.VERSION_ATTR]);
  s.addChildren(null, [c.NOTIFICATION_TAG]);
  s.addAttrs(c.NOTIFICATION_TAG, [c.PENDING_NOTIFICATION_ATTR]);
});

/** Schema of the gadgets document (`UDW_GADGET`). */
export const UDW_GADGET = schema((s) => {
  s.addChildren(null, [c.GADGET_TAG]);
  s.addAttrs(c.GADGET_TAG, [c.ID_ATTR, c.PERMISSIONS_ATTR]);
  s.addChildren(c.GADGET_TAG, [c.STATE_TAG]);
  s.addAttrs(c.STATE_TAG, [c.NAME_ATTR, c.VALUE_ATTR]);
});

/**
 * Schema of the abuse document (`UDW_ABUSE`). Any `certainty` attribute
 * must be a floating-point number and any `timestamp` an integer.
 */
export const UDW_ABUSE = schema((s) => {
  s.addChildren(null, [c.WANTED_EVAL_TAG]);
  s.addAttrs(c.WANTED_EVAL_TAG, [c.WAVELET_ID_ATTR]);
  s.addAttrs(c.WANTED_EVAL_TAG, [c.CERTAINTY_ATTR]);
  s.addAttrs(c.WANTED_EVAL_TAG, [c.TIMESTAMP_ATTR]);
  s.addAttrs(c.WANTED_EVAL_TAG, [c.AGENT_ATTR]);
  s.addAttrs(c.WANTED_EVAL_TAG, [c.COMMENT_ATTR]);
  s.addAttrs(c.WANTED_EVAL_TAG, [c.IGNORED_ATTR]);
  s.addAttrs(c.WANTED_EVAL_TAG, [c.ADDER_ATTR]);
  s.addAttrWithValues(c.WANTED_EVAL_TAG, c.WANTED_ATTR, BOOLEAN_VALUES);
  s.setValueCheck((_tag, attr, value) => {
    switch (attr) {
      case c.CERTAINTY_ATTR:
        return isDouble(value);
      case c.TIMESTAMP_ATTR:
        return isLong(value);
      case c.WAVELET_ID_ATTR:
        return isWaveletId(value);
      default:
        return null;
    }
  });
});

const BY_DOCUMENT: ReadonlyMap<string, XmlSchemaConstraints> = new Map([
  [c.READSTATE_DOCUMENT, UDW_READ],
  [c.ARCHIVING_DOCUMENT, UDW_ARCHIVE],
  [c.FOLDERS_DOCUMENT, UDW_FOLDER],
  [c.MUTED_DOCUMENT, UDW_MUTE],
  [c.PRESENTATION_DOCUMENT, UDW_PRESENTATION],
  [c.CLEARED_DOCUMENT, UDW_CLEARED],
  [c.SEEN_DOCUMENT, UDW_SEEN],
  [c.GADGETS_DOCUMENT, UDW_GADGET],
  [c.ABUSE_DOCUMENT, UDW_ABUSE],
]);

/** The schemas of the supplement documents of user-data wavelets. */
export const UserDataSchemas: SchemaProvider = {
  schemaForId(waveletId: WaveletId, documentId: string): DocumentSchema | null {
    if (!idUtil.isUserDataWavelet(waveletId)) {
      return null;
    }
    return BY_DOCUMENT.get(documentId) ?? null;
  },
};
