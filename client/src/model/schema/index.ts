// The XML schemas of the documents of a wave, and their lookup by wavelet
// and document id.
//
// A schema says which elements may occur where, with which attributes and
// values, and where text is allowed. `XmlSchemaConstraints` is a schema
// built from such rules; the submodules define the schemas of conversation
// documents (`conversation`), of the documents of a user-data wavelet
// (`userData`) and of the roles document (`account`). A `SchemaProvider`
// finds the schema that applies to a document, and `SchemaCollection`
// combines providers. Validate an operation against a schema with
// `validate` of `model/docop`.
//
// TypeScript port of the Rust crate `resonator-model` (module `schema`), itself
// derived from `org.waveprotocol.wave.model.schema`
// (`AbstractXmlSchemaConstraints`, `SchemaCollection`, `SchemaProvider`,
// `SchemaUtils`, `conversation.ConversationSchemas`,
// `supplement.UserDataSchemas`, `account.AccountSchemas`) of Apache Wave,
// licensed under the Apache License, Version 2.0.
//
// Differences from the original:
//
// * A provider answers `null` where the original answers with the
//   `NO_SCHEMA_CONSTRAINTS` object; `SchemaCollection.schemaForIdOrDefault`
//   gives that object.
// * If several providers of a `SchemaCollection` have a schema for a
//   document, the first wins; the original throws.
// * An element type that is the empty string permits nothing; the original
//   throws for it.
// * A wavelet id that is not properly escaped has no schema, and is not a
//   permitted `anchorWavelet` value; the original throws in both cases.
// * Integer attribute values must use ASCII digits; `Integer.parseInt`
//   also takes other Unicode digits.

import { NO_SCHEMA_CONSTRAINTS, type DocumentSchema } from "../docop";
import type { WaveletId } from "../id";
import { AccountSchemas } from "./account";
import type { SchemaProvider } from "./constraints";
import { ConversationSchemas } from "./conversation";
import { UserDataSchemas } from "./userData";

export * as account from "./account";
export { AccountSchemas } from "./account";
export {
  BOOLEAN_VALUES,
  XmlSchemaConstraints,
  isDouble,
  isLong,
  isNonNegativeInteger,
  isPositiveInteger,
  isValidInteger,
  isWaveletId,
  type SchemaProvider,
  type ValueCheck,
} from "./constraints";
export * as conversation from "./conversation";
export { ConversationSchemas } from "./conversation";
export * as userData from "./userData";
export { UserDataSchemas } from "./userData";

/** The schema of a document, or the schema that permits anything. */
export function schemaForIdOrDefault(
  provider: SchemaProvider,
  waveletId: WaveletId,
  documentId: string,
): DocumentSchema {
  return provider.schemaForId(waveletId, documentId) ?? NO_SCHEMA_CONSTRAINTS;
}

/** Several schema providers taken together (`SchemaCollection`). */
export class SchemaCollection implements SchemaProvider {
  private readonly providers: SchemaProvider[] = [];

  /**
   * The schemas of conversations, user-data wavelets and accounts
   * together: every schema this module defines a lookup for.
   */
  static standard(): SchemaCollection {
    const schemas = new SchemaCollection();
    schemas.add(AccountSchemas);
    schemas.add(ConversationSchemas);
    schemas.add(UserDataSchemas);
    return schemas;
  }

  add(provider: SchemaProvider): void {
    this.providers.push(provider);
  }

  /** The schema of the first provider that has one, or `null`. */
  schemaForId(waveletId: WaveletId, documentId: string): DocumentSchema | null {
    for (const provider of this.providers) {
      const schema = provider.schemaForId(waveletId, documentId);
      if (schema !== null) {
        return schema;
      }
    }
    return null;
  }

  /** The schema of a document, or the schema that permits anything. */
  schemaForIdOrDefault(waveletId: WaveletId, documentId: string): DocumentSchema {
    return schemaForIdOrDefault(this, waveletId, documentId);
  }
}
