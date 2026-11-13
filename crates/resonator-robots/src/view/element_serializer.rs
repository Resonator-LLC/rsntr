//! Conversion between API elements and document elements
//! (`ElementSerializer`).

use std::collections::BTreeMap;

use resonator_model::conversation::blips;
use resonator_model::data::WaveletData;
use resonator_model::doc::line_containers::LINE_TAGNAME;
use resonator_model::doc::{DocView, NodeId, XmlFragment};
use resonator_model::docop::Attributes;
use resonator_model::id::constants::ATTACHMENT_METADATA_PREFIX;

use super::api_view::ApiView;
use crate::api::{Element, ElementType};

const CAPTION_TAG: &str = "caption";
const CLICK_TAG: &str = "click";
const ATTACHMENT_ATTR: &str = "attachment";

/// What converting an `image` element with an attachment needs to know
/// about the wavelet: the XML of its attachment metadata documents
/// (`attach+<id>`), and the URL prefix of attachment downloads
/// (`ElementSerializer.attachmentDownloadHostUrl`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachmentContext {
    /// Whether there is a wavelet to look metadata up in. With one, every
    /// attachment gets the `mimeType` and `attachmentUrl` properties, null
    /// and empty if it has no metadata document (the original's wavelet
    /// hands out an empty document for any id).
    has_wavelet: bool,
    metadata: BTreeMap<String, String>,
    pub download_host_url: String,
}

impl AttachmentContext {
    /// No wavelet: attachments have no MIME type or URL properties.
    pub fn none() -> Self {
        Self::default()
    }

    /// The attachment metadata documents of a wavelet.
    pub fn of(wavelet: &WaveletData) -> Self {
        let prefix = format!("{ATTACHMENT_METADATA_PREFIX}+");
        AttachmentContext {
            has_wavelet: true,
            metadata: wavelet
                .documents()
                .filter_map(|doc| {
                    let id = doc.id().strip_prefix(&prefix)?;
                    Some((id.to_owned(), doc.content().to_xml_string()))
                })
                .collect(),
            download_host_url: String::new(),
        }
    }

    pub fn with_download_host_url(mut self, url: &str) -> Self {
        self.download_host_url = url.to_owned();
        self
    }
}

/// The document tag name of an element type (`tagNameForElementType`).
pub fn tag_name_for_element_type(element_type: ElementType) -> &'static str {
    match element_type {
        ElementType::Label => "label",
        ElementType::Input => "input",
        ElementType::Password => "password",
        ElementType::Textarea => "textarea",
        ElementType::Button => "button",
        ElementType::RadioButtonGroup => "radiogroup",
        ElementType::RadioButton => "radio",
        ElementType::Check => "check",
        ElementType::Installer => "extension_installer",
        ElementType::Gadget => "gadget",
        ElementType::Image => "img",
        ElementType::Attachment => "image",
        ElementType::Line => LINE_TAGNAME,
        ElementType::InlineBlip => blips::THREAD_INLINE_ANCHOR_TAGNAME,
    }
}

fn element_type_for_tag_name(tag: &str) -> Option<ElementType> {
    ElementType::ALL
        .into_iter()
        .find(|&t| tag_name_for_element_type(t) == tag)
}

/// Wraps content in an element; attributes without value are left out, as
/// by `XmlStringBuilder.wrap`. Attribute names are fixed and values free
/// text, so building the attributes cannot fail; if it does, the element
/// gets none.
fn wrap(content: XmlFragment, tag: &str, attributes: &[(&str, Option<&str>)]) -> XmlFragment {
    let pairs = attributes.iter().filter_map(|(k, v)| Some((*k, (*v)?)));
    content.wrap_with_attributes(tag, Attributes::from_pairs(pairs).unwrap_or_default())
}

fn is_empty_or_whitespace(value: Option<&str>) -> bool {
    value.is_none_or(|v| v.chars().all(char::is_whitespace))
}

fn value_or_default(e: &Element) -> Option<&str> {
    e.property(Element::VALUE)
        .or_else(|| e.property(Element::DEFAULT_VALUE))
}

