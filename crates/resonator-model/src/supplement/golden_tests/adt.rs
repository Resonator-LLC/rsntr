//! Scenarios of the document-based data types (`adtScenarios` in the
//! generator).

use super::{ROOT, StepResult, Wave, list, nullable};
use crate::adt::{
    AdtError, DocumentBasedBasicMap, DocumentBasedBasicSet, DocumentBasedBasicValue,
    DocumentBasedBoolean, DocumentBasedElementList, DocumentBasedMonotonicMap,
    DocumentBasedMonotonicValue, DocumentBasedSingleton, DocumentBasedStructuredValue,
    FirstTopLevel, Initializer, Root, build_attributes,
};
use crate::doc::{DocView, MutableDocument, NodeId};

const D: &str = "d";

/// A step that edits the document `d` and may answer queries.
fn step(
    wave: &mut Wave,
    label: &str,
    edit: impl FnOnce(&mut MutableDocument<'_>, &mut Vec<String>) -> Result<(), AdtError>,
) {
    wave.step(label, |w| {
        let mut answers = Vec::new();
        let result = {
            let mut sink = w.wavelet(ROOT).document_sink(D);
            let mut doc = MutableDocument::new(&mut sink);
            edit(&mut doc, &mut answers)
        };
        for answer in answers {
            w.q(answer);
        }
        Ok(result?)
    });
}

fn append(wave: &mut Wave, label: &str, xml: &str) {
    wave.step(label, |w| -> StepResult { w.append(ROOT, D, xml) });
}

#[test]
fn adt_basic_map() {
    const MAP: DocumentBasedBasicMap<'static, String, String> =
        DocumentBasedBasicMap::new("e", "k", "v");
    let s = |text: &str| text.to_owned();
    let read = |doc: &MutableDocument<'_>, keys: &[&str]| {
        let view = doc.view();
        let mut answer: Vec<String> = keys
            .iter()
            .map(|key| nullable(MAP.get(view, view.root(), &s(key))))
            .collect();
        answer.push(list(MAP.key_set(view, view.root())));
        answer.join(" ")
    };
    let mut wave = Wave::new();
    let put = |wave: &mut Wave, label: &str, key: &str, value: &str| {
        step(wave, label, |doc, a| {
            a.push(MAP.put(doc, &Root, &s(key), &s(value))?.to_string());
            Ok(())
        });
    };
    put(&mut wave, "put a", "a", "1");
    put(&mut wave, "put b", "b", "2");
    put(&mut wave, "put a same", "a", "1");
    put(&mut wave, "put a other", "a", "3");
    step(&mut wave, "read", |doc, a| {
        a.push(read(doc, &["a", "b", "c"]));
        Ok(())
    });
    step(&mut wave, "remove b", |doc, _| {
        MAP.remove(doc, &Root, &s("b"))
    });
    step(&mut wave, "remove missing", |doc, _| {
        MAP.remove(doc, &Root, &s("zz"))
    });
    append(
        &mut wave,
        "foreign content",
        "<e k=\"a\" v=\"9\"></e><x></x><e k=\"c\" v=\"1\"></e><e v=\"1\"></e>",
    );
    step(&mut wave, "read again", |doc, a| {
        a.push(read(doc, &["a", "c"]));
        Ok(())
    });
    put(&mut wave, "~put c cleans up", "c", "2");
    step(&mut wave, "read last", |doc, a| {
        a.push(read(doc, &["a", "c"]));
        Ok(())
    });
    wave.check("adt-basic-map");
}

#[test]
fn adt_monotonic_map() {
    const MAP: DocumentBasedMonotonicMap<'static, String, i32> =
        DocumentBasedMonotonicMap::new("e", "k", "v");
    let s = |text: &str| text.to_owned();
    let read = |doc: &MutableDocument<'_>, keys: &[&str]| {
        let view = doc.view();
        let mut answer: Vec<String> = keys
            .iter()
            .map(|key| nullable(MAP.get(view, view.root(), &s(key))))
            .collect();
        answer.push(list(MAP.key_set(view, view.root())));
        answer.join(" ")
    };
    let mut wave = Wave::new();
    let put = |wave: &mut Wave, label: &str, key: &str, value: i32| {
        step(wave, label, |doc, a| {
            a.push(MAP.put(doc, &Root, &s(key), &value)?.to_string());
            Ok(())
        });
    };
    put(&mut wave, "put a 5", "a", 5);
    put(&mut wave, "put a 3", "a", 3);
    put(&mut wave, "put a 5 again", "a", 5);
    put(&mut wave, "put b 1", "b", 1);
    put(&mut wave, "put a 7", "a", 7);
    step(&mut wave, "read", |doc, a| {
        a.push(read(doc, &["a", "b", "c"]));
        Ok(())
    });
    step(&mut wave, "remove a", |doc, _| {
        MAP.remove(doc, &Root, &s("a"))
    });
    put(&mut wave, "put a 2", "a", 2);
    append(
        &mut wave,
        "foreign content",
        "<e k=\"b\" v=\"4\"></e><e k=\"b\" v=\"4\"></e>",
    );
    step(&mut wave, "read again", |doc, a| {
        a.push(read(doc, &["a", "b"]));
        Ok(())
    });
    put(&mut wave, "~put b 9 cleans up", "b", 9);
    wave.check("adt-monotonic-map");
}

