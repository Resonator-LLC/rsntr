//! Scenarios of the document layer (`docScenarios` in the generator).

use super::{Wave, alice};
use crate::doc::line_containers::{append_line, append_to_last_line};
use crate::doc::{Nindo, XmlFragment};
use crate::docop::Attributes;

const W: &str = "conv+root";
const D: &str = "b+doc";

#[test]
fn doc_basic() {
    let mut wave = Wave::new(alice());
    let body = XmlFragment::empty().wrap("line").wrap("body");
    wave.step("insert body", |w| {
        w.edit(W, D, |doc| doc.insert_xml(doc.size(), &body).map(drop))
    });
    wave.step("insert text", |w| {
        w.edit(W, D, |doc| doc.insert_text(3, "Hello world"))
    });
    wave.step("bold", |w| {
        w.edit(W, D, |doc| {
            doc.set_annotation(3, 8, "style/fontWeight", Some("bold"))
        })
    });
    wave.step("link", |w| {
        w.edit(W, D, |doc| {
            doc.set_annotation(5, 12, "link/manual", Some("http://x"))
        })
    });
    wave.step("insert in annotated", |w| {
        w.edit(W, D, |doc| doc.insert_text(6, "XY"))
    });
    wave.step("insert at annotation end", |w| {
        w.edit(W, D, |doc| doc.insert_text(10, "Z"))
    });
    wave.step("create reply anchor", |w| {
        w.edit(W, D, |doc| {
            doc.create_element(9, "reply", &[("id", "t1")]).map(drop)
        })
    });
    let element_at = |doc: &crate::doc::MutableDocument<'_>, location| {
        doc.view().element_starting_at(location).unwrap()
    };
    wave.step("set attribute", |w| {
        w.edit(W, D, |doc| {
            doc.set_element_attribute(element_at(doc, 9), "id", Some("t2"))
        })
    });
    wave.step("set same attribute", |w| {
        w.edit(W, D, |doc| {
            doc.set_element_attribute(element_at(doc, 9), "id", Some("t2"))
        })
    });
    wave.step("update attributes", |w| {
        w.edit(W, D, |doc| {
            doc.update_element_attributes(
                element_at(doc, 9),
                &[("a", Some("1")), ("id", None), ("zz", Some("z"))],
            )
        })
    });
    wave.step("replace attributes", |w| {
        w.edit(W, D, |doc| {
            doc.set_element_attributes(
                element_at(doc, 9),
                Attributes::from_pairs([("z", "9"), ("b", "2")]).unwrap(),
            )
        })
    });
    wave.step("unbold part", |w| {
        w.edit(W, D, |doc| {
            doc.set_annotation(4, 7, "style/fontWeight", None)
        })
    });
    wave.step("rebold overlapping", |w| {
        w.edit(W, D, |doc| {
            doc.set_annotation(2, 13, "style/fontWeight", Some("bold"))
        })
    });
    wave.step("set same annotation", |w| {
        w.edit(W, D, |doc| {
            doc.set_annotation(3, 6, "style/fontWeight", Some("bold"))
        })
    });
    wave.step("delete range over annotations", |w| {
        w.edit(W, D, |doc| doc.delete_range(4, 13))
    });
    wave.step("explicit title", |w| {
        w.edit(W, D, |doc| {
            doc.reset_annotation(0, 1, "conv/title", Some("Title"))
        })
    });
    wave.step("implicit title", |w| {
        w.edit(W, D, |doc| {
            doc.reset_annotation(1, 6, "conv/title", Some(""))
        })
    });
    wave.step("append line", |w| {
        w.edit(W, D, |doc| {
            let content = XmlFragment::text("second < line & more");
            append_line(doc, Some(&content), Some(&Attributes::new())).map(drop)
        })
    });
    wave.step("append to last line", |w| {
        w.edit(W, D, |doc| {
            append_to_last_line(doc, &XmlFragment::text(" tail"))
        })
    });
    wave.step("insert empty text", |w| {
        w.edit(W, D, |doc| doc.insert_text(4, ""))
    });
    wave.step("delete empty range", |w| {
        w.edit(W, D, |doc| doc.delete_range(4, 4))
    });
    wave.step("delete line element", |w| {
        w.edit(W, D, |doc| doc.delete_node(element_at(doc, 1)))
    });
    wave.step("delete all text", |w| {
        w.edit(W, D, |doc| doc.delete_range(1, doc.size() - 1))
    });
    wave.step("empty document", |w| {
        w.edit(W, D, |doc| doc.empty_element(doc.view().root()))
    });
    wave.step("append line to empty", |w| {
        w.edit(W, D, |doc| {
            let content = XmlFragment::text("fresh");
            append_line(doc, Some(&content), Some(&Attributes::new())).map(drop)
        })
    });
    wave.step("append to last line of empty", |w| {
        w.edit(W, D, |doc| {
            doc.empty_element(doc.view().root())?;
            append_to_last_line(doc, &XmlFragment::text("x"))
        })
    });
    wave.step("bad location", |w| {
        w.edit(W, D, |doc| doc.insert_text(1000, "x"))
    });
    wave.check("doc-basic");
}

