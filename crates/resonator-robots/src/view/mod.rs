//! How a blip document appears to the API: flattened to text, elements
//! and annotations, with positions that map back to the document.
//!
//! Derived from `com.google.wave.api.data` of Apache Wave (`ApiView`,
//! `DocumentHitIterator`, `ElementSerializer`), licensed under the Apache
//! License, Version 2.0.

mod api_view;
mod element_serializer;
mod hit_iterator;

pub use api_view::{ApiView, ElementInfo};
pub use element_serializer::{
    AttachmentContext, api_element_to_xml, serialize, tag_name_for_element_type, xml_to_api_element,
};
pub use hit_iterator::DocumentHitIterator;

#[cfg(test)]
mod tests;