#[test]
fn adt_set() {
    const SET: DocumentBasedBasicSet<'static, i32> = DocumentBasedBasicSet::new("f", "i");
    let mut wave = Wave::new();
    step(&mut wave, "add 5", |doc, _| {
        SET.add(doc, &Root, &5).map(drop)
    });
    step(&mut wave, "add 7", |doc, _| {
        SET.add(doc, &Root, &7).map(drop)
    });
    step(&mut wave, "add 5 again", |doc, _| {
        SET.add(doc, &Root, &5).map(drop)
    });
    step(&mut wave, "remove 5", |doc, _| {
        SET.remove(doc, &Root, &5).map(drop)
    });
    step(&mut wave, "remove missing", |doc, _| {
        SET.remove(doc, &Root, &6).map(drop)
    });
    append(
        &mut wave,
        "foreign content",
        "<f i=\"7\"></f><x></x><f i=\"2\"></f>",
    );
    step(&mut wave, "read", |doc, a| {
        let view = doc.view();
        let root = view.root();
        a.push(format!(
            "{} {} {} {}",
            SET.contains(view, root, &7),
            SET.contains(view, root, &2),
            SET.contains(view, root, &5),
            list(SET.values(view, root))
        ));
        Ok(())
    });
    step(&mut wave, "add 9 cleans up", |doc, _| {
        SET.add(doc, &Root, &9).map(drop)
    });
    step(&mut wave, "clear", |doc, _| SET.clear(doc, &Root).map(drop));
    step(&mut wave, "clear empty", |doc, _| {
        SET.clear(doc, &Root).map(drop)
    });
    wave.check("adt-set");
}

#[test]
fn adt_values() {
    const VALUE: DocumentBasedMonotonicValue<'static, i32> =
        DocumentBasedMonotonicValue::new("m", "v");
    const FLAG: DocumentBasedBoolean<'static> = DocumentBasedBoolean::new("b", "on");
    let read = |doc: &MutableDocument<'_>, a: &mut Vec<String>| {
        let view = doc.view();
        a.push(format!(
            "{} {}",
            nullable(VALUE.get(view, view.root())),
            nullable(FLAG.get(view, view.root()))
        ));
        Ok(())
    };
    let mut wave = Wave::new();
    let value = |wave: &mut Wave, label: &str, value: i32| {
        step(wave, label, |doc, _| {
            VALUE.set(doc, &Root, &value).map(drop)
        });
    };
    let flag = |wave: &mut Wave, label: &str, value: Option<bool>| {
        step(wave, label, |doc, _| FLAG.set(doc, &Root, value).map(drop));
    };
    value(&mut wave, "value 3", 3);
    value(&mut wave, "value 2", 2);
    value(&mut wave, "value 5", 5);
    flag(&mut wave, "flag true", Some(true));
    flag(&mut wave, "flag true again", Some(true));
    flag(&mut wave, "flag false", Some(false));
    step(&mut wave, "read", |doc, a| read(doc, a));
    flag(&mut wave, "flag null", None);
    flag(&mut wave, "flag null again", None);
    append(
        &mut wave,
        "foreign content",
        "<b on=\"true\"></b><b on=\"false\"></b><b></b><m v=\"5\"></m>",
    );
    step(&mut wave, "read again", |doc, a| read(doc, a));
    flag(&mut wave, "flag false cleans up", Some(false));
    value(&mut wave, "~value 6 cleans up", 6);
    wave.check("adt-values");
}

/// The value the generator's factory adapts an element to.
fn bar(view: &DocView, element: NodeId) -> String {
    nullable(view.attribute(element, "bar"))
}

/// The attributes the generator's factory initialises an element with.
fn bar_attributes(value: Option<&str>) -> Result<crate::adt::InitialAttributes, AdtError> {
    let value = value.map(str::to_owned);
    let initialiser =
        DocumentBasedBasicValue::<String>::new("bar").create_initialiser(value.as_ref());
    build_attributes(&initialiser as &dyn Initializer)
}

