//! Ports of the tests in `org.waveprotocol.wave.model.adt.docbased` (and of
//! the implementation-independent `adt.Observable*TestBase` tests they
//! inherit), without the listener and event tests.
//!
//! The original tests keep a live object and let "remote" changes reach it
//! through document events; here the same documents are read afresh.

use super::*;
use crate::doc::{StandaloneDocument, XmlFragment};

/// A document with the given content.
fn document(xml: &str) -> StandaloneDocument {
    let mut doc = StandaloneDocument::default();
    if !xml.is_empty() {
        let fragment = XmlFragment::parse(xml).unwrap();
        doc.mutable().insert_xml(0, &fragment).unwrap();
    }
    doc
}

fn xml(doc: &StandaloneDocument) -> String {
    crate::doc::DocumentSink::document(doc).to_xml_string()
}

fn s(text: &str) -> String {
    text.to_owned()
}

// ---- serializer ----

#[test]
fn serializers_follow_java() {
    assert_eq!(i32::from_attribute("+12"), Some(12));
    assert_eq!(i32::from_attribute("-0"), Some(0));
    assert_eq!(i32::from_attribute("2147483648"), None);
    assert_eq!(i32::from_attribute(" 1"), None);
    assert_eq!(i32::from_attribute(""), None);
    assert_eq!(i64::from_attribute("9223372036854775807"), Some(i64::MAX));
    assert_eq!(bool::from_attribute("TRUE"), Some(true));
    assert_eq!(bool::from_attribute("yes"), Some(false));
    assert_eq!(true.to_attribute().unwrap(), "true");
    assert_eq!(f64::from_attribute(" 1e1 "), Some(10.0));
    assert_eq!(f64::from_attribute("0x1p3"), Some(8.0));
    assert_eq!(f64::from_attribute("0x.8p1"), Some(1.0));
    assert_eq!(f64::from_attribute("1.5f"), Some(1.5));
    assert_eq!(f64::from_attribute("-Infinity"), Some(f64::NEG_INFINITY));
    assert_eq!(f64::from_attribute("1e400"), Some(f64::INFINITY));
    assert!(f64::from_attribute("NaN").unwrap().is_nan());
    for bad in ["", "nan", "inf", "1e", ".", "0x10", "1L", "--1", "1,5"] {
        assert_eq!(f64::from_attribute(bad), None, "{bad}");
    }
    assert_eq!(0.5f64.to_attribute().unwrap(), "0.5");
    assert_eq!(100f64.to_attribute().unwrap(), "100.0");
    assert_eq!(1e7f64.to_attribute().unwrap(), "1.0E7");
    assert_eq!(1.0E-4f64.to_attribute().unwrap(), "1.0E-4");
    assert_eq!((-0.0f64).to_attribute().unwrap(), "-0.0");
    assert_eq!(
        f64::MIN_POSITIVE.to_attribute().unwrap(),
        "2.2250738585072014E-308"
    );
    assert_eq!(5e-324f64.to_attribute().unwrap(), "4.9E-324");
    assert_eq!(f64::NAN.to_attribute().unwrap(), "NaN");
    assert_eq!(f64::INFINITY.to_attribute().unwrap(), "Infinity");

    let id = crate::id::WaveletId::of("example.com", "conv+root").unwrap();
    assert_eq!(id.to_attribute().unwrap(), "example.com/conv+root");
    assert_eq!(
        crate::id::WaveletId::from_attribute("example.com/conv+root"),
        Some(id)
    );
    assert_eq!(crate::id::WaveletId::from_attribute("conv+root"), None);
    let legacy = crate::id::WaveletId::of_legacy("example.com", "conv/root").unwrap();
    assert!(matches!(
        legacy.to_attribute(),
        Err(AdtError::Unserialisable(_))
    ));

    let version = crate::version::HashedVersion::of(12, b"hash".to_vec());
    assert_eq!(version.to_attribute().unwrap(), "12:aGFzaA==");
    assert_eq!(
        crate::version::HashedVersion::from_attribute("12:aGFzaA=="),
        Some(version)
    );
    assert_eq!(
        crate::version::HashedVersion::from_attribute("7"),
        Some(crate::version::HashedVersion::unsigned(7))
    );
    assert_eq!(crate::version::HashedVersion::from_attribute("x"), None);
}

// ---- InitializerTest, CompoundInitializerTest ----

#[test]
fn initialise_attribute_fails_on_duplicate_key() {
    let mut attrs = InitialAttributes::new();
    attrs.insert(s("k1"), s("v1"));
    assert_eq!(
        initialise_attribute(&mut attrs, "k1", "v2"),
        Err(AdtError::DuplicateInitialiserKey(s("k1")))
    );
    assert_eq!(attrs["k1"], "v1");
}

#[test]
fn empty_compound_initialiser_does_nothing() {
    let init = CompoundInitializer::new();
    assert!(build_attributes(&init).unwrap().is_empty());
}

#[test]
fn compound_initialiser_composes_attributes() {
    let init = CompoundInitializer::new()
        .with(|target: &mut InitialAttributes| {
            target.insert(s("k1"), s("v1"));
            Ok(())
        })
        .with(|target: &mut InitialAttributes| {
            target.insert(s("k2"), s("v2"));
            Ok(())
        });
    let expected: InitialAttributes = [(s("k1"), s("v1")), (s("k2"), s("v2"))].into();
    assert_eq!(build_attributes(&init).unwrap(), expected);
}

#[test]
fn compound_initialiser_reports_duplicate_keys() {
    let value = DocumentBasedBasicValue::<String>::new("k");
    let init = CompoundInitializer::new()
        .with(value.create_initialiser(Some(&s("a"))))
        .with(value.create_initialiser(Some(&s("b"))));
    assert_eq!(
        build_attributes(&init),
        Err(AdtError::DuplicateInitialiserKey(s("k")))
    );
}

// ---- DocumentBasedBasicMapTest, ObservableBasicMapTestBase ----

mod basic_map {
    use super::*;