#[test]
fn doc_delete_across_elements() {
    let mut wave = Wave::new(alice());
    wave.step("build", |w| {
        w.edit(W, D, |doc| {
            let xml = XmlFragment::parse(
                "<a>one<b>two<c>three</c>four</b>five</a><d>six<e>seven</e>eight</d><f>nine</f>",
            )?;
            doc.insert_xml(doc.size(), &xml).map(drop)
        })
    });
    wave.step("annotate", |w| {
        w.edit(W, D, |doc| doc.set_annotation(6, 30, "k", Some("v")))
    });
    wave.step("annotate 2", |w| {
        w.edit(W, D, |doc| doc.set_annotation(10, 20, "j", Some("w")))
    });
    wave.step("delete c to e", |w| {
        w.edit(W, D, |doc| doc.delete_range(11, 36))
    });
    wave.step("delete a to f", |w| {
        w.edit(W, D, |doc| doc.delete_range(2, doc.size() - 3))
    });
    wave.step("delete into child", |w| {
        w.edit(W, D, |doc| doc.delete_range(1, 5))
    });
    wave.check("doc-delete-across-elements");
}

#[test]
fn doc_nindo() {
    let mut wave = Wave::new(alice());
    wave.step("initial content with annotations", |w| {
        w.edit(W, D, |doc| {
            let content = crate::conversation::title::empty_document_with_title();
            doc.consume(&Nindo::from_doc_op(content.as_op(), false))
        })
    });
    wave.step("text", |w| {
        w.edit(W, D, |doc| doc.insert_text(3, "abcdefghij"))
    });
    wave.step("mixed", |w| {
        w.edit(W, D, |doc| {
            let mut b = Nindo::builder();
            b.skip(4);
            b.start_annotation("x", Some("1"));
            b.skip(2);
            b.characters("NEW");
            b.skip(1);
            b.end_annotation("x");
            b.delete_characters(2);
            b.start_annotation("y", Some("2"));
            b.skip(2);
            b.end_annotation("y");
            doc.consume(&b.build())
        })
    });
    wave.step("delete over three annotations", |w| {
        w.edit(W, D, |doc| doc.delete_range(2, 12))
    });
    wave.step("remove annotation everywhere", |w| {
        w.edit(W, D, |doc| doc.set_annotation(0, doc.size(), "x", None))
    });
    wave.step("delete after key removed", |w| {
        w.edit(W, D, |doc| doc.delete_range(3, 4))
    });
    wave.check("doc-nindo");
}

#[test]
fn doc_unicode() {
    let mut wave = Wave::new(alice());
    let body = XmlFragment::empty().wrap("line").wrap("body");
    wave.step("build", |w| {
        w.edit(W, D, |doc| doc.insert_xml(doc.size(), &body).map(drop))
    });
    wave.step("text", |w| {
        w.edit(W, D, |doc| {
            doc.insert_text(3, "h\u{e9}llo \u{4e16}\u{754c} \"q\" 'a' <&>")
        })
    });
    wave.step("annotate", |w| {
        w.edit(W, D, |doc| {
            doc.set_annotation(4, 9, "k\u{e9}y", Some("v\u{e4}l\"ue"))
        })
    });
    wave.step("delete", |w| w.edit(W, D, |doc| doc.delete_range(5, 11)));
    wave.check("doc-unicode");
}
