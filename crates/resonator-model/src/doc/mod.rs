//! Reading a wavelet document as an element tree and editing it through
//! DOM-like calls that emit document operations.
//!
//! The original model never builds [`DocOp`](crate::docop::DocOp)s by hand:
//! it edits a `MutableDocument`, which turns each edit into a
//! non-invertible operation (`Nindo`) that the document converts into the
//! `DocOp` sent to the server. This module does the same in three layers:
//!
//! * [`DocView`] is the element tree of a document at one moment, with
//!   locations, plus the `DocHelper` queries the model needs;
//! * [`Nindo`] is the non-invertible operation, and [`Nindo::to_doc_op`]
//!   the conversion (`IndexedDocumentImpl.NonInvertibleCursor`);
//! * [`MutableDocument`] offers the edits (`MutableDocumentImpl`) over a
//!   [`DocumentSink`], which applies each resulting operation and passes it
//!   on. [`StandaloneDocument`] is a sink for a document on its own;
//!   [`crate::wavelet::OpBasedWavelet`] provides one per wavelet document.
//!
//! [`XmlFragment`] replaces `XmlStringBuilder`, and [`line_containers`]
//! holds the `LineContainers` helpers for blip bodies.
//!
//! Derived from `org.waveprotocol.wave.model.document` (`MutableDocumentImpl`,
//! `indexed.IndexedDocumentImpl`, `operation.Nindo`, `util.DocHelper`,
//! `util.LineContainers`, `util.XmlStringBuilder`, `util.Annotations`) of
//! Apache Wave, licensed under the Apache License, Version 2.0.
//!
//! Differences from the original:
//!
//! * There are no persistent node objects: a [`NodeId`] is valid only until
//!   the next edit. Not ported: `moveSiblings`, `resetAnnotationsInRange`,
//!   local (`@`) annotations, document event listeners, persistent views.
//! * A `Nindo` is not validated against a schema before conversion; the
//!   sink validates the converted operation against the document.
//! * The operations emitted are normalized. The original's are not: how
//!   it splits retains and deletions depends on the history of its DOM
//!   (adjacent text nodes are never merged again). After normalization
//!   they are equal.
//!
//! Quirks kept from the original, all visible in the emitted operations:
//!
//! * Deleting content re-annotates the deleted items: for every annotation
//!   key in use in the document (and every key the same operation has set
//!   on something before), the deletion is wrapped in a change of that key
//!   from its value on the deleted items to its value on the item to the
//!   left, even where both are absent (`"k": null -> null`).
//! * Setting an annotation to the value it already has still emits a
//!   change (`"k": "v" -> "v"`).
//! * An edit that changes nothing still emits an operation, see
//!   [`MutableDocument`].

pub mod line_containers;
mod mutable;
mod nindo;
mod view;

#[cfg(test)]
mod tests;

use thiserror::Error;

pub use mutable::{
    AnnotationReader, DocumentSink, MutableDocument, RangedAnnotation, StandaloneDocument,
};
pub use nindo::{Nindo, NindoBuilder, NindoComponent};
pub use view::{DocView, NodeId, Point, ROOT_TAG};

use crate::docop::{Attributes, DocInitialization, DocOpComponent, OperationError};
use crate::operation::WaveletOperationError;

/// Failure to read or edit a document.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DocError {
    /// A non-invertible operation does not apply to the document.
    #[error("invalid operation: {0}")]
    InvalidOperation(String),
    #[error("location {location} is outside the document of size {size}")]
    InvalidLocation { location: usize, size: usize },
    /// A node or point that does not belong to the current tree, or is of
    /// the wrong kind.
    #[error("node is not valid for this document")]
    InvalidNode,
    #[error("{0}")]
    InvalidArgument(String),
    /// The document does not have the structure the caller expects.
    #[error("unexpected document structure: {0}")]
    Structure(String),
    #[error(transparent)]
    Operation(#[from] OperationError),
    /// The wavelet rejected the operation.
    #[error(transparent)]
    Wavelet(#[from] Box<WaveletOperationError>),
}

/// One item of an [`XmlFragment`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlItem {
    Text(String),
    Start { tag: String, attributes: Attributes },
    End,
}

/// A balanced piece of document content to insert (`XmlStringBuilder`).
///
/// The original holds an XML string that is parsed when inserted; this
/// holds the parsed items, so text needs no escaping.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XmlFragment {
    items: Vec<XmlItem>,
}

impl XmlFragment {
    /// `XmlStringBuilder.createEmpty`.
    pub fn empty() -> Self {
        Self::default()
    }

    /// `XmlStringBuilder.createText`.
    pub fn text(text: &str) -> Self {
        let mut fragment = Self::default();
        fragment.push_text(text);
        fragment
    }

    /// The content of a document initialization, without its annotations.
    pub fn from_initialization(content: &DocInitialization) -> Self {
        let mut fragment = Self::default();
        for component in content.iter() {
            match component {
                DocOpComponent::Characters(chars) => fragment.push_text(chars),
                DocOpComponent::ElementStart { tag, attributes } => {
                    fragment.items.push(XmlItem::Start {
                        tag: tag.clone(),
                        attributes: attributes.clone(),
                    })
                }
                DocOpComponent::ElementEnd => fragment.items.push(XmlItem::End),
                _ => {}
            }
        }
        fragment
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(XmlItem::Text(last)) = self.items.last_mut() {
            last.push_str(text);
        } else {
            self.items.push(XmlItem::Text(text.to_owned()));
        }
    }

