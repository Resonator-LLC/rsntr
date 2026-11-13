//! Ports of `RangeNormalizerTest` and `AnnotationsNormalizerTest`.

use super::boundary;
use crate::docop::*;

fn finish(sink: impl OpSink) -> DocOp {
    DocOp::new(sink.finish()).unwrap()
}

fn range_normalizer() -> RangeNormalizer<Vec<DocOpComponent>> {
    RangeNormalizer::new(Vec::new())
}

#[test]
fn multiple_retain_normalization() {
    let mut normalizer = range_normalizer();
    normalizer.retain(1);
    normalizer.characters("a");
    normalizer.retain(1);
    normalizer.retain(1);
    normalizer.retain(1);
    normalizer.characters("b");
    normalizer.retain(1);
    let expected = DocOpBuilder::new()
        .retain(1)
        .characters("a")
        .retain(3)
        .characters("b")
        .retain(1)
        .build()
        .unwrap();
    assert_eq!(expected, finish(normalizer));
}

#[test]
fn empty_retain_normalization() {
    let mut normalizer = range_normalizer();
    normalizer.retain(1);
    normalizer.characters("a");
    normalizer.retain(0);
    normalizer.characters("b");
    normalizer.retain(1);
    let expected = DocOpBuilder::new()
        .retain(1)
        .characters("ab")
        .retain(1)
        .build()
        .unwrap();
    assert_eq!(expected, finish(normalizer));
}

#[test]
fn multiple_characters_normalization() {
    let mut normalizer = range_normalizer();
    normalizer.retain(1);
    normalizer.characters("a");
    normalizer.characters("b");
    normalizer.characters("c");
    normalizer.retain(1);
    let expected = DocOpBuilder::new()
        .retain(1)
        .characters("abc")
        .retain(1)
        .build()
        .unwrap();
    assert_eq!(expected, finish(normalizer));
}

#[test]
fn empty_characters_normalization() {
    let mut normalizer = range_normalizer();
    normalizer.retain(1);
    normalizer.characters("");
    normalizer.retain(1);
    let expected = DocOpBuilder::new().retain(2).build().unwrap();
    assert_eq!(expected, finish(normalizer));
}

#[test]
fn multiple_delete_characters_normalization() {
    let mut normalizer = range_normalizer();
    normalizer.retain(1);
    normalizer.delete_characters("a");
    normalizer.delete_characters("b");
    normalizer.delete_characters("c");
    normalizer.retain(1);
    let expected = DocOpBuilder::new()
        .retain(1)
        .delete_characters("abc")
        .retain(1)
        .build()
        .unwrap();
    assert_eq!(expected, finish(normalizer));
}

#[test]
fn empty_delete_characters_normalization() {
    let mut normalizer = range_normalizer();
    normalizer.retain(1);
    normalizer.delete_characters("");
    normalizer.retain(1);
    let expected = DocOpBuilder::new().retain(2).build().unwrap();
    assert_eq!(expected, finish(normalizer));
}

fn annotations1() -> AnnotationBoundaryMap {
    boundary(
        &[],
        &[("x", Some("m"), Some("a")), ("y", Some("n"), Some("b"))],
    )
}

fn annotations2() -> AnnotationBoundaryMap {
    boundary(&["y"], &[("x", Some("m"), Some("c"))])
}

fn annotations3() -> AnnotationBoundaryMap {
    boundary(&["x"], &[("y", Some("n"), Some("f"))])
}

fn annotations4() -> AnnotationBoundaryMap {
    boundary(&["y"], &[])
}

fn annotations12() -> AnnotationBoundaryMap {
    boundary(&[], &[("x", Some("m"), Some("c"))])
}

fn annotations23() -> AnnotationBoundaryMap {
    boundary(&["x"], &[("y", Some("n"), Some("f"))])
}

fn annotations123() -> AnnotationBoundaryMap {
    boundary(&[], &[("y", Some("n"), Some("f"))])
}

fn annotations_normalizer() -> AnnotationsNormalizer<Vec<DocOpComponent>> {
    AnnotationsNormalizer::new(Vec::new())
}

