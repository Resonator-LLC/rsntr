//! Tests of the document layer. The conversion of edits to operations is
//! checked against the original in `model_golden_tests`; these cover the
//! tree view and the edges of the editing API.

use super::*;
use crate::docop::{Attributes, BootstrapDocument, to_concise_string};

fn doc(xml: &str) -> StandaloneDocument {
    let mut doc = StandaloneDocument::default();
    doc.mutable()
        .insert_xml(0, &XmlFragment::parse(xml).unwrap())
        .unwrap();
    doc.take_ops();
    doc
}

fn xml(doc: &StandaloneDocument) -> String {
    doc.document().to_xml_string()
}

#[test]
fn view_structure_and_locations() {
    let d = doc("<a x=\"1\">one<b>two</b></a>t<c/>");
    let view = DocView::new(d.document());
    let root = view.root();
    assert_eq!(13, view.size());
    assert_eq!(Some(ROOT_TAG), view.tag_name(root));
    assert_eq!((0, 13), (view.content_start(root), view.content_end(root)));
    let [a, t, c] = view.children(root) else {
        panic!("three children expected");
    };
    assert_eq!(Some("a"), view.tag_name(*a));
    assert_eq!(Some("1"), view.attribute(*a, "x"));
    assert_eq!(None, view.attribute(*a, "y"));
    assert_eq!(Some("t"), view.text(*t));
    assert!(view.is_text(*t) && !view.is_element(*t));
    assert_eq!(None, view.tag_name(*t));
    assert_eq!(
        (0, 1, 9, 10),
        (
            view.location(*a),
            view.content_start(*a),
            view.content_end(*a),
            view.location_after(*a)
        )
    );
    assert_eq!((10, 11), (view.location(*t), view.location_after(*t)));
    assert_eq!((11, 2), (view.location(*c), view.item_size(*c)));
    let b = view.element_with_tag_name("b", root).unwrap();
    assert_eq!(Some(*a), view.parent(b));
    assert_eq!(4, view.location(b));
    assert_eq!("two", view.text_content(b));
    assert_eq!("onetwo", view.text_content(*a));
    assert_eq!(Some(*t), view.next_sibling(*a));
    assert_eq!(Some(*c), view.next_sibling_element(*a));
    assert_eq!(Some(*a), view.previous_sibling(*t));
    assert_eq!(None, view.previous_sibling(*a));
    assert_eq!(None, view.next_sibling(*c));
    assert_eq!(Some(*a), view.first_child_element(root));
    assert_eq!(Some(*c), view.last_child(root));
    assert_eq!(*a, view.enclosing_element(view.first_child(*a).unwrap()));
    assert_eq!(vec![*a, b, *c], view.descendant_elements(root));
    assert_eq!(6, view.descendants(root).len());
    assert_eq!("netwot", view.text_between(2, 12));
    assert_eq!("", view.text_between(5, 5));
    assert_eq!(Some(b), view.element_starting_at(4));
    assert_eq!(None, view.element_starting_at(5));
    assert_eq!(None, view.element_starting_at(10));
    assert_eq!(Some(*t), view.node_starting_at(10));
    assert_eq!(Some(b), view.element_at(6));
    assert_eq!(Some(root), view.element_at(10));
    assert_eq!(None, view.element_at(14));
}

#[test]
fn view_locate_round_trips() {
    let d = doc("<a>one<b>two</b></a>t<c/>");
    let view = DocView::new(d.document());
    for location in 0..=view.size() {
        let point = view.locate(location).unwrap();
        assert_eq!(Some(location), view.point_location(point), "{location}");
    }
    assert_eq!(None, view.locate(view.size() + 1));
    let a = view.first_child(view.root()).unwrap();
    let text = view.first_child(a).unwrap();
    assert_eq!(
        Some(Point::InText {
            node: text,
            offset: 1
        }),
        view.locate(2)
    );
    assert_eq!(
        Some(Point::InElement {
            container: a,
            node_after: Some(text)
        }),
        view.locate(1)
    );
    assert_eq!(
        Some(Point::InElement {
            container: a,
            node_after: None
        }),
        view.locate(9)
    );
    // Invalid points.
    assert_eq!(
        None,
        view.point_location(Point::InText { node: a, offset: 0 })
    );
    assert_eq!(
        None,
        view.point_location(Point::InText {
            node: text,
            offset: 4
        })
    );
    assert_eq!(
        None,
        view.point_location(Point::InElement {
            container: view.root(),
            node_after: Some(text)
        })
    );
}

#[test]
fn view_merges_text_split_by_annotations() {
    let mut d = doc("<p>abcdef</p>");
    d.mutable().set_annotation(3, 5, "k", Some("v")).unwrap();
    let view = DocView::new(d.document());
    let p = view.first_child(view.root()).unwrap();
    assert_eq!(1, view.children(p).len());
    assert_eq!("abcdef", view.text_content(p));
}