    /// Appends text.
    pub fn append_text(mut self, text: &str) -> Self {
        self.push_text(text);
        self
    }

    /// Appends another fragment (`append`).
    pub fn append(mut self, other: &XmlFragment) -> Self {
        for item in &other.items {
            match item {
                XmlItem::Text(text) => self.push_text(text),
                other => self.items.push(other.clone()),
            }
        }
        self
    }

    /// Wraps the fragment in an element without attributes (`wrap`).
    pub fn wrap(self, tag: &str) -> Self {
        self.wrap_with_attributes(tag, Attributes::new())
    }

    /// Wraps the fragment in an element.
    pub fn wrap_with_attributes(mut self, tag: &str, attributes: Attributes) -> Self {
        self.items.insert(
            0,
            XmlItem::Start {
                tag: tag.to_owned(),
                attributes,
            },
        );
        self.items.push(XmlItem::End);
        self
    }

    pub fn items(&self) -> &[XmlItem] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Number of document items the fragment inserts (`getLength`).
    pub fn item_count(&self) -> usize {
        self.items
            .iter()
            .map(|item| match item {
                XmlItem::Text(text) => text.encode_utf16().count(),
                _ => 1,
            })
            .sum()
    }
}

impl XmlFragment {
    /// Parses a piece of XML: elements with attributes and text
    /// (`XmlStringBuilder.createFromXmlString`). The five predefined
    /// entities and numeric character references are understood; comments,
    /// processing instructions, CDATA sections and doctypes are rejected.
    pub fn parse(xml: &str) -> Result<XmlFragment, DocError> {
        let bad = |message: &str| DocError::InvalidArgument(format!("Ill-formed xml: {message}"));
        let mut fragment = XmlFragment::default();
        let mut open: Vec<String> = Vec::new();
        let mut rest = xml;
        while !rest.is_empty() {
            let Some(after_lt) = rest.strip_prefix('<') else {
                let end = rest.find('<').unwrap_or(rest.len());
                fragment.push_text(&unescape_xml(&rest[..end]).ok_or_else(|| bad("bad entity"))?);
                rest = &rest[end..];
                continue;
            };
            let end = after_lt.find('>').ok_or_else(|| bad("unterminated tag"))?;
            let tag = &after_lt[..end];
            rest = &after_lt[end + 1..];
            if let Some(name) = tag.strip_prefix('/') {
                if open.pop().as_deref() != Some(name.trim_end()) {
                    return Err(bad("mismatched end tag"));
                }
                fragment.items.push(XmlItem::End);
                continue;
            }
            if tag.starts_with('!') || tag.starts_with('?') {
                return Err(bad("unsupported markup"));
            }
            let (tag, self_closing) = match tag.strip_suffix('/') {
                Some(tag) => (tag, true),
                None => (tag, false),
            };
            let name_end = tag.find(char::is_whitespace).unwrap_or(tag.len());
            let name = &tag[..name_end];
            if !crate::docop::utf16::is_xml_name(name) {
                return Err(bad("bad element name"));
            }
            let mut attributes: Vec<(String, String)> = Vec::new();
            let mut attrs = tag[name_end..].trim_start();
            while !attrs.is_empty() {
                let eq = attrs
                    .find('=')
                    .ok_or_else(|| bad("attribute without value"))?;
                let attr_name = attrs[..eq].trim_end();
                if !crate::docop::utf16::is_xml_name(attr_name) {
                    return Err(bad("bad attribute name"));
                }
                let value = attrs[eq + 1..].trim_start();
                let quote = value.chars().next().filter(|c| matches!(c, '"' | '\''));
                let quote = quote.ok_or_else(|| bad("unquoted attribute value"))?;
                let close = value[1..]
                    .find(quote)
                    .ok_or_else(|| bad("unterminated attribute value"))?;
                let unescaped =
                    unescape_xml(&value[1..1 + close]).ok_or_else(|| bad("bad entity"))?;
                if attributes.iter().any(|(n, _)| n == attr_name) {
                    return Err(bad("duplicate attribute"));
                }
                attributes.push((attr_name.to_owned(), unescaped));
                attrs = value[close + 2..].trim_start();
            }
            fragment.items.push(XmlItem::Start {
                tag: name.to_owned(),
                attributes: nindo::attributes_from_pairs(
                    attributes.iter().map(|(n, v)| (n.as_str(), v.as_str())),
                ),
            });
            if self_closing {
                fragment.items.push(XmlItem::End);
            } else {
                open.push(name.to_owned());
            }
        }
        if !open.is_empty() {
            return Err(bad("unclosed element"));
        }
        Ok(fragment)
    }
}

fn unescape_xml(text: &str) -> Option<String> {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        result.push_str(&rest[..amp]);
        let semi = rest[amp..].find(';')?;
        let entity = &rest[amp + 1..amp + semi];
        result.push(match entity {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = entity.strip_prefix('#')?;
                let value = match code.strip_prefix('x') {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => code.parse().ok()?,
                };
                char::from_u32(value)?
            }
        });
        rest = &rest[amp + semi + 1..];
    }
    result.push_str(rest);
    Some(result)
}
