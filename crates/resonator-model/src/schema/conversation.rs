//! Schemas of conversation documents (`ConversationSchemas`).

use std::sync::LazyLock;

use super::{
    BOOLEAN_VALUES, SchemaProvider, XmlSchemaConstraints, is_non_negative_integer,
    is_positive_integer, is_valid_integer,
};
use crate::adt::Serializable;
use crate::docop::DocumentSchema;
use crate::id::WaveletId;
use crate::id::constants::MANIFEST_DOCUMENT_ID;
use crate::id::util::{is_blip_id, is_conversational_id};

fn line_container(s: &mut XmlSchemaConstraints, element: &str) {
    s.add_children(
        Some(element),
        &[
            "line",
            "image",
            "gadget",
            "eqn",
            "experimental",
            "mediasearch",
            "img",
            "reply",
            "profile",
        ],
    );
    s.contains_blip_text(&[element]);
    s.add_required_initial(element, &["line"]);
}

fn one_liner(s: &mut XmlSchemaConstraints, element: &str) {
    s.contains_blip_text(&[element]);
}

fn contains_form_elements(s: &mut XmlSchemaConstraints, element: &str) {
    s.add_children(
        Some(element),
        &[
            "button",
            "check",
            "input",
            "label",
            "password",
            "radiogroup",
            "radio",
            "textarea",
        ],
    );
}

fn blip_value_check(tag: &str, attr: &str, value: &str) -> Option<bool> {
    match (tag, attr) {
        ("line", "i") => Some(is_positive_integer(value)),
        ("img", "width" | "height") => Some(is_valid_integer(value, 0)),
        ("lmt", "t") => Some(is_non_negative_integer(value)),
        ("invitation", "remaining") => Some(is_non_negative_integer(value)),
        _ => None,
    }
}

/// Schema of a blip document (`BLIP_SCHEMA_CONSTRAINTS`,
/// `DefaultDocumentSchema`): an optional `head`, and a `body` of lines of
/// text with inline elements. A `body` (like `textarea` and the other line
/// containers) must start with a `line` element.
pub static BLIP_SCHEMA_CONSTRAINTS: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &["head"]);
    s.add_children(Some("head"), &["timestamp"]);
    s.add_children(Some("timestamp"), &["lmt"]);
    s.add_attrs("lmt", &["t"]);

    s.add_children(None, &["body"]);
    line_container(&mut s, "body");

    s.add_attr_with_values("line", "t", &["h1", "h2", "h3", "h4", "li"]);
    s.add_attr_with_values("line", "listyle", &["decimal"]);
    s.add_attr_with_values("line", "a", &["l", "r", "c", "j"]);
    s.add_attr_with_values("line", "d", &["l", "r"]);
    s.add_attr_with_values("line", "i", &[]);

    s.add_children(Some("image"), &["caption"]);
    s.add_attr_with_values("image", "attachment", &[]);
    s.add_attr_with_values("image", "style", &["full"]);
    s.add_children(Some("image"), &["gadget"]);
    one_liner(&mut s, "caption");
    s.add_children(Some("caption"), &["reply"]);

    one_liner(&mut s, "label");
    one_liner(&mut s, "input");

    s.add_attrs("reply", &["id"]);

    contains_form_elements(&mut s, "body");
    line_container(&mut s, "textarea");

    s.add_attrs("button", &["name"]);
    s.add_children(Some("button"), &["caption", "events"]);
    s.add_children(Some("events"), &["click"]);
    s.add_attrs("click", &["time", "clicker"]);
    s.add_attrs("check", &["name", "submit", "value"]);
    s.add_attrs("radiogroup", &["name", "submit", "value"]);
    s.add_attrs("password", &["name", "submit", "value"]);
    s.add_attrs("textarea", &["name", "submit", "value"]);
    s.add_attrs("input", &["name", "submit"]);
    s.add_attrs("radio", &["name", "group"]);
    s.add_attrs("click", &["time", "clicker"]);
    s.add_attrs("label", &["for"]);

    s.add_children(
        Some("gadget"),
        &["title", "thumbnail", "category", "state", "pref"],
    );
    s.add_attrs(
        "gadget",
        &[
            "url",
            "title",
            "prefs",
            "state",
            "author",
            "height",
            "width",
            "id",
            "extension",
            "ifr",
            "snippet",
            // Wave apps: `app` is the bundle's `sha256:<hex>`, `bundle` its
            // attachment id.
            "app",
            "bundle",
        ],
    );
    for gadget_el in ["category", "state", "pref"] {
        s.add_attrs(gadget_el, &["name"]);
    }
    for gadget_el in ["title", "thumbnail", "state", "pref"] {
        s.add_attrs(gadget_el, &["value"]);
    }

    s.add_children(Some("profile"), &["profile-field", "gadget"]);
    s.add_attrs("profile-field", &["name", "user-set"]);
    s.add_attrs("profile", &["avatar-url"]);
    s.contains_blip_text(&["profile-field"]);

    s.add_children(Some("mediasearch"), &["result", "customsearch"]);
    s.add_attrs(
        "mediasearch",
        &["page", "corpora", "query", "selected", "pending", "lang"],
    );
    s.add_attrs(
        "result",
        &[
            "thumbnail",
            "thumbwidth",
            "thumbheight",
            "content",
            "url",
            "dispurl",
            "title",
            "snippet",
            "num",
            "type",
            "disphtml",
        ],
    );
    s.add_attrs(
        "customsearch",
        &[
            "name",
            "icon",
            "shortname",
            "resultrows",
            "resultcols",
            "addmethod",
        ],
    );

    s.add_children(Some("body"), &["trustreq"]);
    s.contains_blip_text(&["trustreq"]);
    s.add_children(Some("trustreq"), &["trwave"]);
    s.add_attrs("trustreq", &["from", "numberOfWaves", "userAction"]);
    s.add_attrs("trwave", &["messageCount", "lastModified"]);
    s.contains_blip_text(&["trwave"]);

    s.add_children(Some("body"), &["blacklist"]);
    s.add_attrs("blacklist", &["address", "contacts"]);

    s.add_children(Some("body"), &["invitation"]);
    s.add_attrs("invitation", &["remaining", "title", "invitedString"]);
    s.add_children(Some("invitation"), &["invited"]);
    s.add_attrs("invited", &["address"]);

    s.add_attr_with_values("eqn", "format", &["tex"]);
    s.contains_blip_text(&["eqn"]);

    s.add_children(Some("body"), &["settings"]);
    s.add_attrs("settings", &["name"]);
    s.add_children(
        Some("settings"),
        &[
            "bool-setting",
            "radio-setting",
            "text-setting",
            "listbox-setting",
        ],
    );
    s.add_attrs("bool-setting", &["id", "live-value", "saved-value"]);
    s.add_attrs("radio-setting", &["id", "live-value", "saved-value"]);
    s.add_attrs("listbox-setting", &["id", "live-value", "saved-value"]);
    s.add_attrs("text-setting", &["id", "saved-value"]);
    one_liner(&mut s, "text-setting");

    s.add_children(Some("body"), &["html"]);
    s.add_children(Some("html"), &["data"]);
    s.contains_blip_text(&["data"]);

    s.add_children(Some("body"), &["experimental"]);
    s.add_attrs("experimental", &["url"]);
    s.add_children(Some("experimental"), &["namevaluepair", "part"]);
    s.add_attrs("part", &["id"]);
    line_container(&mut s, "part");
    contains_form_elements(&mut s, "part");
    s.add_attrs("namevaluepair", &["name"]);
    s.add_attrs("namevaluepair", &["value"]);

    s.add_children(Some("body"), &["translation"]);
    s.add_children(Some("translation"), &["stanza"]);
    line_container(&mut s, "stanza");
    s.add_attrs("stanza", &["lang", "users"]);

    s.add_children(Some("body"), &["extension_installer"]);
    s.add_attrs("extension_installer", &["manifest", "img", "installed"]);

    s.add_children(Some("body"), &["ext-settings"]);
    s.add_attrs("ext-settings", &["manifest", "enabled"]);

    s.add_children(Some("body"), &["gadget-settings"]);
    s.add_attrs("gadget-settings", &["url", "prefs"]);

    s.add_attrs("img", &["alt", "height", "width", "src"]);

    s.add_children(Some("body"), &["quote"]);
    line_container(&mut s, "quote");

    s.set_value_check(blip_value_check);
    s
});