#[test]
fn adt_list() {
    const LIST: DocumentBasedElementList<'static> = DocumentBasedElementList::new("foo");
    let container = FirstTopLevel("c");
    let mut wave = Wave::new();
    append(&mut wave, "container", "<x></x><c><y></y></c>");
    step(&mut wave, "add x", |doc, _| {
        LIST.add(doc, &container, &bar_attributes(Some("x"))?)
            .map(drop)
    });
    step(&mut wave, "add y", |doc, _| {
        LIST.add(doc, &container, &bar_attributes(Some("y"))?)
            .map(drop)
    });
    let add_at = |wave: &mut Wave, label: &str, index: usize, value: Option<&str>| {
        step(wave, label, |doc, _| {
            LIST.add_at(doc, &container, index, &bar_attributes(value)?)
                .map(drop)
        });
    };
    add_at(&mut wave, "add z at 0", 0, Some("z"));
    add_at(&mut wave, "add null at 1", 1, None);
    add_at(&mut wave, "add w at end", 4, Some("w"));
    add_at(&mut wave, "add beyond end", 6, Some("v"));
    let index_of = |doc: &MutableDocument<'_>, value: &str| {
        let view = doc.view();
        let node = FirstTopLevel("c").locate_in(view);
        LIST.values(view, node, bar).iter().position(|v| v == value)
    };
    step(&mut wave, "read", |doc, a| {
        let view = doc.view();
        let node = container.locate_in(view);
        let values = LIST.values(view, node, bar);
        a.push(format!(
            "{} {} {} {}",
            LIST.size(view, node),
            list(&values),
            bar(view, LIST.get(view, node, 2).unwrap()),
            index_of(doc, "y").unwrap()
        ));
        Ok(())
    });
    let remove = |wave: &mut Wave, label: &str, value: &str| {
        step(wave, label, |doc, a| {
            let removed = match index_of(doc, value) {
                Some(index) => LIST.remove(doc, &container, index)?,
                None => false,
            };
            a.push(removed.to_string());
            Ok(())
        });
    };
    remove(&mut wave, "remove y", "y");
    remove(&mut wave, "remove missing", "nope");
    step(&mut wave, "clear", |doc, _| LIST.clear(doc, &container));
    wave.check("adt-list");
}

trait LocateIn {
    fn locate_in(&self, view: &DocView) -> NodeId;
}

impl LocateIn for FirstTopLevel<'_> {
    fn locate_in(&self, view: &DocView) -> NodeId {
        crate::adt::Locator::locate(self, view).unwrap()
    }
}

#[test]
fn adt_singleton() {
    const SINGLE: DocumentBasedSingleton<'static> = DocumentBasedSingleton::new("foo");
    let read = |doc: &MutableDocument<'_>, a: &mut Vec<String>| {
        let view = doc.view();
        let element = SINGLE.get(view, view.root());
        a.push(format!(
            "{} {}",
            SINGLE.has_value(view, view.root()),
            nullable(element.map(|e| bar(view, e)))
        ));
        Ok(())
    };
    let mut wave = Wave::new();
    let set = |wave: &mut Wave, label: &str, value: &str| {
        step(wave, label, |doc, a| {
            let element = SINGLE.set(doc, &Root, &bar_attributes(Some(value))?)?;
            a.push(bar(doc.view(), element));
            Ok(())
        });
    };
    step(&mut wave, "read empty", |doc, a| read(doc, a));
    set(&mut wave, "set 1", "1");
    set(&mut wave, "set 2", "2");
    append(
        &mut wave,
        "foreign content",
        "<foo bar=\"7\"></foo><w><foo bar=\"8\"></foo></w>",
    );
    step(&mut wave, "read", |doc, a| read(doc, a));
    set(&mut wave, "set 3 cleans up", "3");
    append(&mut wave, "more foreign content", "<foo bar=\"9\"></foo>");
    step(&mut wave, "clear", |doc, _| SINGLE.clear(doc, &Root));
    step(&mut wave, "clear empty", |doc, _| SINGLE.clear(doc, &Root));
    wave.check("adt-singleton");
}

#[test]
fn adt_structured() {
    let value: DocumentBasedStructuredValue<&str, i32> = DocumentBasedStructuredValue::new();
    const BASIC: DocumentBasedBasicValue<'static, i32> = DocumentBasedBasicValue::new("alpha");
    let element = |doc: &MutableDocument<'_>| FirstTopLevel("s").locate_in(doc.view());
    let read = |doc: &MutableDocument<'_>, a: &mut Vec<String>| {
        let view = doc.view();
        let e = element(doc);
        a.push(format!(
            "{} {} {}",
            nullable(value.get(view, e, &"alpha")),
            nullable(value.get(view, e, &"beta")),
            nullable(BASIC.get(view, e))
        ));
        Ok(())
    };
    let mut wave = Wave::new();
    append(&mut wave, "element", "<s></s>");
    step(&mut wave, "set alpha", |doc, _| {
        value.set(doc, element(doc), &"alpha", Some(&5))
    });
    step(&mut wave, "set alpha same", |doc, _| {
        value.set(doc, element(doc), &"alpha", Some(&5))
    });
    for label in ["set both", "set both same"] {
        step(&mut wave, label, |doc, _| {
            value.set_all(doc, element(doc), &[("alpha", Some(6)), ("beta", Some(7))])
        });
    }
    step(&mut wave, "read", |doc, a| read(doc, a));
    step(&mut wave, "remove beta", |doc, _| {
        value.set(doc, element(doc), &"beta", None)
    });
    step(&mut wave, "basic set", |doc, _| {
        BASIC.set(doc, element(doc), Some(&9))
    });
    step(&mut wave, "basic set same", |doc, _| {
        BASIC.set(doc, element(doc), Some(&9))
    });
    step(&mut wave, "basic remove", |doc, _| {
        BASIC.set(doc, element(doc), None)
    });
    step(&mut wave, "read again", |doc, a| read(doc, a));
    wave.check("adt-structured");
}