/// retain(1), then each boundary followed by retain(1).
fn expected_with_boundaries(maps: Vec<AnnotationBoundaryMap>) -> DocOp {
    let mut builder = DocOpBuilder::new().retain(1);
    for map in maps {
        builder = builder.annotation_boundary(map).retain(1);
    }
    builder.build().unwrap()
}

#[test]
fn annotation_normalization1() {
    let mut normalizer = annotations_normalizer();
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations1());
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations2());
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations3());
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations4());
    normalizer.retain(1);
    let expected = expected_with_boundaries(vec![
        annotations1(),
        annotations2(),
        annotations3(),
        annotations4(),
    ]);
    assert_eq!(expected, finish(normalizer));
}

#[test]
fn annotation_normalization2() {
    let mut normalizer = annotations_normalizer();
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations1());
    normalizer.annotation_boundary(annotations2());
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations3());
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations4());
    normalizer.retain(1);
    let expected = expected_with_boundaries(vec![annotations12(), annotations3(), annotations4()]);
    assert_eq!(expected, finish(normalizer));
}

#[test]
fn annotation_normalization3() {
    let mut normalizer = annotations_normalizer();
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations1());
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations2());
    normalizer.annotation_boundary(annotations3());
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations4());
    normalizer.retain(1);
    let expected = expected_with_boundaries(vec![annotations1(), annotations23(), annotations4()]);
    assert_eq!(expected, finish(normalizer));
}

#[test]
fn annotation_normalization4() {
    let mut normalizer = annotations_normalizer();
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations1());
    normalizer.annotation_boundary(annotations2());
    normalizer.annotation_boundary(annotations3());
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations4());
    normalizer.retain(1);
    let expected = expected_with_boundaries(vec![annotations123(), annotations4()]);
    assert_eq!(expected, finish(normalizer));
}

/// The annotation tests with an empty component between the second and third
/// boundary, which must not act as a separator.
fn empty_component_normalization(empty: DocOpComponent) {
    let mut normalizer = annotations_normalizer();
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations1());
    normalizer.annotation_boundary(annotations2());
    normalizer.push(empty);
    normalizer.annotation_boundary(annotations3());
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations4());
    normalizer.retain(1);
    let expected = expected_with_boundaries(vec![annotations123(), annotations4()]);
    assert_eq!(expected, finish(normalizer));
}

#[test]
fn annotations_empty_retain_normalization() {
    empty_component_normalization(DocOpComponent::Retain(0));
}

#[test]
fn annotations_empty_characters_normalization() {
    empty_component_normalization(DocOpComponent::Characters(String::new()));
}

#[test]
fn annotations_empty_delete_characters_normalization() {
    empty_component_normalization(DocOpComponent::DeleteCharacters(String::new()));
}

// Not in the original suite: the normalizer drops a change that repeats the
// change already in effect.
#[test]
fn redundant_annotation_change_is_dropped() {
    let mut normalizer = annotations_normalizer();
    normalizer.annotation_boundary(annotations12());
    normalizer.retain(1);
    normalizer.annotation_boundary(annotations12());
    normalizer.retain(1);
    normalizer.annotation_boundary(boundary(&["x"], &[]));
    let expected = DocOpBuilder::new()
        .annotation_boundary(annotations12())
        .retain(2)
        .annotation_boundary(boundary(&["x"], &[]))
        .build()
        .unwrap();
    let mut range = range_normalizer();
    for component in normalizer.finish() {
        range.push(component);
    }
    assert_eq!(expected, finish(range));
}

#[test]
fn normalize_combines_both_normalizers() {
    let op = DocOpBuilder::new()
        .retain(1)
        .retain(2)
        .annotation_boundary(annotations12())
        .characters("a")
        .annotation_boundary(annotations12())
        .characters("b")
        .annotation_boundary(boundary(&["x"], &[]))
        .retain(1)
        .build_unchecked();
    let expected = DocOpBuilder::new()
        .retain(3)
        .annotation_boundary(annotations12())
        .characters("ab")
        .annotation_boundary(boundary(&["x"], &[]))
        .retain(1)
        .build()
        .unwrap();
    assert_eq!(expected, normalize(&op).unwrap());
}
