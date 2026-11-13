//! Document schema constraints consulted during validation.

use super::utf16::{
    BlipCodePointResult, REPLACEMENT_CHARACTER, is_code_point_good_for_blip, is_code_point_valid,
};

/// Constraint on the text allowed directly inside an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermittedCharacters {
    /// No characters permitted.
    None,
    /// Only "blip text" characters permitted.
    BlipText,
    /// Any valid Unicode.
    Any,
}

impl PermittedCharacters {
    /// Converts any string into one that satisfies this constraint, or
    /// `None` if no text is permitted at all.
    ///
    /// Deviation from the original: supplementary code points are kept
    /// rather than replaced by U+FFFD.
    pub fn coerce_string(self, string: &str) -> Option<String> {
        match self {
            PermittedCharacters::None => None,
            PermittedCharacters::BlipText => Some(
                string
                    .chars()
                    .flat_map(|c| {
                        let (c, count) = match c {
                            '\t' => (' ', 4),
                            '\n' | '\r' => (' ', 1),
                            c if is_code_point_good_for_blip(c) == BlipCodePointResult::Ok => {
                                (c, 1)
                            }
                            _ => (REPLACEMENT_CHARACTER, 1),
                        };
                        std::iter::repeat_n(c, count)
                    })
                    .collect(),
            ),
            PermittedCharacters::Any => Some(
                string
                    .chars()
                    .map(|c| {
                        if is_code_point_valid(c) {
                            c
                        } else {
                            REPLACEMENT_CHARACTER
                        }
                    })
                    .collect(),
            ),
        }
    }
}

/// Queries on what the XML schema of a document allows. `None` as an element
/// type stands for the top level of the document.
pub trait DocumentSchema {
    /// Whether `child` elements may occur as children of `parent`.
    fn permits_child(&self, parent: Option<&str>, child: &str) -> bool;

    /// What text is permitted directly inside elements of type `element`.
    fn permitted_characters(&self, element: Option<&str>) -> PermittedCharacters;

    /// Whether `element` permits an attribute named `name`.
    fn permits_attribute(&self, element: &str, name: &str) -> bool;

    /// Whether `element` permits attribute `name` with the given value.
    fn permits_attribute_value(&self, element: &str, name: &str, value: &str) -> bool;

    /// Element types that must occur, in this order, at the start of
    /// `element`. Validation supports at most one required child and
    /// ignores any further entries (the original throws in that case).
    fn required_initial_children(&self, element: Option<&str>) -> Vec<String>;
}

/// A schema that permits anything.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSchemaConstraints;

/// A schema that permits anything.
pub static NO_SCHEMA_CONSTRAINTS: NoSchemaConstraints = NoSchemaConstraints;

impl DocumentSchema for NoSchemaConstraints {
    fn permits_child(&self, _parent: Option<&str>, _child: &str) -> bool {
        true
    }

    fn permitted_characters(&self, _element: Option<&str>) -> PermittedCharacters {
        PermittedCharacters::Any
    }

    fn permits_attribute(&self, _element: &str, _name: &str) -> bool {
        true
    }

    fn permits_attribute_value(&self, _element: &str, _name: &str, _value: &str) -> bool {
        true
    }

    fn required_initial_children(&self, _element: Option<&str>) -> Vec<String> {
        Vec::new()
    }
}