/// The document content for an API element (`apiElementToXml`).
pub fn api_element_to_xml(e: &Element) -> XmlFragment {
    let tag = tag_name_for_element_type(e.element_type);
    let name = e.property(Element::NAME);
    let empty = XmlFragment::empty;
    match e.element_type {
        ElementType::Label => wrap(
            XmlFragment::text(value_or_default(e).unwrap_or("")),
            tag,
            &[("for", name)],
        ),
        ElementType::Input => wrap(
            XmlFragment::text(value_or_default(e).unwrap_or("")),
            tag,
            &[("name", name)],
        ),
        ElementType::Password => wrap(
            empty(),
            tag,
            &[("name", name), ("value", value_or_default(e))],
        ),
        ElementType::Textarea => {
            let mut content = empty();
            let value = e.property(Element::VALUE);
            match value {
                Some(value) if !is_empty_or_whitespace(Some(value)) => {
                    for paragraph in value.split('\n') {
                        content = content
                            .append(&empty().wrap(LINE_TAGNAME))
                            .append_text(paragraph);
                    }
                }
                _ => content = content.append(&empty().wrap(LINE_TAGNAME)),
            }
            wrap(content, tag, &[("name", name)])
        }
        ElementType::Button => {
            let content = empty()
                .append(
                    &XmlFragment::text(e.property(Element::VALUE).unwrap_or("")).wrap(CAPTION_TAG),
                )
                .append(&empty().wrap("events"));
            wrap(content, tag, &[("name", name)])
        }
        ElementType::RadioButtonGroup => wrap(empty(), tag, &[("name", name)]),
        ElementType::RadioButton => wrap(
            empty(),
            tag,
            &[("name", name), ("group", e.property(Element::VALUE))],
        ),
        ElementType::Check => wrap(
            empty(),
            tag,
            &[
                ("name", name),
                ("submit", e.property(Element::DEFAULT_VALUE)),
                ("value", e.property(Element::VALUE)),
            ],
        ),
        ElementType::Installer => wrap(empty(), tag, &[("manifest", e.property("manifest"))]),
        ElementType::Gadget => {
            let mut content = empty();
            if e.has_property("category") {
                content = content.append(&wrap(
                    empty(),
                    "category",
                    &[("name", e.property("category"))],
                ));
            }
            if e.has_property("title") {
                content =
                    content.append(&wrap(empty(), "title", &[("value", e.property("title"))]));
            }
            if e.has_property("thumbnail") {
                content = content.append(&wrap(
                    empty(),
                    "thumbnail",
                    &[("value", e.property("thumbnail"))],
                ));
            }
            // The original iterates a hash map; here properties go in key
            // order.
            for (key, value) in &e.properties {
                match key.as_str() {
                    "category" | "url" | "title" | "thumbnail" | "author" => {}
                    "pref" => {
                        content =
                            content.append(&wrap(empty(), "pref", &[("value", value.as_deref())]));
                    }
                    _ => {
                        content = content.append(&wrap(
                            empty(),
                            "state",
                            &[("name", Some(key.as_str())), ("value", value.as_deref())],
                        ));
                    }
                }
            }
            let mut attributes = vec![("url", e.property(Element::URL))];
            if e.has_property(Element::AUTHOR) {
                attributes.push(("author", e.property(Element::AUTHOR)));
            }
            if e.has_property(Element::IFRAME) {
                attributes.push(("ifr", e.property(Element::IFRAME)));
            }
            wrap(content, tag, &attributes)
        }
        ElementType::Image => wrap(
            empty(),
            tag,
            &[
                ("src", e.property(Element::URL)),
                ("width", e.property("width")),
                ("height", e.property("height")),
                ("alt", e.property(Element::CAPTION)),
            ],
        ),
        ElementType::Attachment => {
            if !e.has_property(Element::ATTACHMENT_ID) {
                return empty();
            }
            let mut content = empty();
            if let Some(caption) = e.property(Element::CAPTION) {
                content = content.append(&XmlFragment::text(caption).wrap(CAPTION_TAG));
            }
            wrap(
                content,
                tag,
                &[(ATTACHMENT_ATTR, e.property(Element::ATTACHMENT_ID))],
            )
        }
        ElementType::Line => wrap(
            empty(),
            tag,
            &[
                ("t", non_blank(e.property(Element::LINE_TYPE))),
                ("i", non_blank(e.property(Element::INDENT))),
                ("a", non_blank(e.property(Element::ALIGNMENT))),
                ("d", non_blank(e.property(Element::DIRECTION))),
            ],
        ),
        ElementType::InlineBlip => wrap(
            empty(),
            tag,
            &[(blips::THREAD_INLINE_ANCHOR_ID_ATTR, e.property("id"))],
        ),
    }
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !is_empty_or_whitespace(Some(v)))
}