    const KEY1: &str = "bEefFaCe*2";
    const KEY2: &str = "caFeBabE*9";
    const MAP: DocumentBasedBasicMap<'static, String, i32> =
        DocumentBasedBasicMap::new("read", "blipId", "version");
    const CONTAINER: FirstTopLevel<'static> = FirstTopLevel("supplement");

    /// The document the original's `substrate` builds.
    fn substrate(entries: &[(&str, i32)]) -> String {
        let entries: String = entries
            .iter()
            .map(|(key, value)| format!("<read blipId=\"{key}\" version=\"{value}\"/>"))
            .collect();
        if entries.is_empty() {
            s("<supplement/>")
        } else {
            format!("<supplement>{entries}</supplement>")
        }
    }

    fn target(entries: &[(&str, i32)]) -> StandaloneDocument {
        document(&substrate(entries))
    }

    fn get(doc: &mut StandaloneDocument, key: &str) -> Option<i32> {
        let doc = doc.mutable();
        let container = CONTAINER.locate(doc.view()).unwrap();
        MAP.get(doc.view(), container, &s(key))
    }

    fn keys(doc: &mut StandaloneDocument) -> Vec<String> {
        let doc = doc.mutable();
        let container = CONTAINER.locate(doc.view()).unwrap();
        MAP.key_set(doc.view(), container)
    }

    fn put(doc: &mut StandaloneDocument, key: &str, value: i32) -> bool {
        MAP.put(&mut doc.mutable(), &CONTAINER, &s(key), &value)
            .unwrap()
    }

    fn remove(doc: &mut StandaloneDocument, key: &str) {
        MAP.remove(&mut doc.mutable(), &CONTAINER, &s(key)).unwrap();
    }

    #[test]
    fn put_on_empty_map_is_returned_by_get() {
        let mut doc = target(&[]);
        put(&mut doc, KEY1, 10);
        assert_eq!(get(&mut doc, KEY1), Some(10));
    }

    #[test]
    fn put_on_empty_map_inserts_into_substrate() {
        let mut doc = target(&[]);
        put(&mut doc, KEY1, 10);
        assert_eq!(xml(&doc), substrate(&[(KEY1, 10)]));
    }

    #[test]
    fn load_leaves_overridden_entries_in_substrate_but_cleans_on_write() {
        let mut doc = target(&[(KEY1, 10), (KEY2, 20), (KEY1, 30)]);
        assert_eq!(get(&mut doc, KEY1), Some(30));
        assert_eq!(xml(&doc), substrate(&[(KEY1, 10), (KEY2, 20), (KEY1, 30)]));
        put(&mut doc, KEY2, 50);
        assert_eq!(xml(&doc), substrate(&[(KEY2, 50), (KEY1, 30)]));
    }

    #[test]
    fn put_of_later_values_replace_old() {
        let mut doc = target(&[(KEY1, 10), (KEY2, 20)]);
        put(&mut doc, KEY1, 5);
        assert_eq!(get(&mut doc, KEY1), Some(5));
        assert_eq!(xml(&doc), substrate(&[(KEY1, 5), (KEY2, 20)]));
        put(&mut doc, KEY1, 30);
        assert_eq!(get(&mut doc, KEY1), Some(30));
        assert_eq!(xml(&doc), substrate(&[(KEY1, 30), (KEY2, 20)]));
    }

    #[test]
    fn remote_added_later_value_obviates_old_entry() {
        // The state after the original's remote removal of KEY1's entry
        // and remote insertion of a new one at the start.
        let mut doc = target(&[(KEY1, 10), (KEY2, 20)]);
        assert_eq!(get(&mut doc, KEY1), Some(10));
        put(&mut doc, KEY2, 50);
        assert_eq!(xml(&doc), substrate(&[(KEY2, 50), (KEY1, 10)]));
    }

    #[test]
    fn get_put() {
        let mut doc = target(&[]);
        assert_eq!(get(&mut doc, "A"), None);
        assert_eq!(get(&mut doc, "B"), None);
        assert!(put(&mut doc, "A", 1));
        assert!(put(&mut doc, "B", 2));
        assert_eq!(get(&mut doc, "A"), Some(1));
        assert_eq!(get(&mut doc, "B"), Some(2));
        assert!(put(&mut doc, "A", 3));
        assert_eq!(get(&mut doc, "A"), Some(3));
        assert!(!put(&mut doc, "A", 3));
        assert_eq!(get(&mut doc, "A"), Some(3));
    }

    #[test]
    fn key_set() {
        let mut doc = target(&[]);
        assert!(keys(&mut doc).is_empty());
        put(&mut doc, "A", 1);
        put(&mut doc, "B", 2);
        assert_eq!(keys(&mut doc), ["A", "B"]);
        put(&mut doc, "C", 3);
        assert_eq!(keys(&mut doc), ["A", "B", "C"]);
        remove(&mut doc, "B");
        assert_eq!(keys(&mut doc), ["A", "C"]);
    }

    #[test]
    fn remove_removes() {
        let mut doc = target(&[]);
        assert_eq!(get(&mut doc, "A"), None);
        put(&mut doc, "A", 1);
        assert_eq!(get(&mut doc, "A"), Some(1));
        remove(&mut doc, "A");
        assert_eq!(get(&mut doc, "A"), None);
    }

    #[test]
    fn malformed_entries_are_ignored_and_kept() {
        let content = "<supplement><read blipId=\"a\" version=\"x\"/><read version=\"1\"/>\
                       <read blipId=\"a\"/><other blipId=\"a\" version=\"3\"/>text</supplement>";
        let mut doc = document(content);
        assert_eq!(get(&mut doc, "a"), None);
        assert!(keys(&mut doc).is_empty());
        put(&mut doc, "a", 2);
        assert_eq!(get(&mut doc, "a"), Some(2));
        assert_eq!(
            xml(&doc),
            content.replace(
                "<supplement>",
                "<supplement><read blipId=\"a\" version=\"2\"/>"
            )
        );
    }

