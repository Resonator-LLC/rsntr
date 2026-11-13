// Document operations and operational transformation.
//
// A `DocOp` is a sequence of components that walks an XML-like document from
// start to end, retaining, inserting, deleting or re-annotating items. This
// module provides the operation types, `compose`, `transform`, `invert`,
// normalization, and validation against a document and schema.
//
// All lengths, counts and positions are in UTF-16 code units, and keys are
// ordered by UTF-16 code units, to match the wire format and the server.
//
// This is a TypeScript port of the Rust crate `resonator-model` (module `docop`),
// itself derived from the `org.waveprotocol.wave.model.document.operation`
// package of Apache Wave, licensed under the Apache License, Version 2.0.
// Unlike the original, characters outside the Basic Multilingual Plane are
// allowed; see `validate.ts`.

export {
  AnnotationBoundaryMap,
  annotationChange,
  validateAnnotationKey,
  type AnnotationChange,
  type ChangeTriple,
} from "./annotation";
export {
  AnnotationMap,
  AnnotationsUpdate,
  Attributes,
  AttributesUpdate,
  type Attribute,
  type AttributeUpdate,
  type UpdateTriple,
} from "./attributes";
export {
  DocOpCollector,
  compose,
  composeAll,
  composeInitialization,
  composeUnchecked,
} from "./compose";
export { BootstrapDocument } from "./document";
export {
  OperationError,
  TransformError,
  type OperationErrorDetail,
  type TransformErrorDetail,
} from "./error";
export { invert } from "./invert";
export {
  AnnotationsNormalizer,
  RangeNormalizer,
  newNormalizer,
  type OpNormalizer,
} from "./normalize";
export {
  BufferSink,
  Component,
  DocInitialization,
  DocOp,
  DocOpBuilder,
  OpSink,
  componentEquals,
  componentTypeName,
  isInitializationComponent,
  type DocOpComponent,
  type DocOpComponentType,
} from "./op";
export {
  NO_SCHEMA_CONSTRAINTS,
  NoSchemaConstraints,
  coerceString,
  type DocumentSchema,
  type PermittedCharacters,
} from "./schema";
export { decompose, transform, type OperationPair } from "./transform";
export {
  asInitialization,
  initialDocumentLength,
  normalize,
  normalizeInitialization,
  resultingDocumentLength,
  toConciseString,
  toXmlString,
} from "./util";
export {
  DocOpAutomaton,
  EMPTY_DOCUMENT,
  EmptyDocument,
  ValidationResult,
  Violation,
  ViolationCollector,
  isWellFormed,
  mergeValidationResults,
  validate,
  validateInitialization,
  type AutomatonDocument,
  type Collector,
} from "./validate";
