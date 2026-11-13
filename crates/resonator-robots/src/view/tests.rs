//! Ports of `ApiViewTest`, `DocumentHitIteratorTest` and
//! `ElementSerializerTest`.

use std::collections::BTreeMap;

use resonator_model::doc::{DocView, StandaloneDocument, XmlFragment};
use resonator_model::docop::BootstrapDocument;

use super::*;
use crate::api::{Element, ElementType, Range};

/// A document `<body><line/>` + content + `</body>`
/// (`LineContainers.debugContainerWrap`).
fn document(content: &str) -> StandaloneDocument {
    let mut document = StandaloneDocument::new(BootstrapDocument::new());
    let xml = XmlFragment::parse(&format!("<body><line/>{content}</body>")).unwrap();
    document.mutable().append_xml(&xml).unwrap();
    document
}

fn view_of(document: &mut StandaloneDocument) -> ApiView {
    ApiView::new(document.mutable().view(), &AttachmentContext::none())
}

fn image() -> Element {
    Element::with_properties(
        ElementType::Image,
        [("attachmentId", "id1"), ("caption", "caption")],
    )
}

/// The view agrees with one made afresh from the document.
fn assert_in_sync(document: &mut StandaloneDocument, api: &ApiView) {
    let fresh = view_of(document);
    assert_eq!(fresh.api_contents(), api.api_contents());
    let positions = |view: &ApiView| -> Vec<(ElementType, usize, usize)> {
        view.elements()
            .into_iter()
            .map(|info| {
                (
                    info.element.element_type,
                    info.api_position,
                    info.xml_position,
                )
            })
            .collect()
    };
    assert_eq!(positions(&fresh), positions(api));
}

macro_rules! edit {
    ($document:expr, $api:expr, $method:ident($($arg:expr),*)) => {{
        let mut doc = $document.mutable();
        $api.$method(&mut doc, $($arg),*).unwrap();
        drop(doc);
        assert_in_sync(&mut $document, &$api);
    }};
}

#[test]
fn string_append() {
    let mut document = document("");
    let mut api = view_of(&mut document);
    edit!(document, api, insert_text(1, "world"));
    assert_eq!("\nworld", api.api_contents());
    edit!(document, api, insert_text(1, "hello "));
    assert_eq!("\nhello world", api.api_contents());
}

#[test]
fn string_replace() {
    let mut document = document("");
    let mut api = view_of(&mut document);
    edit!(document, api, insert_text(1, "22 a b 22 c d 22 e f"));
    while let Some(p) = api.api_contents().find("22") {
        edit!(document, api, insert_text(p, "q"));
        edit!(document, api, delete(p + 1, p + 3));
    }
    assert_eq!("\nq a b q c d q e f", api.api_contents());
}

#[test]
fn shift() {
    let mut document = document("");
    let mut api = view_of(&mut document);
    edit!(document, api, insert_text(1, "0123456789"));
    edit!(document, api, insert_element(4, &image()));
    edit!(document, api, insert_element(8, &image()));
    edit!(document, api, delete(2, 3));
    let elements = api.elements();
    assert_eq!(3, elements.len());
    // A range that ends before it starts deletes nothing.
    edit!(
        document,
        api,
        delete(elements[1].api_position, elements[0].api_position + 1)
    );
    assert_eq!(3, api.elements().len());
}

#[test]
fn compose_doc_using_append() {
    let mut document = document("");
    let mut api = view_of(&mut document);
    edit!(document, api, insert_text(1, "hello"));
    edit!(document, api, insert_element(1, &image()));
    assert_eq!(2, api.elements().len());
    assert_eq!("\n hello", api.api_contents());
    edit!(document, api, insert_text(api.len(), " world"));
    assert_eq!("\n hello world", api.api_contents());
    edit!(document, api, insert_element(api.len(), &image()));
    edit!(document, api, insert_text(api.len(), "!"));
    assert_eq!("\n hello world !", api.api_contents());
}

#[test]
fn delete_string() {
    let mut document = document("");
    let mut api = view_of(&mut document);
    edit!(document, api, insert_text(1, "helllo"));
    edit!(document, api, delete(3, 4));
    assert_eq!("\nhello", api.api_contents());
    edit!(document, api, delete(0, 2));
    assert_eq!("ello", api.api_contents());
}

#[test]
fn insert_and_delete_element_in_string() {
    let mut document = document("");
    let mut api = view_of(&mut document);
    edit!(document, api, insert_text(1, "hello"));
    edit!(document, api, insert_element(3, &image()));
    assert_eq!("\nhe llo", api.api_contents());
    edit!(document, api, delete(3, 5));
    assert_eq!("\nhelo", api.api_contents());
}