    #[test]
    fn missing_container_is_an_error() {
        let mut doc = document("<other/>");
        assert_eq!(
            MAP.put(&mut doc.mutable(), &CONTAINER, &s("a"), &1),
            Err(AdtError::MissingContainer)
        );
        assert_eq!(doc.ops().len(), 1);
    }
}

// ---- DocumentBasedMonotonicMapTest ----

mod monotonic_map {
    use super::*;

    const KEY1: &str = "bEefFaCe*2";
    const KEY2: &str = "caFeBabE*9";
    const MAP: DocumentBasedMonotonicMap<'static, String, i32> =
        DocumentBasedMonotonicMap::new("read", "blipId", "version");
    const CONTAINER: FirstTopLevel<'static> = FirstTopLevel("supplement");

    fn substrate(entries: &[(&str, i32)]) -> String {
        let entries: String = entries
            .iter()
            .map(|(key, value)| format!("<read blipId=\"{key}\" version=\"{value}\"/>"))
            .collect();
        if entries.is_empty() {
            s("<supplement/>")
        } else {
            format!("<supplement>{entries}</supplement>")
        }
    }

    fn target(entries: &[(&str, i32)]) -> StandaloneDocument {
        document(&substrate(entries))
    }

    fn get(doc: &mut StandaloneDocument, key: &str) -> Option<i32> {
        let doc = doc.mutable();
        let container = CONTAINER.locate(doc.view()).unwrap();
        MAP.get(doc.view(), container, &s(key))
    }

    fn put(doc: &mut StandaloneDocument, key: &str, value: i32) -> bool {
        MAP.put(&mut doc.mutable(), &CONTAINER, &s(key), &value)
            .unwrap()
    }

    #[test]
    fn put_on_empty_map_is_returned_by_get() {
        let mut doc = target(&[]);
        put(&mut doc, KEY1, 10);
        assert_eq!(get(&mut doc, KEY1), Some(10));
    }

    #[test]
    fn put_on_empty_map_inserts_into_substrate() {
        let mut doc = target(&[]);
        put(&mut doc, KEY1, 10);
        assert_eq!(xml(&doc), substrate(&[(KEY1, 10)]));
    }

    #[test]
    fn load_leaves_overridden_entries_in_substrate_but_cleans_on_write() {
        let mut doc = target(&[(KEY1, 10), (KEY2, 20), (KEY1, 30)]);
        assert_eq!(get(&mut doc, KEY1), Some(30));
        assert_eq!(xml(&doc), substrate(&[(KEY1, 10), (KEY2, 20), (KEY1, 30)]));
        put(&mut doc, KEY2, 50);
        assert_eq!(xml(&doc), substrate(&[(KEY2, 50), (KEY1, 30)]));
    }

    #[test]
    fn put_of_a_lesser_value_does_nothing() {
        let mut doc = target(&[(KEY1, 10), (KEY2, 20)]);
        assert!(!put(&mut doc, KEY1, 5));
        assert_eq!(get(&mut doc, KEY1), Some(10));
        assert_eq!(xml(&doc), substrate(&[(KEY1, 10), (KEY2, 20)]));
        assert!(doc.ops().len() == 1);
    }

    #[test]
    fn put_of_an_equal_value_does_nothing() {
        let mut doc = target(&[(KEY1, 10), (KEY2, 20)]);
        assert!(!put(&mut doc, KEY2, 20));
        assert_eq!(xml(&doc), substrate(&[(KEY1, 10), (KEY2, 20)]));
    }

    #[test]
    fn remove_put_with_lesser_values() {
        let mut doc = target(&[(KEY1, 10), (KEY2, 20)]);
        MAP.remove(&mut doc.mutable(), &CONTAINER, &s(KEY1))
            .unwrap();
        put(&mut doc, KEY1, 5);
        assert_eq!(get(&mut doc, KEY1), Some(5));
        assert_eq!(xml(&doc), substrate(&[(KEY1, 5), (KEY2, 20)]));
    }

    #[test]
    fn put_of_a_greater_value_replaces_old() {
        let mut doc = target(&[(KEY1, 10), (KEY2, 20)]);
        assert!(put(&mut doc, KEY1, 30));
        assert_eq!(get(&mut doc, KEY1), Some(30));
        assert_eq!(xml(&doc), substrate(&[(KEY1, 30), (KEY2, 20)]));
    }

    #[test]
    fn remote_added_lesser_entries_get_scheduled_for_removal() {
        let mut doc = target(&[(KEY1, 30), (KEY2, 20), (KEY1, 10)]);
        assert_eq!(get(&mut doc, KEY1), Some(30));
        put(&mut doc, KEY2, 50);
        assert_eq!(xml(&doc), substrate(&[(KEY2, 50), (KEY1, 30)]));
    }

    #[test]
    fn remote_added_greater_entries_obviate_old_entry() {
        let mut doc = target(&[(KEY1, 10), (KEY2, 20), (KEY1, 30)]);
        assert_eq!(get(&mut doc, KEY1), Some(30));
        put(&mut doc, KEY1, 50);
        assert_eq!(xml(&doc), substrate(&[(KEY1, 50), (KEY2, 20)]));
    }

    #[test]
    fn first_of_equal_values_counts() {
        let mut doc = target(&[(KEY1, 30), (KEY1, 30), (KEY2, 20)]);
        MAP.remove(&mut doc.mutable(), &CONTAINER, &s(KEY2))
            .unwrap();
        assert_eq!(xml(&doc), substrate(&[(KEY1, 30)]));
        // Two deletions, in document order: the second KEY1 entry (now at
        // location 3), then KEY2's.
        assert_eq!(doc.ops().len(), 3);
    }
}

// ---- DocumentBasedBasicSetTest, ObservableBasicSetTestBase ----

mod basic_set {
    use super::*;

    const INTS: DocumentBasedBasicSet<'static, i32> = DocumentBasedBasicSet::new("folder", "id");
    const STRINGS: DocumentBasedBasicSet<'static, String> =
        DocumentBasedBasicSet::new("read", "blipId");
    const CONTAINER: FirstTopLevel<'static> = FirstTopLevel("supplement");

