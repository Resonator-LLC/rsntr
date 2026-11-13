//! The XML schemas of the documents of a wave, and their lookup by wavelet
//! and document id.
//!
//! A schema says which elements may occur where, with which attributes and
//! values, and where text is allowed. [`XmlSchemaConstraints`] is a schema
//! built from such rules; the submodules define the schemas of
//! conversation documents ([`conversation`]), of the documents of a
//! user-data wavelet ([`user_data`]) and of the roles document
//! ([`account`]). A [`SchemaProvider`] finds the schema that applies to a
//! document, and [`SchemaCollection`] combines providers.
//!
//! Validate an operation against a schema with
//! [`crate::docop::validate()`]:
//!
//! ```
//! use resonator_model::docop::DocumentSchema;
//! use resonator_model::id::WaveletId;
//! use resonator_model::schema::{SchemaCollection, SchemaProvider};
//!
//! let schemas = SchemaCollection::standard();
//! let root = WaveletId::of("example.com", "conv+root").unwrap();
//! let schema = schemas.schema_for_id_or_default(&root, "b+abc");
//! assert!(schema.permits_child(None, "body"));
//! assert!(!schema.permits_child(None, "wavelet"));
//! ```
//!
//! Derived from `org.waveprotocol.wave.model.schema`
//! (`AbstractXmlSchemaConstraints`, `SchemaCollection`, `SchemaProvider`,
//! `SchemaUtils`, `conversation.ConversationSchemas`,
//! `supplement.UserDataSchemas`, `account.AccountSchemas`) of Apache Wave,
//! licensed under the Apache License, Version 2.0.
//!
//! Differences from the original:
//!
//! * A provider answers `None` where the original answers with the
//!   `NO_SCHEMA_CONSTRAINTS` object; [`SchemaProvider::schema_for_id_or_default`]
//!   gives that object.
//! * If several providers of a [`SchemaCollection`] have a schema for a
//!   document, the first wins; the original throws.
//! * An element type that is the empty string permits nothing; the original
//!   throws for it.
//! * A wavelet id that is not properly escaped has no schema, and is not a
//!   permitted `anchorWavelet` value; the original throws in both cases.
//! * Integer attribute values must use ASCII digits; `Integer.parseInt`
//!   also takes other Unicode digits.

pub mod account;
pub mod conversation;
pub mod user_data;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

pub use account::AccountSchemas;
pub use conversation::ConversationSchemas;
pub use user_data::UserDataSchemas;

use crate::adt::parse_java_double;
use crate::docop::{DocumentSchema, NO_SCHEMA_CONSTRAINTS, PermittedCharacters};
use crate::id::WaveletId;

/// The values of a boolean attribute (`SchemaUtils.BOOLEAN_VALUES`).
pub const BOOLEAN_VALUES: [&str; 2] = ["true", "false"];

/// Whether a string is an integer of at least `min`
/// (`SchemaUtils.isValidInteger`).
pub fn is_valid_integer(value: &str, min: i32) -> bool {
    value.parse::<i32>().is_ok_and(|x| x >= min)
}

/// `SchemaUtils.isNonNegativeInteger`.
pub fn is_non_negative_integer(value: &str) -> bool {
    is_valid_integer(value, 0)
}

/// `SchemaUtils.isPositiveInteger`.
pub fn is_positive_integer(value: &str) -> bool {
    is_valid_integer(value, 1)
}

/// `SchemaUtils.isWaveletId`, which accepts anything.
pub fn is_wavelet_id(_value: &str) -> bool {
    true
}

/// `SchemaUtils.isLong`.
pub fn is_long(value: &str) -> bool {
    value.parse::<i64>().is_ok()
}

/// Whether `Double.parseDouble` accepts a string (`SchemaUtils.isDouble`).
pub fn is_double(value: &str) -> bool {
    parse_java_double(value).is_some()
}

/// A check of an attribute value that takes precedence over the rules of
/// an [`XmlSchemaConstraints`]: `Some(permitted)` decides, `None` leaves
/// the decision to the rules. Arguments: element type, attribute name,
/// value. Stands in for the `permitsAttribute(tag, attr, value)` overrides
/// of the original's schema subclasses.
pub type ValueCheck = fn(&str, &str, &str) -> Option<bool>;

/// A schema built from rules (`AbstractXmlSchemaConstraints`). Nothing is
/// permitted until a rule permits it.
#[derive(Debug, Clone, Default)]
pub struct XmlSchemaConstraints {
    /// Keyed by parent type, "" for the top level.
    permitted_children: BTreeMap<String, BTreeSet<String>>,
    /// By element type and attribute name: the permitted values, `None` for
    /// any value.
    permitted_attrs: BTreeMap<String, BTreeMap<String, Option<BTreeSet<String>>>>,
    text_permitted: BTreeMap<String, PermittedCharacters>,
    required_initial: BTreeMap<String, Vec<String>>,
    value_check: Option<ValueCheck>,
}

/// The key of an element type in the rule tables: "" for the top level;
/// `None` for the invalid type "".
fn fix_type(element: Option<&str>) -> Option<&str> {
    match element {
        None => Some(""),
        Some("") => None,
        Some(element) => Some(element),
    }
}

impl XmlSchemaConstraints {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the check consulted before the rules for attribute values.
    pub fn set_value_check(&mut self, check: ValueCheck) {
        self.value_check = Some(check);
    }