#[test]
fn insert_text_with_newline() {
    let mut document = document("");
    let mut api = view_of(&mut document);
    edit!(document, api, insert_text(1, "hello\nworld"));
    assert_eq!("\nhello\nworld", api.api_contents());
    let elements = api.elements();
    assert_eq!(2, elements.len());
    assert!(
        elements
            .iter()
            .all(|info| info.element.element_type == ElementType::Line)
    );
}

#[test]
fn insert_before_element_then_delete_element() {
    let mut document = document("");
    let mut api = view_of(&mut document);
    edit!(document, api, insert_text(1, "0123456789"));
    edit!(document, api, insert_element(4, &image()));
    edit!(document, api, insert_text(4, "4"));
    edit!(document, api, delete(5, 6));
    assert_eq!(1, api.elements().len());
    assert_eq!("\n01243456789", api.api_contents());
}

/// Where the original loses track: text put directly before an element is
/// then found where it is, and a deletion inside text does not leave the
/// old length behind.
#[test]
fn positions_after_edits() {
    let mut document = document("abc<gadget></gadget>def");
    let mut api = view_of(&mut document);
    edit!(document, api, insert_text(4, "XY"));
    assert_eq!("\nabcXY def", api.api_contents());
    // (The original would say 4 and 5.)
    assert_eq!(Some(6), api.transform_to_xml_offset(4));
    assert_eq!(Some(7), api.transform_to_xml_offset(5));
    edit!(document, api, delete(8, 10));
    edit!(document, api, insert_text(api.len(), "!"));
    assert_eq!("\nabcXY d!", api.api_contents());
    assert_eq!(
        "<body><line/>abcXY<gadget/>d!</body>",
        document.mutable().to_xml_string()
    );
}

#[test]
fn positions_outside_the_text() {
    let mut document = document("abc");
    let mut api = view_of(&mut document);
    // Past the end: appended.
    edit!(document, api, insert_text(100, "d"));
    assert_eq!("\nabcd", api.api_contents());
    // A start past the end deletes nothing; an end past it is an error.
    edit!(document, api, delete(50, 60));
    assert!(api.delete(&mut document.mutable(), 2, 60).is_err());
    // A blip without a body has no positions.
    let mut empty = StandaloneDocument::new(BootstrapDocument::new());
    let mut api = view_of(&mut empty);
    assert!(api.is_empty());
    assert_eq!(None, api.transform_to_xml_offset(0));
    assert!(api.insert_text(&mut empty.mutable(), 0, "x").is_err());
    assert!(
        api.insert_element(&mut empty.mutable(), 0, &Element::line())
            .is_err()
    );
}

#[test]
fn transform_to_xml_offset() {
    let mut document = document("some text<gadget></gadget>");
    let mut api = view_of(&mut document);
    edit!(document, api, insert_element(3, &image()));
    for info in api.elements() {
        assert_eq!(
            Some(info.xml_position),
            api.transform_to_xml_offset(info.api_position)
        );
    }
}

#[test]
fn locate_element() {
    let mut document = document("01234567890123456789");
    let mut api = view_of(&mut document);
    let button = |name: &str| Element::form(ElementType::Button, name, "", "");
    edit!(document, api, insert_element(3, &button("buttonName")));
    assert_eq!(Some(3), api.locate_element(&button("buttonName")));
    assert_eq!(None, api.locate_element(&button("notInDocument")));
    edit!(
        document,
        api,
        insert_element(4, &Element::gadget("http://test.com"))
    );
    assert_eq!(
        Some(4),
        api.locate_element(&Element::gadget("http://test.com"))
    );
    assert_eq!(
        None,
        api.locate_element(&Element::gadget("http://test.com/something"))
    );
    let inline_blip = Element::with_properties(ElementType::InlineBlip, [("id", "b+1234")]);
    edit!(document, api, insert_element(5, &inline_blip));
    assert_eq!(Some(5), api.locate_element(&inline_blip));
}

#[test]
fn transform_to_text_offset() {
    let mut document = document("123<gadget><state>foo</state></gadget>456");
    let api = view_of(&mut document);
    let expected = [0, 0, 0, 1, 2, 3, 4, 4, 4, 4, 4, 4, 4, 5, 6, 7, 8, 8];
    for (xml_offset, text_offset) in expected.into_iter().enumerate() {
        assert_eq!(
            text_offset,
            api.transform_to_text_offset(xml_offset),
            "at {xml_offset}"
        );
    }
}