#[test]
fn empty_document_view() {
    let view = DocView::new(&BootstrapDocument::new());
    assert_eq!(0, view.size());
    assert!(view.children(view.root()).is_empty());
    assert_eq!(
        Some(Point::InElement {
            container: view.root(),
            node_after: None
        }),
        view.locate(0)
    );
    assert_eq!("", view.text_between(0, 10));
}

#[test]
fn xml_fragment() {
    let fragment = XmlFragment::text("a<b")
        .wrap("x")
        .append(&XmlFragment::text("c"))
        .append_text("d");
    assert_eq!(7, fragment.item_count());
    assert_eq!(4, fragment.items().len());
    assert!(XmlFragment::empty().is_empty());
    assert_eq!(0, XmlFragment::text("").item_count());
    let parsed =
        XmlFragment::parse("<a x='1' y=\"&lt;&amp;&#65;&#x42;\" >t&gt;&quot;&apos;<b/></a >tail")
            .unwrap();
    let mut d = StandaloneDocument::default();
    d.mutable().append_xml(&parsed).unwrap();
    assert_eq!("<a x=\"1\" y=\"<&AB\">t&gt;\"'<b/></a>tail", xml(&d));
    assert_eq!(
        XmlFragment::from_initialization(&d.document().as_operation()),
        parsed
    );
    for bad in [
        "<a>",
        "</a>",
        "<a></b>",
        "<a x=1/>",
        "<a x/>",
        "<a x=\"1/>",
        "<!-- c -->",
        "<?pi?>",
        "<a",
        "&bogus;",
        "<a x=\"1\" x=\"2\"/>",
        "<1a/>",
    ] {
        assert!(XmlFragment::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn edits_check_their_arguments() {
    let mut d = doc("<a>one</a>");
    let mut m = d.mutable();
    let root = m.view().root();
    let a = m.view().first_child(root).unwrap();
    let text = m.view().first_child(a).unwrap();
    assert!(matches!(
        m.insert_text(6, "x"),
        Err(DocError::InvalidLocation { .. })
    ));
    assert!(m.delete_range(3, 2).is_err());
    assert!(m.delete_range(0, 6).is_err());
    assert_eq!(Err(DocError::InvalidNode), m.delete_node(root));
    assert_eq!(Err(DocError::InvalidNode), m.delete_node(text));
    assert_eq!(
        Err(DocError::InvalidNode),
        m.set_element_attribute(root, "x", Some("1"))
    );
    assert_eq!(Err(DocError::InvalidNode), m.empty_element(text));
    assert_eq!(
        Err(DocError::InvalidNode),
        m.create_child_element(text, "b", &[]).map(drop)
    );
    assert!(m.set_annotation(0, 2, "bad?key", Some("v")).is_err());
    assert!(m.set_annotation(0, 2, "@local", Some("v")).is_err());
    assert!(m.set_annotation(2, 9, "k", Some("v")).is_err());
    assert_eq!("<a>one</a>", xml(&d));
    assert!(d.ops().is_empty());
    // An edit the document rejects leaves it unchanged.
    assert!(
        d.mutable()
            .consume(&{
                let mut b = Nindo::builder();
                b.skip(1);
                b.element_end();
                b.build()
            })
            .is_err()
    );
    assert!(
        d.mutable()
            .consume(&{
                let mut b = Nindo::builder();
                b.delete_characters(1);
                b.build()
            })
            .is_err()
    );
    assert!(
        d.mutable()
            .consume(&{
                let mut b = Nindo::builder();
                b.start_annotation("k", Some("v"));
                b.skip(2);
                b.build()
            })
            .is_err()
    );
    assert!(
        d.mutable()
            .consume(&{
                let mut b = Nindo::builder();
                b.end_annotation("k");
                b.build()
            })
            .is_err()
    );
    assert_eq!("<a>one</a>", xml(&d));
}

#[test]
fn edits_return_new_nodes() {
    let mut d = doc("<a>one</a>");
    let mut m = d.mutable();
    let b = m.create_element(2, "b", &[("k", "v"), ("j", "w")]).unwrap();
    assert_eq!(Some("b"), m.view().tag_name(b));
    assert_eq!(2, m.view().location(b));
    let a = m.view().first_child(m.view().root()).unwrap();
    let c = m.create_child_element(a, "c", &[]).unwrap();
    assert_eq!(6, m.view().location(c));
    assert_eq!(None, m.insert_xml(1, &XmlFragment::text("t")).unwrap());
    let inserted = m
        .insert_xml(0, &XmlFragment::text("u").wrap("d"))
        .unwrap()
        .unwrap();
    assert_eq!(Some("d"), m.view().tag_name(inserted));
    assert_eq!("<d>u</d><a>to<b j=\"w\" k=\"v\"/>ne<c/></a>", xml(&d));
    assert_eq!(4, d.ops().len());
    assert_eq!(
        "__2; << b { j=\"w\", k=\"v\" }; >>; __3; ",
        to_concise_string(&d.ops()[0])
    );
}

#[test]
fn guarded_reset_annotation() {
    let mut d = doc("<a>one</a>");
    assert!(
        d.mutable()
            .guarded_reset_annotation(1, 3, "k", Some("v"))
            .unwrap()
    );
    assert!(
        !d.mutable()
            .guarded_reset_annotation(1, 3, "k", Some("v"))
            .unwrap()
    );
    assert!(
        d.mutable()
            .guarded_reset_annotation(2, 3, "k", Some("v"))
            .unwrap()
    );
    assert!(
        d.mutable()
            .guarded_reset_annotation(2, 3, "k", Some("w"))
            .unwrap()
    );
    assert_eq!(3, d.ops().len());
    assert_eq!("<a>o<?a \"k\"=\"w\"?>n<?a \"k\"?>e</a>", xml(&d));
    // Resetting on an empty document does nothing.
    let mut empty = StandaloneDocument::default();
    empty
        .mutable()
        .reset_annotation(0, 0, "k", Some("v"))
        .unwrap();
    assert!(empty.ops().is_empty());
}

#[test]
fn annotation_reading() {
    let mut d = doc("<a>abcdefgh</a>");
    let mut m = d.mutable();
    m.set_annotation(2, 6, "k", Some("v")).unwrap();
    m.set_annotation(4, 8, "j", Some("w")).unwrap();
    assert_eq!(Some("v".to_owned()), m.annotation(2, "k"));
    assert_eq!(None, m.annotation(6, "k"));
    assert_eq!(None, m.annotation(100, "k"));
    assert_eq!(Some(2), m.find_annotation_change(0, 10, "k", None));
    assert_eq!(Some(6), m.find_annotation_change(2, 10, "k", Some("v")));
    assert_eq!(None, m.find_annotation_change(2, 6, "k", Some("v")));
    assert_eq!(
        vec!["j", "k"],
        m.annotation_keys().into_iter().collect::<Vec<_>>()
    );
    let ranges: Vec<(String, Option<String>, usize, usize)> = m
        .ranged_annotations(3, 7, None)
        .into_iter()
        .map(|r| (r.key, r.value, r.start, r.end))
        .collect();
    let s = |v: &str| Some(v.to_owned());
    assert_eq!(
        vec![
            ("j".to_owned(), None, 0, 4),
            ("k".to_owned(), s("v"), 2, 6),
            ("j".to_owned(), s("w"), 4, 8),
            ("k".to_owned(), None, 6, 10),
        ],
        ranges
    );
    assert_eq!(1, m.ranged_annotations(2, 3, Some(&["k"])).len());
    assert!(m.ranged_annotations(5, 5, None).is_empty());
}

#[test]
fn nindo_from_doc_op() {
    let mut d = doc("<a>one</a>");
    d.mutable().set_annotation(1, 3, "k", Some("v")).unwrap();
    let op = d.ops()[0].clone();
    let nindo = Nindo::from_doc_op(&op, true);
    assert_eq!(
        &[
            NindoComponent::Skip(1),
            NindoComponent::StartAnnotation {
                key: "k".to_owned(),
                value: Some("v".to_owned())
            },
            NindoComponent::Skip(2),
            NindoComponent::EndAnnotation("k".to_owned()),
        ],
        nindo.components()
    );
    assert_eq!(5, Nindo::from_doc_op(&op, false).components().len());
    assert!(
        Nindo::set_annotation(3, 3, "k", None)
            .components()
            .is_empty()
    );
    // Replaying the non-invertible form on the changed document shows the
    // old values as they now are.
    d.mutable().consume(&nindo).unwrap();
    assert_eq!(
        "__1; || { \"k\": \"v\" -> \"v\" }; __2; || { \"k\" }; __2; ",
        to_concise_string(&d.ops()[1])
    );
}

#[test]
fn line_container_helpers() {
    use line_containers::*;
    let mut d = StandaloneDocument::default();
    let mut m = d.mutable();
    let attributes = Attributes::from_pairs([("t", "h1")]).unwrap();
    let line = append_line(&mut m, Some(&XmlFragment::text("first")), Some(&attributes)).unwrap();
    assert_eq!(Some("h1"), m.view().attribute(line, "t"));
    let line = append_line(&mut m, None, None).unwrap();
    assert_eq!(8, m.view().location(line));
    append_to_last_line(&mut m, &XmlFragment::text("second")).unwrap();
    append_line(&mut m, Some(&XmlFragment::empty()), Some(&attributes)).unwrap();
    assert_eq!(
        "<body><line t=\"h1\"/>first<line/>second<line t=\"h1\"/></body>",
        xml(&d)
    );
    assert!(is_line_container_tagname("body") && !is_line_container_tagname("line"));
    // Creating the container: content, then a replace of the attributes.
    assert_eq!(
        vec![
            "<< body {}; << line {}; >>; >>; ",
            "__3; ++\"first\"; __1; ",
            "__1; r@ {} { t=\"h1\" }; __7; ",
            "__8; << line {}; >>; __1; ",
            "__10; ++\"second\"; __1; ",
            "__16; << line { t=\"h1\" }; >>; __1; ",
        ],
        d.ops().iter().map(to_concise_string).collect::<Vec<_>>()
    );
}