    fn substrate(values: &[i32]) -> String {
        let entries: String = values
            .iter()
            .map(|value| format!("<folder id=\"{value}\"/>"))
            .collect();
        if entries.is_empty() {
            s("<supplement/>")
        } else {
            format!("<supplement>{entries}</supplement>")
        }
    }

    fn values(doc: &mut StandaloneDocument) -> Vec<i32> {
        let doc = doc.mutable();
        let container = CONTAINER.locate(doc.view()).unwrap();
        INTS.values(doc.view(), container)
    }

    #[test]
    fn add_on_empty_state_is_returned_by_get() {
        let mut doc = document(&substrate(&[]));
        INTS.add(&mut doc.mutable(), &CONTAINER, &10).unwrap();
        assert_eq!(values(&mut doc), [10]);
    }

    #[test]
    fn add_on_empty_state_inserts_into_substrate() {
        let mut doc = document(&substrate(&[]));
        INTS.add(&mut doc.mutable(), &CONTAINER, &10).unwrap();
        assert_eq!(xml(&doc), substrate(&[10]));
    }

    #[test]
    fn single_substrate_value_is_reflected() {
        assert_eq!(values(&mut document(&substrate(&[10]))), [10]);
    }

    #[test]
    fn multiple_substrate_values_are_reflected() {
        assert_eq!(values(&mut document(&substrate(&[30, 10]))), [10, 30]);
    }

    #[test]
    fn duplicate_substrate_values_are_not_exposed() {
        assert_eq!(values(&mut document(&substrate(&[10, 10]))), [10]);
    }

    fn strings(doc: &mut StandaloneDocument) -> Vec<String> {
        let doc = doc.mutable();
        STRINGS.values(doc.view(), doc.view().root())
    }

    fn contains(doc: &mut StandaloneDocument, value: &str) -> bool {
        let doc = doc.mutable();
        STRINGS.contains(doc.view(), doc.view().root(), &s(value))
    }

    #[test]
    fn add_remove_contains() {
        let mut doc = document("");
        assert!(strings(&mut doc).is_empty());
        for value in ["A", "B", "C"] {
            assert!(STRINGS.add(&mut doc.mutable(), &Root, &s(value)).unwrap());
        }
        assert_eq!(strings(&mut doc), ["A", "B", "C"]);
        assert!(contains(&mut doc, "B"));
        assert!(!contains(&mut doc, "D"));
        assert!(!STRINGS.add(&mut doc.mutable(), &Root, &s("B")).unwrap());
        assert_eq!(strings(&mut doc), ["A", "B", "C"]);
        assert!(STRINGS.remove(&mut doc.mutable(), &Root, &s("B")).unwrap());
        assert_eq!(strings(&mut doc), ["A", "C"]);
        assert!(!contains(&mut doc, "B"));
        STRINGS.remove(&mut doc.mutable(), &Root, &s("C")).unwrap();
        STRINGS.remove(&mut doc.mutable(), &Root, &s("A")).unwrap();
        assert!(strings(&mut doc).is_empty());
        assert_eq!(xml(&doc), "");
    }

    #[test]
    fn clear() {
        let mut doc = document("");
        for value in ["A", "B", "C"] {
            STRINGS.add(&mut doc.mutable(), &Root, &s(value)).unwrap();
        }
        assert_eq!(strings(&mut doc), ["A", "B", "C"]);
        assert!(STRINGS.clear(&mut doc.mutable(), &Root).unwrap());
        assert!(strings(&mut doc).is_empty());
        assert!(!STRINGS.clear(&mut doc.mutable(), &Root).unwrap());
    }

    #[test]
    fn writes_delete_redundant_elements_only() {
        let content = "<supplement><folder id=\"3\"/><x/><folder id=\"3\"/><folder/>\
                       <folder id=\"y\"/><folder id=\"4\"/><folder id=\"3\"/></supplement>";
        let mut doc = document(content);
        assert_eq!(values(&mut doc), [3, 4]);
        // Removing a value deletes the redundant elements too, but not the
        // elements that are not values.
        INTS.remove(&mut doc.mutable(), &CONTAINER, &4).unwrap();
        assert_eq!(
            xml(&doc),
            "<supplement><folder id=\"3\"/><x/><folder/><folder id=\"y\"/></supplement>"
        );
        INTS.clear(&mut doc.mutable(), &CONTAINER).unwrap();
        assert_eq!(
            xml(&doc),
            "<supplement><x/><folder/><folder id=\"y\"/></supplement>"
        );
    }
}

// ---- DocumentBasedBooleanTest ----

mod boolean {
    use super::*;

    const TARGET: DocumentBasedBoolean<'static> = DocumentBasedBoolean::new("muted", "muted");

    fn target(state: &[Option<bool>]) -> StandaloneDocument {
        let content: String = state
            .iter()
            .map(|value| match value {
                Some(value) => format!("<muted muted=\"{value}\"/>"),
                None => s("<muted/>"),
            })
            .collect();
        document(&content)
    }

    fn get(doc: &mut StandaloneDocument) -> Option<bool> {
        let doc = doc.mutable();
        TARGET.get(doc.view(), doc.view().root())
    }

    fn set(doc: &mut StandaloneDocument, value: Option<bool>) {
        TARGET.set(&mut doc.mutable(), &Root, value).unwrap();
    }

    #[test]
    fn initial_state_is_null() {
        assert_eq!(get(&mut target(&[])), None);
    }

    #[test]
    fn set_true_on_initial_state_makes_true() {
        let mut doc = target(&[]);
        set(&mut doc, Some(true));
        assert_eq!(get(&mut doc), Some(true));
    }

    #[test]
    fn set_false_on_initial_state_makes_false() {
        let mut doc = target(&[]);
        set(&mut doc, Some(false));
        assert_eq!(get(&mut doc), Some(false));
    }

    #[test]
    fn backwards_compatible_with_true() {
        assert_eq!(get(&mut target(&[Some(true)])), Some(true));
    }