/// The text of a node's first child, if that is text.
fn first_child_text(view: &DocView, element: NodeId) -> Option<&str> {
    view.text(view.first_child(element)?)
}

/// `createFormElement`: name from the `name` attribute, empty default and
/// value unless an initial value is given.
fn form_element(
    view: &DocView,
    element: NodeId,
    element_type: ElementType,
    initial_value: Option<&str>,
) -> Element {
    let mut result = Element::new(element_type);
    result.set_opt_property(Element::NAME, view.attribute(element, "name"));
    let value = initial_value.unwrap_or("");
    result.set_property(Element::DEFAULT_VALUE, value);
    result.set_property(Element::VALUE, value);
    result
}

/// Finds `key" value="<captured>"` in the XML of a metadata document, as
/// the original's regular expressions do.
fn extract_value(xml: &str, key: &str) -> Option<String> {
    let marker = format!("{key}\" value=\"");
    let start = xml.find(&marker)? + marker.len();
    let end = xml[start..].find('"')?;
    Some(xml[start..start + end].to_owned())
}

/// The API element for a document element; `None` for elements the API
/// does not know (`xmlToApiElement`).
pub fn xml_to_api_element(
    view: &DocView,
    element: NodeId,
    attachments: &AttachmentContext,
) -> Option<Element> {
    let element_type = element_type_for_tag_name(view.tag_name(element)?)?;
    let attr = |name: &str| view.attribute(element, name);
    Some(match element_type {
        ElementType::Label => {
            let mut e = form_element(view, element, element_type, None);
            e.set_opt_property(Element::NAME, attr("for"));
            if view.first_child(element).is_some() {
                let text = first_child_text(view, element);
                e.set_opt_property(Element::DEFAULT_VALUE, text);
                e.set_opt_property(Element::VALUE, text);
            }
            e
        }
        ElementType::Input => {
            let mut e = form_element(view, element, element_type, attr("submit"));
            if view.first_child(element).is_some() {
                e.set_opt_property(Element::VALUE, first_child_text(view, element));
            }
            e
        }
        ElementType::Password | ElementType::RadioButtonGroup => {
            form_element(view, element, element_type, attr("value"))
        }
        ElementType::Textarea => {
            let mut value = String::new();
            let mut first = true;
            for &node in view.children(element) {
                if let Some(text) = view.text(node) {
                    value.push_str(text);
                } else if view.has_tag(node, LINE_TAGNAME) {
                    if first {
                        first = false;
                    } else {
                        value.push('\n');
                    }
                }
            }
            form_element(view, element, element_type, Some(&value))
        }
        ElementType::Button => {
            let mut e = form_element(view, element, element_type, None);
            let first_child = view.first_child(element);
            if let Some(caption) = first_child.filter(|&c| view.has_tag(c, CAPTION_TAG))
                && view.first_child(caption).is_some()
            {
                e.set_opt_property(Element::DEFAULT_VALUE, first_child_text(view, caption));
            }
            let clicked = first_child
                .and_then(|c| view.next_sibling(c))
                .and_then(|events| view.first_child(events))
                .is_some_and(|c| view.has_tag(c, CLICK_TAG));
            if clicked {
                e.set_property(Element::VALUE, "clicked");
            } else {
                let default = e.property(Element::DEFAULT_VALUE).unwrap_or("").to_owned();
                e.set_property(Element::VALUE, default);
            }
            e
        }
        ElementType::RadioButton => {
            // FormElement(type, name, defaultValue): the group is both
            // default and value.
            let mut e = Element::new(element_type);
            e.set_opt_property(Element::NAME, attr("name"));
            e.set_opt_property(Element::DEFAULT_VALUE, attr("group"));
            e.set_opt_property(Element::VALUE, attr("group"));
            e
        }
        ElementType::Check => {
            let mut e = form_element(view, element, element_type, attr("value"));
            e.set_opt_property(Element::DEFAULT_VALUE, attr("submit"));
            e
        }
        ElementType::Installer => {
            let mut e = Element::new(element_type);
            e.set_opt_property("manifest", attr("manifest"));
            e
        }
        ElementType::Gadget => {
            let mut e = Element::new(element_type);
            e.set_opt_property(Element::URL, attr("url"));
            if let Some(author) = attr("author") {
                e.set_property(Element::AUTHOR, author);
            }
            if let Some(ifr) = attr("ifr") {
                e.set_property(Element::IFRAME, ifr);
            }
            // The original stops at the first child that is not an element.
            for &child in view.children(element) {
                if !view.is_element(child) {
                    break;
                }
                let value = view.attribute(child, "value");
                match view.tag_name(child) {
                    Some("name") => e.set_opt_property("name", value),
                    Some("title") => e.set_opt_property("title", value),
                    Some("thumbnail") => e.set_opt_property("thumbnail", value),
                    Some("pref") => e.set_opt_property("pref", value),
                    Some("state") => {
                        // A state without a name is filed under a null key
                        // in the original; it is left out here.
                        if let Some(name) = view.attribute(child, "name") {
                            e.set_opt_property(name, value);
                        }
                    }
                    Some("category") => {
                        e.set_opt_property("category", view.attribute(child, "name"));
                    }
                    _ => {}
                }
            }
            e
        }
        ElementType::Image => {
            let mut e = Element::new(element_type);
            if let Some(src) = attr("src") {
                e.set_property(Element::URL, src);
            }
            if let Some(alt) = attr("alt") {
                e.set_property(Element::CAPTION, alt);
            }
            // The original parses these as integers and fails on anything
            // else; here a non-number is left out.
            for name in ["width", "height"] {
                if let Some(number) = attr(name).and_then(|v| v.trim().parse::<i32>().ok()) {
                    e.set_property(name, number.to_string());
                }
            }
            e
        }
        ElementType::Attachment => {
            let mut e = Element::new(element_type);
            let attachment_id = attr(ATTACHMENT_ATTR);
            if let Some(id) = attachment_id {
                e.set_property(Element::ATTACHMENT_ID, id);
            }
            let caption = view
                .children(element)
                .iter()
                .find(|&&c| view.has_tag(c, CAPTION_TAG) && view.first_child(c).is_some())
                .and_then(|&c| first_child_text(view, c));
            if let Some(caption) = caption {
                e.set_property(Element::CAPTION, caption);
            }
            if let Some(id) = attachment_id.filter(|_| attachments.has_wavelet) {
                let xml = attachments.metadata.get(id).map_or("", String::as_str);
                e.set_opt_property(
                    Element::MIME_TYPE,
                    extract_value(xml, "mime_type").as_deref(),
                );
                let url = extract_value(xml, "attachment_url")
                    .map(|raw| raw.replace("&amp;", "&"))
                    .unwrap_or_default();
                e.set_property(
                    Element::ATTACHMENT_URL,
                    format!("{}{url}", attachments.download_host_url),
                );
            }
            e
        }
        ElementType::Line => {
            let mut e = Element::new(element_type);
            for (attribute, property) in [
                ("t", Element::LINE_TYPE),
                ("i", Element::INDENT),
                ("a", Element::ALIGNMENT),
                ("d", Element::DIRECTION),
            ] {
                if let Some(value) = attr(attribute) {
                    e.set_property(property, value);
                }
            }
            e
        }
        ElementType::InlineBlip => {
            let mut e = Element::new(element_type);
            e.set_opt_property("id", attr(blips::THREAD_INLINE_ANCHOR_ID_ATTR));
            e
        }
    })
}

/// The API elements of a blip document by their position in the blip's
/// text (`ElementSerializer.serialize`).
pub fn serialize(view: &DocView, attachments: &AttachmentContext) -> BTreeMap<i32, Element> {
    let mut result = BTreeMap::new();
    let api_view = ApiView::new(view, attachments);
    let Some(body) = blips::body(view) else {
        return result;
    };
    for &node in view.children(body) {
        if !view.is_element(node) {
            continue;
        }
        if let Some(element) = xml_to_api_element(view, node, attachments) {
            let offset = api_view.transform_to_text_offset(view.location(node));
            result.insert(offset as i32, element);
        }
    }
    result
}
