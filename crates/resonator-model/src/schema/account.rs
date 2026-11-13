//! Schemas of account documents (`AccountSchemas`).

use std::sync::LazyLock;

use super::{SchemaProvider, XmlSchemaConstraints};
use crate::docop::DocumentSchema;
use crate::id::WaveletId;
use crate::id::constants::ROLES_DATA_DOC_ID;
use crate::id::util::is_conversational_id;

/// The names of the roles a participant can be assigned (`Role`).
pub const ROLES: [&str; 2] = ["FULL", "READ_ONLY"];

/// The stored forms of `IndexDecision`.
pub const INDEX_DECISIONS: [&str; 2] = ["YES", "NO"];

/// Schema of the roles document, `roles`, of a conversational wavelet:
/// `<assign address=".." role="FULL"/>` elements
/// (`ROLES_SCHEMA_CONSTRAINTS`).
pub static ROLES_SCHEMA_CONSTRAINTS: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &["assign"]);
    s.add_attrs("assign", &["address"]);
    s.add_attr_with_values("assign", "role", &ROLES);
    s
});

/// Schema of an indexability document: `<index address=".." i="YES"/>`
/// elements (`INDEXABILITY_SCHEMA_CONSTRAINTS`). As in the original, no
/// document id is mapped to it.
pub static INDEXABILITY_SCHEMA_CONSTRAINTS: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &["index"]);
    s.add_attrs("index", &["address"]);
    s.add_attr_with_values("index", "i", &INDEX_DECISIONS);
    s
});

/// The schemas of account documents: the roles document of conversational
/// wavelets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountSchemas;

impl SchemaProvider for AccountSchemas {
    fn schema_for_id(
        &self,
        wavelet_id: &WaveletId,
        document_id: &str,
    ) -> Option<&dyn DocumentSchema> {
        let conversational = is_conversational_id(wavelet_id).unwrap_or(false);
        (conversational && document_id == ROLES_DATA_DOC_ID)
            .then_some(&*ROLES_SCHEMA_CONSTRAINTS as &dyn DocumentSchema)
    }
}