    #[test]
    fn backwards_compatible_with_corrupt_true() {
        assert_eq!(get(&mut target(&[Some(true), Some(true)])), Some(true));
    }

    #[test]
    fn backwards_compatible_with_false() {
        assert_eq!(get(&mut target(&[Some(false)])), Some(false));
    }

    #[test]
    fn backwards_compatible_with_corrupt_false() {
        assert_eq!(get(&mut target(&[Some(false), Some(false)])), Some(false));
    }

    #[test]
    fn write_true_cleans_up() {
        let mut doc = target(&[Some(false), Some(false)]);
        set(&mut doc, Some(true));
        assert_eq!(xml(&doc), xml(&target(&[Some(true)])));
    }

    #[test]
    fn write_false_cleans_up() {
        let mut doc = target(&[Some(false), Some(true), None]);
        // An element without the attribute counts as true.
        assert_eq!(get(&mut doc), Some(true));
        set(&mut doc, Some(false));
        assert_eq!(xml(&doc), xml(&target(&[Some(false)])));
    }

    #[test]
    fn write_null_cleans_up() {
        let mut doc = target(&[Some(false), Some(true), None]);
        set(&mut doc, None);
        assert_eq!(xml(&doc), "");
    }

    #[test]
    fn write_does_not_cleanup_foreign_elements() {
        let mut doc = document("<muted muted=\"false\"/><muted muted=\"true\"/><foreign/>");
        set(&mut doc, None);
        assert_eq!(xml(&doc), "<foreign/>");
    }

    #[test]
    fn write_of_current_value_does_nothing() {
        let mut doc = target(&[Some(false), Some(true)]);
        assert!(!TARGET.set(&mut doc.mutable(), &Root, Some(true)).unwrap());
        assert_eq!(xml(&doc), xml(&target(&[Some(false), Some(true)])));
    }
}

// ---- DocumentBasedMonotonicValueTest ----

mod monotonic_value {
    use super::*;

    const VALUE: DocumentBasedMonotonicValue<'static, i32> =
        DocumentBasedMonotonicValue::new("all", "read");
    const CONTAINER: FirstTopLevel<'static> = FirstTopLevel("supplement");

    fn substrate(values: &[i32]) -> String {
        let entries: String = values
            .iter()
            .map(|value| format!("<all read=\"{value}\"/>"))
            .collect();
        if entries.is_empty() {
            s("<supplement/>")
        } else {
            format!("<supplement>{entries}</supplement>")
        }
    }

    fn get(doc: &mut StandaloneDocument) -> Option<i32> {
        let doc = doc.mutable();
        let container = CONTAINER.locate(doc.view()).unwrap();
        VALUE.get(doc.view(), container)
    }

    fn set(doc: &mut StandaloneDocument, value: i32) -> bool {
        VALUE.set(&mut doc.mutable(), &CONTAINER, &value).unwrap()
    }

    #[test]
    fn set_on_empty_state_is_returned_by_get_and_inserts_into_substrate() {
        let mut doc = document(&substrate(&[]));
        assert_eq!(get(&mut doc), None);
        assert!(set(&mut doc, 10));
        assert_eq!(get(&mut doc), Some(10));
        assert_eq!(xml(&doc), substrate(&[10]));
    }

    #[test]
    fn leaves_obsolete_entries_on_load_but_cleans_on_write() {
        let mut doc = document(&substrate(&[10, 30, 20]));
        assert_eq!(get(&mut doc), Some(30));
        assert_eq!(xml(&doc), substrate(&[10, 30, 20]));
        set(&mut doc, 40);
        assert_eq!(get(&mut doc), Some(40));
        assert_eq!(xml(&doc), substrate(&[40]));
    }

    #[test]
    fn set_of_a_lesser_values_does_nothing() {
        let mut doc = document(&substrate(&[10]));
        assert!(!set(&mut doc, 5));
        assert_eq!(get(&mut doc), Some(10));
        assert_eq!(xml(&doc), substrate(&[10]));
    }

    #[test]
    fn set_of_a_greater_values_replaces_old() {
        let mut doc = document(&substrate(&[10]));
        set(&mut doc, 20);
        assert_eq!(get(&mut doc), Some(20));
        assert_eq!(xml(&doc), substrate(&[20]));
    }

    #[test]
    fn remote_added_lesser_entries_scheduled_for_removal() {
        let mut doc = document(&substrate(&[20, 10]));
        assert_eq!(get(&mut doc), Some(20));
        set(&mut doc, 30);
        assert_eq!(get(&mut doc), Some(30));
        assert_eq!(xml(&doc), substrate(&[30]));
    }

    #[test]
    fn remote_added_greater_entries_replace_old_entry_but_is_not_cleaned() {
        let mut doc = document(&substrate(&[10, 20]));
        assert_eq!(get(&mut doc), Some(20));
        assert_eq!(xml(&doc), substrate(&[10, 20]));
    }

    #[test]
    fn set_of_an_equal_value_does_nothing() {
        let mut doc = document(&substrate(&[10, 20]));
        assert!(!set(&mut doc, 20));
        assert_eq!(xml(&doc), substrate(&[10, 20]));
    }
}

// ---- DocumentBasedBasicValueTest ----

mod basic_value {
    use super::*;

    const VALUE: DocumentBasedBasicValue<'static, i32> = DocumentBasedBasicValue::new("state");

    fn substrate(value: i32) -> String {
        format!("<inboxing state=\"{value}\"/>")
    }

    fn get(doc: &mut StandaloneDocument) -> Option<i32> {
        let doc = doc.mutable();
        let element = FirstTopLevel("inboxing").locate(doc.view()).unwrap();
        VALUE.get(doc.view(), element)
    }

    fn set(doc: &mut StandaloneDocument, value: Option<i32>) {
        let mut doc = doc.mutable();
        let element = FirstTopLevel("inboxing").locate(doc.view()).unwrap();
        VALUE.set(&mut doc, element, value.as_ref()).unwrap();
    }