    /// Sets the children an element must start with (`addRequiredInitial`).
    pub fn add_required_initial(&mut self, element: &str, children: &[&str]) {
        self.required_initial.insert(
            element.to_owned(),
            children.iter().map(|&child| child.to_owned()).collect(),
        );
    }

    /// Permits blip text in elements (`containsBlipText`).
    pub fn contains_blip_text(&mut self, elements: &[&str]) {
        for &element in elements {
            self.text_permitted
                .insert(element.to_owned(), PermittedCharacters::BlipText);
        }
    }

    /// Permits any text in elements (`containsAnyText`).
    pub fn contains_any_text(&mut self, elements: &[&str]) {
        for &element in elements {
            self.text_permitted
                .insert(element.to_owned(), PermittedCharacters::Any);
        }
    }

    /// Permits children in a parent, `None` being the top level
    /// (`addChildren`).
    pub fn add_children(&mut self, parent: Option<&str>, children: &[&str]) {
        let Some(parent) = fix_type(parent) else {
            return;
        };
        let permitted = self
            .permitted_children
            .entry(parent.to_owned())
            .or_default();
        permitted.extend(children.iter().map(|&child| child.to_owned()));
    }

    /// Permits attributes with any value (`addAttrs`).
    pub fn add_attrs(&mut self, element: &str, names: &[&str]) {
        for name in names {
            self.add_attr_with_values(element, name, &[]);
        }
    }

    /// Permits an attribute with the given values, in addition to the
    /// values it may have so far; without values, with any value
    /// (`addAttrWithValues`).
    pub fn add_attr_with_values(&mut self, element: &str, name: &str, values: &[&str]) {
        let attrs = self.permitted_attrs.entry(element.to_owned()).or_default();
        if values.is_empty() {
            attrs.insert(name.to_owned(), None);
        } else {
            let permitted = attrs.entry(name.to_owned()).or_default();
            permitted
                .get_or_insert_default()
                .extend(values.iter().map(|&value| value.to_owned()));
        }
    }

    /// The answer of the rules alone, without the value check.
    fn rules_permit_value(&self, element: &str, name: &str, value: &str) -> bool {
        match self.permitted_attrs.get(element).and_then(|a| a.get(name)) {
            None => false,
            Some(None) => true,
            Some(Some(values)) => values.contains(value),
        }
    }
}

impl DocumentSchema for XmlSchemaConstraints {
    fn permits_child(&self, parent: Option<&str>, child: &str) -> bool {
        let Some(parent) = fix_type(parent) else {
            return false;
        };
        !child.is_empty()
            && self
                .permitted_children
                .get(parent)
                .is_some_and(|permitted| permitted.contains(child))
    }

    fn permitted_characters(&self, element: Option<&str>) -> PermittedCharacters {
        fix_type(element)
            .and_then(|element| self.text_permitted.get(element).copied())
            .unwrap_or(PermittedCharacters::None)
    }

    fn permits_attribute(&self, element: &str, name: &str) -> bool {
        self.permitted_attrs
            .get(element)
            .is_some_and(|attrs| attrs.contains_key(name))
    }

    fn permits_attribute_value(&self, element: &str, name: &str, value: &str) -> bool {
        if let Some(decision) = self
            .value_check
            .and_then(|check| check(element, name, value))
        {
            return decision;
        }
        self.rules_permit_value(element, name, value)
    }

    fn required_initial_children(&self, element: Option<&str>) -> Vec<String> {
        fix_type(element)
            .and_then(|element| self.required_initial.get(element).cloned())
            .unwrap_or_default()
    }
}

/// Finds the schema of a document (`SchemaProvider`).
pub trait SchemaProvider {
    /// The schema of a document of a wavelet; `None` if the provider puts
    /// no constraints on the document (`getSchemaForId`).
    fn schema_for_id(
        &self,
        wavelet_id: &WaveletId,
        document_id: &str,
    ) -> Option<&dyn DocumentSchema>;

    /// The schema of a document, or the schema that permits anything.
    fn schema_for_id_or_default(
        &self,
        wavelet_id: &WaveletId,
        document_id: &str,
    ) -> &dyn DocumentSchema {
        self.schema_for_id(wavelet_id, document_id)
            .unwrap_or(&NO_SCHEMA_CONSTRAINTS)
    }
}

/// Several schema providers taken together (`SchemaCollection`).
#[derive(Default)]
pub struct SchemaCollection {
    providers: Vec<Box<dyn SchemaProvider + Send + Sync>>,
}

impl SchemaCollection {
    /// A collection without providers, which constrains nothing (`empty`).
    pub fn new() -> Self {
        Self::default()
    }

    /// The schemas of conversations, user-data wavelets and accounts
    /// together: every schema this module defines a lookup for.
    pub fn standard() -> Self {
        let mut schemas = Self::new();
        schemas.add(AccountSchemas);
        schemas.add(ConversationSchemas);
        schemas.add(UserDataSchemas);
        schemas
    }

    pub fn add(&mut self, provider: impl SchemaProvider + Send + Sync + 'static) {
        self.providers.push(Box::new(provider));
    }
}

impl SchemaProvider for SchemaCollection {
    fn schema_for_id(
        &self,
        wavelet_id: &WaveletId,
        document_id: &str,
    ) -> Option<&dyn DocumentSchema> {
        self.providers
            .iter()
            .find_map(|provider| provider.schema_for_id(wavelet_id, document_id))
    }
}
