//! The structure of blip documents (`Blips`).

use crate::doc::line_containers::LINE_TAGNAME;
use crate::doc::{DocView, NodeId, XmlFragment};

/// Prefix of the conversation model's annotation keys.
pub const ANNOTATION_PREFIX: &str = "conv";
pub const BODY_TAGNAME: &str = "body";
pub const HEAD_TAGNAME: &str = "head";
/// The element in a blip that anchors an inline reply thread.
pub const THREAD_INLINE_ANCHOR_TAGNAME: &str = "reply";
/// The attribute of an anchor holding the id of its thread.
pub const THREAD_INLINE_ANCHOR_ID_ATTR: &str = "id";

/// The content a new blip starts with: `<body><line/></body>`
/// (`Blips.INITIAL_CONTENT`).
pub fn initial_content() -> XmlFragment {
    XmlFragment::empty().wrap(LINE_TAGNAME).wrap(BODY_TAGNAME)
}

/// The body element of a blip document (`Blips.getBody`).
pub fn body(view: &DocView) -> Option<NodeId> {
    view.element_with_tag_name(BODY_TAGNAME, view.root())
}