    #[test]
    fn initialiser_encodes_value() {
        let init =
            DocumentBasedBasicValue::<String>::new("foo").create_initialiser(Some(&s("bar")));
        let expected: InitialAttributes = [(s("foo"), s("bar"))].into();
        assert_eq!(build_attributes(&init).unwrap(), expected);
    }

    #[test]
    fn initialiser_skips_null_value() {
        let init = DocumentBasedBasicValue::<String>::new("foo").create_initialiser(None);
        assert!(build_attributes(&init).unwrap().is_empty());
    }

    #[test]
    fn set_on_empty_state_is_returned_by_get() {
        let mut doc = document(&substrate(0));
        set(&mut doc, Some(10));
        assert_eq!(get(&mut doc), Some(10));
    }

    #[test]
    fn set_on_empty_state_inserts_into_substrate() {
        let mut doc = document(&substrate(0));
        set(&mut doc, Some(10));
        assert_eq!(xml(&doc), substrate(10));
    }

    #[test]
    fn substrate_value_is_reflected() {
        assert_eq!(get(&mut document(&substrate(10))), Some(10));
    }

    #[test]
    fn set_replaces_value() {
        let mut doc = document(&substrate(10));
        set(&mut doc, Some(5));
        assert_eq!(get(&mut doc), Some(5));
        assert_eq!(xml(&doc), substrate(5));
    }

    #[test]
    fn remote_set_replaces_value() {
        let mut doc = document(&substrate(10));
        {
            let mut doc = doc.mutable();
            let element = FirstTopLevel("inboxing").locate(doc.view()).unwrap();
            doc.set_element_attribute(element, "state", Some("5"))
                .unwrap();
        }
        assert_eq!(get(&mut doc), Some(5));
        assert_eq!(xml(&doc), substrate(5));
    }

    #[test]
    fn set_null_removes_the_attribute_and_same_value_emits_nothing() {
        let mut doc = document(&substrate(10));
        set(&mut doc, Some(10));
        assert_eq!(doc.ops().len(), 1);
        set(&mut doc, None);
        assert_eq!(get(&mut doc), None);
        assert_eq!(xml(&doc), "<inboxing/>");
    }
}

// ---- DocumentBasedElementListTest ----

mod element_list {
    use super::*;

    const WORD: [&str; 3] = ["lah", "dee", "dah"];
    const LIST: DocumentBasedElementList<'static> = DocumentBasedElementList::new("foo");
    const VALUE: DocumentBasedBasicValue<'static, String> = DocumentBasedBasicValue::new("bar");
    const FRESH: FirstTopLevel<'static> = FirstTopLevel("blank");
    const PREPOPULATED: FirstTopLevel<'static> = FirstTopLevel("preopopulated");
    const PREPOPULATED_SIZE: usize = 3;

    /// The document the original's `setUp` builds.
    fn lists() -> StandaloneDocument {
        document(
            "<blank/><preopopulated><random flubble=\"huh\" foo=\"1\"/>\
             <foo bar=\"0\"/><foo bar=\"1\"/><foo bar=\"2\"/></preopopulated>",
        )
    }

    fn attributes(value: Option<&str>) -> InitialAttributes {
        let value = value.map(str::to_owned);
        build_attributes(&VALUE.create_initialiser(value.as_ref())).unwrap()
    }

    /// The values of a list; `None` for an item without the attribute.
    fn values(doc: &mut StandaloneDocument, list: &dyn Locator) -> Vec<Option<String>> {
        let doc = doc.mutable();
        let container = list.locate(doc.view()).unwrap();
        LIST.values(doc.view(), container, |view, element| {
            VALUE.get(view, element)
        })
    }

    fn size(doc: &mut StandaloneDocument, list: &dyn Locator) -> usize {
        let doc = doc.mutable();
        let container = list.locate(doc.view()).unwrap();
        LIST.size(doc.view(), container)
    }

    fn some(values: &[&str]) -> Vec<Option<String>> {
        values.iter().map(|value| Some(s(value))).collect()
    }

    #[test]
    fn empty_list() {
        assert_eq!(size(&mut lists(), &FRESH), 0);
    }

    #[test]
    fn add() {
        let mut doc = lists();
        for (index, word) in WORD.iter().enumerate() {
            {
                let mut doc = doc.mutable();
                let element = LIST.add(&mut doc, &FRESH, &attributes(None)).unwrap();
                VALUE.set(&mut doc, element, Some(&s(word))).unwrap();
            }
            assert_eq!(size(&mut doc, &FRESH), index + 1);
            assert_eq!(values(&mut doc, &FRESH)[index], Some(s(word)));
        }
    }

    #[test]
    fn add_with_initial_state() {
        let mut doc = lists();
        let mut mutable = doc.mutable();
        let element = LIST
            .add(&mut mutable, &FRESH, &attributes(Some("baz")))
            .unwrap();
        assert_eq!(VALUE.get(mutable.view(), element), Some(s("baz")));
    }

    #[test]
    fn remove() {
        let mut doc = lists();
        assert_eq!(size(&mut doc, &FRESH), 0);
        LIST.add(&mut doc.mutable(), &FRESH, &attributes(None))
            .unwrap();
        assert_eq!(size(&mut doc, &FRESH), 1);
        assert!(LIST.remove(&mut doc.mutable(), &FRESH, 0).unwrap());
        assert_eq!(size(&mut doc, &FRESH), 0);
        assert!(!LIST.remove(&mut doc.mutable(), &FRESH, 0).unwrap());
    }

    #[test]
    fn clear() {
        let mut doc = lists();
        LIST.clear(&mut doc.mutable(), &FRESH).unwrap();
        assert_eq!(size(&mut doc, &FRESH), 0);
        assert_ne!(size(&mut doc, &PREPOPULATED), 0);
        LIST.clear(&mut doc.mutable(), &PREPOPULATED).unwrap();
        assert_eq!(size(&mut doc, &PREPOPULATED), 0);
        assert_eq!(
            xml(&doc),
            "<blank/><preopopulated><random flubble=\"huh\" foo=\"1\"/></preopopulated>"
        );
    }

