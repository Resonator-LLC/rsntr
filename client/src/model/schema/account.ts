// Schemas of account documents (`AccountSchemas`).
//
// TypeScript port of the Rust crate `resonator-model` (module `schema`), itself
// derived from `org.waveprotocol.wave.model.schema.account`
// (`AccountSchemas`) of Apache Wave, licensed under the Apache License,
// Version 2.0.

import type { DocumentSchema } from "../docop";
import { ROLES_DATA_DOC_ID, type WaveletId } from "../id";
import { XmlSchemaConstraints, isConversational, type SchemaProvider } from "./constraints";

/** The names of the roles a participant can be assigned (`Role`). */
export const ROLES: readonly string[] = ["FULL", "READ_ONLY"];

/** The stored forms of `IndexDecision`. */
export const INDEX_DECISIONS: readonly string[] = ["YES", "NO"];

function rolesSchema(): XmlSchemaConstraints {
  const s = new XmlSchemaConstraints();
  s.addChildren(null, ["assign"]);
  s.addAttrs("assign", ["address"]);
  s.addAttrWithValues("assign", "role", ROLES);
  return s;
}

function indexabilitySchema(): XmlSchemaConstraints {
  const s = new XmlSchemaConstraints();
  s.addChildren(null, ["index"]);
  s.addAttrs("index", ["address"]);
  s.addAttrWithValues("index", "i", INDEX_DECISIONS);
  return s;
}

/**
 * Schema of the roles document, `roles`, of a conversational wavelet:
 * `<assign address=".." role="FULL"/>` elements
 * (`ROLES_SCHEMA_CONSTRAINTS`).
 */
export const ROLES_SCHEMA_CONSTRAINTS: XmlSchemaConstraints = rolesSchema();

/**
 * Schema of an indexability document: `<index address=".." i="YES"/>`
 * elements (`INDEXABILITY_SCHEMA_CONSTRAINTS`). As in the original, no
 * document id is mapped to it.
 */
export const INDEXABILITY_SCHEMA_CONSTRAINTS: XmlSchemaConstraints = indexabilitySchema();

/**
 * The schemas of account documents: the roles document of conversational
 * wavelets.
 */
export const AccountSchemas: SchemaProvider = {
  schemaForId(waveletId: WaveletId, documentId: string): DocumentSchema | null {
    return isConversational(waveletId) && documentId === ROLES_DATA_DOC_ID
      ? ROLES_SCHEMA_CONSTRAINTS
      : null;
  },
};
