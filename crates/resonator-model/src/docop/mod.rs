//! Document operations and operational transformation.
//!
//! A [`DocOp`] is a sequence of components that walks an XML-like document
//! from start to end, retaining, inserting, deleting or re-annotating items.
//! This module provides the operation types, [`compose()`], [`transform()`],
//! [`invert()`], normalization, and validation against a document and schema.
//!
//! All lengths, counts and positions are in UTF-16 code units, and keys are
//! ordered by UTF-16 code units, to match the wire format and browser
//! clients; see [`utf16`].
//!
//! Derived from the `org.waveprotocol.wave.model.document.operation` package
//! of Apache Wave, licensed under the Apache License, Version 2.0. Unlike
//! the original, characters outside the Basic Multilingual Plane are
//! allowed; see the [`validate`](mod@validate) module.

pub mod annotation;
pub mod attributes;
pub mod compose;
pub mod document;
pub mod error;
pub mod invert;
pub mod normalize;
pub mod op;
pub mod schema;
pub mod transform;
pub mod utf16;
pub mod util;
pub mod validate;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use annotation::{AnnotationBoundaryMap, AnnotationChange};
pub use attributes::{
    AnnotationMap, AnnotationsUpdate, Attribute, AttributeUpdate, Attributes, AttributesUpdate,
};
pub use compose::{
    DocOpCollector, compose, compose_all, compose_initialization, compose_unchecked,
};
pub use document::BootstrapDocument;
pub use error::{OperationError, TransformError};
pub use invert::invert;
pub use normalize::{AnnotationsNormalizer, OpNormalizer, RangeNormalizer, new_normalizer};
pub use op::{DocInitialization, DocOp, DocOpBuilder, DocOpComponent, OpSink};
pub use schema::{DocumentSchema, NO_SCHEMA_CONSTRAINTS, NoSchemaConstraints, PermittedCharacters};
pub use transform::{OperationPair, decompose, transform};
pub use util::{
    as_initialization, initial_document_length, normalize, normalize_initialization,
    resulting_document_length, to_concise_string, to_xml_string,
};
pub use validate::{
    AutomatonDocument, DocOpAutomaton, EMPTY_DOCUMENT, EmptyDocument, ValidationResult, Violation,
    ViolationCollector, is_well_formed, validate, validate_initialization,
};
