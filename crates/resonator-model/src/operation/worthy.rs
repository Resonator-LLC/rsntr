//! Decides whether a change is worth attributing to its author
//! (`WorthyChangeChecker`).

use crate::docop::{DocOp, DocOpComponent};

/// Tag of the inline reply anchor element, whose insertion and deletion are
/// not worthy changes.
pub const THREAD_INLINE_ANCHOR_TAGNAME: &str = "reply";

/// Annotation key prefixes whose changes are not worthy: user, spelling,
/// link, translation and language annotations.
const UNWORTHY_ANNOTATION_PREFIXES: [&str; 5] = ["user", "spell", "link", "tr", "lang"];

/// Whether a document operation is worth attributing to its author, i.e.
/// whether it makes its author a contributor and updates the blip's
/// last-modified metadata. Retains, reply anchors and changes to
/// bookkeeping annotations are not.
pub fn is_worthy(op: &DocOp) -> bool {
    op.iter().any(|component| match component {
        DocOpComponent::Retain(_)
        | DocOpComponent::ElementEnd
        | DocOpComponent::DeleteElementEnd => false,
        DocOpComponent::Characters(_)
        | DocOpComponent::DeleteCharacters(_)
        | DocOpComponent::ReplaceAttributes { .. }
        | DocOpComponent::UpdateAttributes(_) => true,
        DocOpComponent::ElementStart { tag, .. }
        | DocOpComponent::DeleteElementStart { tag, .. } => tag != THREAD_INLINE_ANCHOR_TAGNAME,
        DocOpComponent::AnnotationBoundary(map) => map.changes().iter().any(|change| {
            change.old_value != change.new_value
                && !UNWORTHY_ANNOTATION_PREFIXES
                    .iter()
                    .any(|prefix| change.key.starts_with(prefix))
        }),
    })
}

/// Whether changes to the document with this id are attributed at all:
/// false for attachment, `mini` and translation documents.
pub fn is_blip_id_worthy(doc_id: &str) -> bool {
    !(doc_id.starts_with("attach") || doc_id == "mini" || doc_id.starts_with("tr+"))
}