    #[test]
    fn can_iterate_over_values() {
        let mut doc = lists();
        for word in WORD {
            LIST.add(&mut doc.mutable(), &FRESH, &attributes(Some(word)))
                .unwrap();
        }
        assert_eq!(values(&mut doc, &FRESH), some(&WORD));
    }

    #[test]
    fn pre_populated() {
        let mut doc = lists();
        assert_eq!(size(&mut doc, &PREPOPULATED), PREPOPULATED_SIZE);
        assert_eq!(values(&mut doc, &PREPOPULATED), some(&["0", "1", "2"]));
    }

    #[test]
    fn index_of() {
        let mut doc = lists();
        let mut mutable = doc.mutable();
        let container = PREPOPULATED.locate(mutable.view()).unwrap();
        for index in 0..PREPOPULATED_SIZE {
            let element = LIST.get(mutable.view(), container, index).unwrap();
            assert_eq!(
                LIST.index_of(mutable.view(), container, element),
                Some(index)
            );
        }
        let last = LIST
            .add(&mut mutable, &PREPOPULATED, &attributes(None))
            .unwrap();
        let container = PREPOPULATED.locate(mutable.view()).unwrap();
        assert_eq!(
            LIST.index_of(mutable.view(), container, last),
            Some(PREPOPULATED_SIZE)
        );
        let fresh = FRESH.locate(mutable.view()).unwrap();
        assert_eq!(LIST.index_of(mutable.view(), fresh, last), None);
    }

    #[test]
    fn add_at_invalid_index() {
        let mut doc = lists();
        assert_eq!(
            LIST.add_at(
                &mut doc.mutable(),
                &PREPOPULATED,
                PREPOPULATED_SIZE + 1,
                &attributes(None)
            ),
            Err(AdtError::IndexOutOfBounds { index: 4, size: 3 })
        );
        assert_eq!(doc.ops().len(), 1);
    }

    #[test]
    fn add_at_index() {
        let mut doc = lists();
        LIST.add_at(&mut doc.mutable(), &FRESH, 0, &attributes(Some("first")))
            .unwrap();
        assert_eq!(values(&mut doc, &FRESH), some(&["first"]));

        LIST.add_at(
            &mut doc.mutable(),
            &PREPOPULATED,
            1,
            &attributes(Some("new second")),
        )
        .unwrap();
        assert_eq!(
            values(&mut doc, &PREPOPULATED),
            some(&["0", "new second", "1", "2"])
        );
        LIST.add_at(
            &mut doc.mutable(),
            &PREPOPULATED,
            0,
            &attributes(Some("new first")),
        )
        .unwrap();
        assert_eq!(
            values(&mut doc, &PREPOPULATED),
            some(&["new first", "0", "new second", "1", "2"])
        );
        // The first item goes before the first item's element, after the
        // foreign element.
        assert!(xml(&doc).contains("<random flubble=\"huh\" foo=\"1\"/><foo bar=\"new first\"/>"));
    }
}

// ---- DocumentBasedStructuredValueTest ----

mod structured_value {
    use super::*;

    const NAME1: &str = "NAME1";
    const NAME2: &str = "NAME2";