#[test]
fn positions_are_utf16_code_units() {
    let mut document = document("a\u{1F600}b");
    let mut api = view_of(&mut document);
    assert_eq!(5, api.len());
    edit!(document, api, insert_text(4, "X"));
    assert_eq!("\na\u{1F600}Xb", api.api_contents());
    edit!(document, api, delete(2, 4));
    assert_eq!("\naXb", api.api_contents());
}

// DocumentHitIteratorTest

fn starts(hits: &mut DocumentHitIterator, view: &ApiView) -> Vec<i32> {
    std::iter::from_fn(|| hits.next(view))
        .map(|range| range.start)
        .collect()
}

#[test]
fn singleshot() {
    let mut document = document("");
    let view = view_of(&mut document);
    let range = Range::new(0, 10);
    let mut hits = DocumentHitIterator::singleshot(range);
    assert_eq!(Some(range), hits.next(&view));
    assert_eq!(None, hits.next(&view));
}

#[test]
fn text_matcher_without_shift() {
    let mut document = document("1 1 1");
    let view = view_of(&mut document);
    let mut hits = DocumentHitIterator::text_matcher("1", -1);
    for i in 0..3 {
        assert_eq!(Some(Range::new(i * 2 + 1, i * 2 + 2)), hits.next(&view));
    }
    assert_eq!(None, hits.next(&view));
    let mut limited = DocumentHitIterator::text_matcher("1", 2);
    assert_eq!(vec![1, 3], starts(&mut limited, &view));
}

#[test]
fn text_matcher_deleting_the_first_char() {
    let mut document = document("1 1 1");
    let mut view = view_of(&mut document);
    let mut hits = DocumentHitIterator::text_matcher("1", -1);
    for i in 0..3 {
        assert_eq!(Some(i + 1), hits.next(&view).map(|range| range.start));
        view.delete(&mut document.mutable(), 1, 2).unwrap();
        hits.shift(1, -1);
    }
    assert_eq!(None, hits.next(&view));
}

#[test]
fn text_matcher_shift_insert_after() {
    let mut document = document("1 1 1");
    let mut view = view_of(&mut document);
    let mut hits = DocumentHitIterator::text_matcher("1", -1);
    for i in 0..3 {
        let range = hits.next(&view).unwrap();
        assert_eq!(i * 3 + 1, range.start);
        view.insert_text(&mut document.mutable(), range.end as usize, "1")
            .unwrap();
        hits.shift(i64::from(range.end), 1);
    }
    assert_eq!(None, hits.next(&view));
}

#[test]
fn basic_element_matcher() {
    let xml = "text <gadget></gadget> <something/> <gadget/> hello";
    let mut document = document(xml);
    let view = view_of(&mut document);
    let mut hits = DocumentHitIterator::element_matcher(ElementType::Gadget, BTreeMap::new(), -1);
    // Elements count as one character, unknown ones too.
    assert_eq!(vec![6, 10], starts(&mut hits, &view));
}

#[test]
fn element_matcher_with_restrictions() {
    let mut document = document(
        "text <image attachment=\"nR_0YFT75\" style=\"full\">\
         <caption>good times</caption></image> hello",
    );
    let view = view_of(&mut document);
    let count = |restrictions: &[(&str, &str)]| {
        let restrictions = restrictions
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let mut hits =
            DocumentHitIterator::element_matcher(ElementType::Attachment, restrictions, -1);
        starts(&mut hits, &view).len()
    };
    assert_eq!(1, count(&[]));
    assert_eq!(1, count(&[("attachmentId", "nR_0YFT75")]));
    assert_eq!(0, count(&[("attachmentId", "nR_0YFT76")]));
    assert_eq!(1, count(&[("caption", "good times")]));
}

// ElementSerializerTest: every kind of element survives the trip into a
// document and back.

fn round_trip(element: &Element) -> Element {
    let xml = api_element_to_xml(element);
    let mut document = StandaloneDocument::new(BootstrapDocument::new());
    document.mutable().append_xml(&xml).unwrap();
    let mutable = document.mutable();
    let view: &DocView = mutable.view();
    let node = view.first_child(view.root()).expect("an element");
    xml_to_api_element(view, node, &AttachmentContext::none()).expect("a known element")
}

