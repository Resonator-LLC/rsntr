//! The title of a wave, kept as an annotation on its root blip
//! (`TitleHelper`).
//!
//! The annotation `conv/title` either carries the title itself (an
//! explicit title, set on the first item of the document) or is the empty
//! string over the range of text that is the title (an implicit title:
//! normally the first line or sentence).

use super::blips;
use crate::doc::line_containers::LINE_TAGNAME;
use crate::doc::{AnnotationReader, DocError, DocView, MutableDocument};
use crate::docop::{
    AnnotationBoundaryMap, Attributes, BootstrapDocument, DocInitialization, DocOpBuilder,
};

/// The annotation key of the title.
pub const TITLE_KEY: &str = "conv/title";

/// The annotation value that marks a range as an implicit title.
pub const AUTO_VALUE: &str = "";

/// A blip document with one empty line that is marked as implicit title:
/// `<body><line/></body>` with `conv/title=""` from inside the body to the
/// end of the document (`TitleHelper.emptyDocumentWithTitle`).
pub fn empty_document_with_title() -> DocInitialization {
    let start = AnnotationBoundaryMap::changes_only([(TITLE_KEY, None, Some(AUTO_VALUE))])
        .unwrap_or_default();
    let end = AnnotationBoundaryMap::ends([TITLE_KEY]).unwrap_or_default();
    DocOpBuilder::new()
        .element_start(blips::BODY_TAGNAME, Attributes::new())
        .annotation_boundary(start)
        .element_start(LINE_TAGNAME, Attributes::new())
        .element_end()
        .element_end()
        .annotation_boundary(end)
        .build_initialization()
        .unwrap_or_default()
}

/// The title of a document: the explicit title if there is one, else the
/// text of the implicit title range, else the empty string
/// (`TitleHelper.extractTitle`).
pub fn extract_title(doc: &BootstrapDocument) -> String {
    let size = DocView::new(doc).size();
    let Some(start) = doc.find_annotation_change(0, size, TITLE_KEY, None) else {
        return String::new();
    };
    match doc.annotation(start, TITLE_KEY) {
        Some(explicit) if !explicit.is_empty() => explicit,
        _ => {
            let end = doc
                .find_annotation_change(start, size, TITLE_KEY, Some(AUTO_VALUE))
                .unwrap_or(size);
            DocView::new(doc).text_between(start, end)
        }
    }
}

/// Whether the document has a non-empty explicit title
/// (`TitleHelper.hasExplicitTitle`).
pub fn has_explicit_title(doc: &BootstrapDocument) -> bool {
    let size = DocView::new(doc).size();
    doc.find_annotation_change(0, size, TITLE_KEY, None)
        .and_then(|start| doc.annotation(start, TITLE_KEY))
        .is_some_and(|value| !value.is_empty())
}

/// Sets an explicit title, replacing any title there was
/// (`TitleHelper.setExplicitTitle`). The document must not be empty.
pub fn set_explicit_title(doc: &mut MutableDocument<'_>, title: &str) -> Result<(), DocError> {
    if doc.size() == 0 {
        return Err(DocError::InvalidArgument(
            "Cannot set title on empty document".to_owned(),
        ));
    }
    doc.reset_annotation(0, 1, TITLE_KEY, Some(title))
}

/// Marks a range as the implicit title, unless it already is
/// (`TitleHelper.setImplicitTitle`).
pub fn set_implicit_title(
    doc: &mut MutableDocument<'_>,
    start: usize,
    end: usize,
) -> Result<(), DocError> {
    if start >= end {
        return Err(DocError::InvalidArgument(
            "Implicit title range is invalid".to_owned(),
        ));
    }
    doc.guarded_reset_annotation(start, end, TITLE_KEY, Some(AUTO_VALUE))
        .map(drop)
}

/// The range an implicit title should cover: from the first line element
/// of the body to the end of the first sentence (the whitespace after `.`,
/// `!` or `?`) or the start of the second line, whichever comes first, or
/// else almost to the end of the document
/// (`TitleHelper.findImplicitTitle`).
pub fn find_implicit_title(view: &DocView) -> Option<(usize, usize)> {
    let body = blips::body(view)?;
    let mut after_punctuation = false;
    let mut start: Option<usize> = None;
    let mut end = view.size().checked_sub(1)?;
    // From the body to the end of the document, as the original's
    // iteration is not confined to the body.
    let nodes = view.descendants(view.root());
    'outer: for &node in nodes.iter().skip_while(|&&node| node != body) {
        if let Some(text) = view.text(node) {
            if start.is_none() {
                continue;
            }
            for (index, unit) in text.encode_utf16().enumerate() {
                let is = |c: char| unit == c as u16;
                if is('.') || is('!') || is('?') {
                    after_punctuation = true;
                } else if after_punctuation && is(' ') {
                    end = view.location(node) + index;
                    break 'outer;
                } else {
                    after_punctuation = false;
                }
            }
        } else if view.has_tag(node, LINE_TAGNAME) {
            if start.is_some() {
                end = view.location(node);
                break;
            }
            start = Some(view.location(node));
        }
    }
    let start = start?;
    (start > 0 && end > start).then_some((start, end))
}

/// Sets the implicit title to the range [`find_implicit_title`] finds,
/// unless the document has an explicit title
/// (`TitleHelper.maybeFindAndSetImplicitTitle`).
pub fn maybe_find_and_set_implicit_title(doc: &mut MutableDocument<'_>) -> Result<(), DocError> {
    if has_explicit_title(doc.document()) {
        return Ok(());
    }
    if let Some((start, end)) = find_implicit_title(doc.view()) {
        set_implicit_title(doc, start, end)?;
    }
    Ok(())
}