    fn value() -> DocumentBasedStructuredValue<&'static str, i32> {
        DocumentBasedStructuredValue::new()
    }

    fn substrate(values: &[(&str, i32)]) -> String {
        let attributes: String = values
            .iter()
            .map(|(name, value)| format!(" {name}=\"{value}\""))
            .collect();
        format!("<container{attributes}/>")
    }

    fn get(doc: &mut StandaloneDocument, name: &'static str) -> Option<i32> {
        let doc = doc.mutable();
        let element = FirstTopLevel("container").locate(doc.view()).unwrap();
        value().get(doc.view(), element, &name)
    }

    fn set(doc: &mut StandaloneDocument, name: &'static str, new_value: Option<i32>) {
        let mut doc = doc.mutable();
        let element = FirstTopLevel("container").locate(doc.view()).unwrap();
        value()
            .set(&mut doc, element, &name, new_value.as_ref())
            .unwrap();
    }

    fn set_all(doc: &mut StandaloneDocument, values: &[(&'static str, Option<i32>)]) {
        let mut doc = doc.mutable();
        let element = FirstTopLevel("container").locate(doc.view()).unwrap();
        value().set_all(&mut doc, element, values).unwrap();
    }

    #[test]
    fn initialiser() {
        let init = value().create_initialiser(&[(NAME1, Some(23)), (NAME2, Some(42))]);
        let expected: InitialAttributes = [(s(NAME1), s("23")), (s(NAME2), s("42"))].into();
        assert_eq!(build_attributes(&init).unwrap(), expected);
        let init = value().create_initialiser(&[(NAME1, None)]);
        assert!(build_attributes(&init).unwrap().is_empty());
    }

    #[test]
    fn empty_attribute_is_null() {
        assert_eq!(get(&mut document(&substrate(&[])), NAME1), None);
    }

    #[test]
    fn set_on_empty_state_is_returned_by_get() {
        let mut doc = document(&substrate(&[]));
        set(&mut doc, NAME1, Some(42));
        assert_eq!(get(&mut doc, NAME1), Some(42));
        assert_eq!(get(&mut doc, NAME2), None);

        let mut doc = document(&substrate(&[]));
        set_all(&mut doc, &[(NAME1, Some(42))]);
        assert_eq!(get(&mut doc, NAME1), Some(42));
        assert_eq!(get(&mut doc, NAME2), None);
    }

    #[test]
    fn set_on_empty_state_inserts_into_substrate() {
        let mut doc = document(&substrate(&[]));
        set(&mut doc, NAME1, Some(10));
        assert_eq!(xml(&doc), substrate(&[(NAME1, 10)]));
        set(&mut doc, NAME2, Some(20));
        assert_eq!(xml(&doc), substrate(&[(NAME1, 10), (NAME2, 20)]));

        let mut doc = document(&substrate(&[]));
        set_all(&mut doc, &[(NAME1, Some(10))]);
        assert_eq!(xml(&doc), substrate(&[(NAME1, 10)]));
        set_all(&mut doc, &[(NAME2, Some(20))]);
        assert_eq!(xml(&doc), substrate(&[(NAME1, 10), (NAME2, 20)]));
    }

    #[test]
    fn substrate_value_is_reflected() {
        let mut doc = document(&substrate(&[(NAME1, 10)]));
        assert_eq!(get(&mut doc, NAME1), Some(10));
        assert_eq!(get(&mut doc, NAME2), None);
    }

    #[test]
    fn set_replaces_value() {
        let mut doc = document(&substrate(&[(NAME1, 10), (NAME2, 20)]));
        set(&mut doc, NAME1, Some(5));
        assert_eq!(get(&mut doc, NAME1), Some(5));
        assert_eq!(get(&mut doc, NAME2), Some(20));
        assert_eq!(xml(&doc), substrate(&[(NAME1, 5), (NAME2, 20)]));

        let mut doc = document(&substrate(&[(NAME1, 10), (NAME2, 20)]));
        set_all(&mut doc, &[(NAME1, Some(5))]);
        assert_eq!(get(&mut doc, NAME1), Some(5));
        assert_eq!(get(&mut doc, NAME2), Some(20));
        assert_eq!(xml(&doc), substrate(&[(NAME1, 5), (NAME2, 20)]));
    }

    #[test]
    fn set_null_clears_value() {
        let mut doc = document(&substrate(&[(NAME1, 10)]));
        set(&mut doc, NAME1, None);
        assert_eq!(get(&mut doc, NAME1), None);
        assert_eq!(xml(&doc), substrate(&[]));

        let mut doc = document(&substrate(&[(NAME1, 10)]));
        set_all(&mut doc, &[(NAME1, None)]);
        assert_eq!(get(&mut doc, NAME1), None);
        assert_eq!(xml(&doc), substrate(&[]));
    }

    #[test]
    fn remote_set_replaces_value() {
        let mut doc = document(&substrate(&[(NAME1, 10), (NAME2, 20)]));
        {
            let mut doc = doc.mutable();
            let element = FirstTopLevel("container").locate(doc.view()).unwrap();
            doc.set_element_attribute(element, NAME1, Some("5"))
                .unwrap();
        }
        assert_eq!(get(&mut doc, NAME1), Some(5));
        assert_eq!(get(&mut doc, NAME2), Some(20));
        assert_eq!(xml(&doc), substrate(&[(NAME1, 5), (NAME2, 20)]));
    }
}

// ---- ObservableSingletonTestBase, ObservableSingletonWithDocumentBasedSingletonTest ----

mod singleton {
    use super::*;

    const TARGET: DocumentBasedSingleton<'static> = DocumentBasedSingleton::new("int");

    /// The value the original's integer factory adapts the element to.
    fn get(doc: &mut StandaloneDocument) -> Option<i32> {
        let doc = doc.mutable();
        let view = doc.view();
        let element = TARGET.get(view, view.root());
        assert_eq!(TARGET.has_value(view, view.root()), element.is_some());
        i32::from_attribute(view.attribute(element?, "v")?)
    }

    fn set(doc: &mut StandaloneDocument, value: &str) {
        let attributes: InitialAttributes = [(s("v"), s(value))].into();
        let mut doc = doc.mutable();
        let element = TARGET.set(&mut doc, &Root, &attributes).unwrap();
        assert_eq!(doc.view().attribute(element, "v"), Some(value));
    }

    fn clear(doc: &mut StandaloneDocument) {
        TARGET.clear(&mut doc.mutable(), &Root).unwrap();
    }

    #[test]
    fn empty_singleton_has_no_value() {
        assert_eq!(get(&mut document("")), None);
    }

    #[test]
    fn set_sets_value() {
        let mut doc = document("");
        set(&mut doc, "42");
        assert_eq!(get(&mut doc), Some(42));
    }

    #[test]
    fn set_replaces_value() {
        let mut doc = document("");
        set(&mut doc, "42");
        set(&mut doc, "43");
        assert_eq!(get(&mut doc), Some(43));
        assert_eq!(xml(&doc), "<int v=\"43\"/>");
    }

    #[test]
    fn clear_removes_value() {
        let mut doc = document("");
        set(&mut doc, "42");
        clear(&mut doc);
        assert_eq!(get(&mut doc), None);
    }

    #[test]
    fn set_after_clear_sets_value() {
        let mut doc = document("");
        set(&mut doc, "42");
        clear(&mut doc);
        set(&mut doc, "43");
        assert_eq!(get(&mut doc), Some(43));
    }

    #[test]
    fn clear_empty_does_nothing() {
        let mut doc = document("");
        clear(&mut doc);
        assert!(doc.ops().is_empty());
    }

    #[test]
    fn initial_state_is_readable() {
        assert_eq!(get(&mut document("<int v=\"42\"/>")), Some(42));
    }

    #[test]
    fn remote_changes_update_state() {
        // A value inserted before the present one becomes the value.
        assert_eq!(
            get(&mut document("<int v=\"43\"/><int v=\"42\"/>")),
            Some(43)
        );
    }

    #[test]
    fn addition_of_non_canonical_value_changes_nothing() {
        assert_eq!(
            get(&mut document("<int v=\"42\"/><int v=\"43\"/>")),
            Some(42)
        );
    }

    #[test]
    fn clear_removes_redundant_values() {
        let mut doc = document("<int v=\"42\"/><other/><int v=\"43\"/>");
        clear(&mut doc);
        assert_eq!(get(&mut doc), None);
        assert_eq!(xml(&doc), "<other/>");
    }

    #[test]
    fn set_clears_redundant_values() {
        let mut doc = document("<int v=\"42\"/><other><int v=\"45\"/></other><int v=\"43\"/>");
        set(&mut doc, "44");
        assert_eq!(get(&mut doc), Some(44));
        assert_eq!(xml(&doc), "<int v=\"44\"/><other/>");
    }
}