#[test]
fn form_elements_round_trip() {
    let input = Element::form(ElementType::Input, "name", "", "value");
    let read = round_trip(&input);
    assert_eq!(Some("name"), read.property("name"));
    assert_eq!(Some("value"), read.property("value"));
    let label = round_trip(&Element::form(
        ElementType::Label,
        "for-input",
        "",
        "A label",
    ));
    assert_eq!(Some("for-input"), label.property("name"));
    assert_eq!(Some("A label"), label.property("value"));
    assert_eq!(Some("A label"), label.property("defaultValue"));
    let password = round_trip(&Element::form(ElementType::Password, "pw", "", "secret"));
    assert_eq!(Some("secret"), password.property("value"));
    let textarea = round_trip(&Element::form(ElementType::Textarea, "ta", "", "one\ntwo"));
    assert_eq!(Some("one\ntwo"), textarea.property("value"));
    assert_eq!(Some("ta"), textarea.property("name"));
    let empty_textarea = round_trip(&Element::form(ElementType::Textarea, "ta", "", " "));
    assert_eq!(Some(""), empty_textarea.property("value"));
    let button = round_trip(&Element::form(ElementType::Button, "btn", "", "Click"));
    assert_eq!(Some("Click"), button.property("defaultValue"));
    assert_eq!(Some("Click"), button.property("value"));
    let group = round_trip(&Element::form(ElementType::RadioButtonGroup, "grp", "", ""));
    assert_eq!(Some("grp"), group.property("name"));
    let radio = round_trip(&Element::form(ElementType::RadioButton, "r1", "", "grp"));
    assert_eq!(Some("r1"), radio.property("name"));
    assert_eq!(Some("grp"), radio.property("value"));
    let check = round_trip(&Element::form(ElementType::Check, "chk", "d", "true"));
    assert_eq!(Some("true"), check.property("value"));
    assert_eq!(Some("d"), check.property("defaultValue"));
}

#[test]
fn clicked_button_reads_as_clicked() {
    let mut document =
        document("<button name=\"b\"><caption>Go</caption><events><click/></events></button>");
    let view = view_of(&mut document);
    let button = &view.elements()[1].element;
    assert_eq!(ElementType::Button, button.element_type);
    assert_eq!(Some("clicked"), button.property("value"));
    assert_eq!(Some("Go"), button.property("defaultValue"));
}

#[test]
fn other_elements_round_trip() {
    let mut gadget = Element::gadget("http://example.com/g.xml");
    gadget.set_property("author", "alice@example.com");
    gadget.set_property("title", "Title");
    gadget.set_property("thumbnail", "thumb");
    gadget.set_property("category", "cat");
    gadget.set_property("pref", "prefs");
    gadget.set_property("state1", "value1");
    let read = round_trip(&gadget);
    for (key, value) in &gadget.properties {
        assert_eq!(value.as_deref(), read.property(key), "{key}");
    }
    // The original also files the author as a state.
    assert_eq!(ElementType::Gadget, read.element_type);

    let image = Element::image("http://example.com/i.png", 10, 20, "caption");
    assert_eq!(image, round_trip(&image));

    let attachment = Element::with_properties(
        ElementType::Attachment,
        [("attachmentId", "att"), ("caption", "caption")],
    );
    assert_eq!(attachment, round_trip(&attachment));
    assert_eq!("image", tag_name_for_element_type(ElementType::Attachment));
    // An attachment without an id is nothing in the document.
    assert!(api_element_to_xml(&Element::new(ElementType::Attachment)).is_empty());

    let mut line = Element::line();
    line.set_property(Element::LINE_TYPE, "h1");
    line.set_property(Element::INDENT, "2");
    line.set_property(Element::ALIGNMENT, "c");
    line.set_property(Element::DIRECTION, "r");
    assert_eq!(line, round_trip(&line));
    assert_eq!(Element::line(), round_trip(&Element::line()));

    let inline = Element::with_properties(ElementType::InlineBlip, [("id", "b+1")]);
    assert_eq!(inline, round_trip(&inline));
    assert_eq!("reply", tag_name_for_element_type(ElementType::InlineBlip));

    let installer = Element::with_properties(ElementType::Installer, [("manifest", "http://m/")]);
    assert_eq!(installer, round_trip(&installer));
}

#[test]
fn elements_by_position() {
    let mut document = document("ab<img src=\"u\"/>c<unknown/>d");
    let mutable = document.mutable();
    let elements = serialize(mutable.view(), &AttachmentContext::none());
    let positions: Vec<(i32, ElementType)> = elements
        .iter()
        .map(|(position, element)| (*position, element.element_type))
        .collect();
    assert_eq!(
        vec![(0, ElementType::Line), (3, ElementType::Image)],
        positions
    );
}