fn manifest_value_check(tag: &str, attr: &str, value: &str) -> Option<bool> {
    match (tag, attr) {
        ("conversation", "anchorWavelet") => Some(
            WaveletId::from_attribute(value)
                .is_some_and(|id| is_conversational_id(&id).unwrap_or(false)),
        ),
        ("conversation", "anchorManifestOffset" | "anchorVersion" | "anchorOffset") => {
            Some(is_non_negative_integer(value))
        }
        ("conversation", "anchorBlip") => Some(is_blip_id(value)),
        ("blip", "id") => Some(is_blip_id(value)),
        _ => None,
    }
}

/// Schema of the conversation manifest, the `conversation` document
/// (`MANIFEST_SCHEMA_CONSTRAINTS`): a `conversation` element of nested
/// `blip` and `thread` elements. Blip ids must be blip ids, and the anchor
/// wavelet a conversational wavelet id in the form `domain/id`.
pub static MANIFEST_SCHEMA_CONSTRAINTS: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &["conversation"]);
    s.add_attrs(
        "conversation",
        &[
            "anchorWavelet",
            "anchorManifestOffset",
            "anchorVersion",
            "anchorBlip",
            "anchorOffset",
            "sort",
        ],
    );
    s.add_children(Some("conversation"), &["blip"]);
    s.add_attrs("blip", &["id"]);
    s.add_attr_with_values("blip", "deleted", &BOOLEAN_VALUES);
    s.add_children(Some("blip"), &["thread"]);
    s.add_children(Some("blip"), &["peer"]);
    s.add_attrs("thread", &["id"]);
    s.add_attr_with_values("thread", "inline", &BOOLEAN_VALUES);
    s.add_children(Some("thread"), &["blip"]);
    s.add_attrs("peer", &["id"]);
    s.set_value_check(manifest_value_check);
    s
});

/// The schemas of the documents of conversational wavelets: blips and the
/// manifest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConversationSchemas;

impl SchemaProvider for ConversationSchemas {
    fn schema_for_id(
        &self,
        wavelet_id: &WaveletId,
        document_id: &str,
    ) -> Option<&dyn DocumentSchema> {
        if !is_conversational_id(wavelet_id).unwrap_or(false) {
            return None;
        }
        if is_blip_id(document_id) {
            Some(&*BLIP_SCHEMA_CONSTRAINTS)
        } else if document_id == MANIFEST_DOCUMENT_ID {
            Some(&*MANIFEST_SCHEMA_CONSTRAINTS)
        } else {
            None
        }
    }
}
