//! Wave data model and operational transform.
//!
//! Derived from Apache Wave (Apache License 2.0): `docop` is the
//! document-operation core, `id` and `version` identify wavelets and their
//! versions, `operation` and `data` are wavelet operations and the wavelet
//! state they apply to, and `proto` is the protobuf wire and storage
//! encoding. On top of those, `doc` reads and edits documents as element
//! trees, `wavelet` records model mutations as wavelet operations, `adt`
//! holds the document-backed data types, `schema` the document schemas,
//! `conversation` the conversation model, `supplement` the per-user read
//! and inbox state, and `digest` the wave summaries for search.

pub mod adt;
pub mod conversation;
pub mod data;
pub mod digest;
pub mod doc;
pub mod docop;
pub mod id;
pub mod operation;
pub mod proto;
pub mod schema;
pub mod supplement;
pub mod version;
pub mod wavelet;

#[cfg(test)]
mod golden_tests;
#[cfg(test)]
mod model_golden_tests;
