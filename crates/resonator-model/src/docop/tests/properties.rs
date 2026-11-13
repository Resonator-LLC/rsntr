//! Randomized property tests, ported from `DomainRandomTester` /
//! `DocumentDomainLargeTest` and `OpComparatorsLargeTest.testRandomDocOps`.
//!
//! Every run is seeded; a failure message names the seed and iteration.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::docop::testing::{Parameters, RandomProvider, RandomProviderImpl, generate_random_op};
use crate::docop::*;

const NUM_ITERATIONS: u64 = 50;
const INITIAL_MUTATION_COUNT: usize = 3;
const FEATURE_ITERATION_COUNT: usize = 20;

struct SeededRandom(StdRng);

impl RandomProvider for SeededRandom {
    fn next_int(&mut self, upper_bound: usize) -> usize {
        self.0.random_range(0..upper_bound)
    }
}

/// The two generator configurations every property is checked with: the
/// original's, and one that also inserts characters outside the BMP.
fn parameter_sets() -> [(&'static str, Parameters); 2] {
    [
        ("default", Parameters::default()),
        (
            "supplementary",
            Parameters {
                supplementary_characters: true,
                ..Parameters::default()
            },
        ),
    ]
}

struct Run {
    context: String,
    r: SeededRandom,
    p: Parameters,
}

impl Run {
    fn random_op(&mut self, doc: &BootstrapDocument) -> DocOp {
        generate_random_op(&mut self.r, &self.p, doc)
    }

    /// A document after a few random mutations.
    fn initial_document(&mut self) -> BootstrapDocument {
        let mut doc = BootstrapDocument::new();
        for _ in 0..INITIAL_MUTATION_COUNT {
            let op = self.random_op(&doc);
            self.apply(&op, &mut doc);
        }
        doc
    }

    fn apply(&self, op: &DocOp, doc: &mut BootstrapDocument) {
        if let Err(e) = doc.consume(op) {
            panic!("{}: {e}\nop: {op}\ndoc: {doc}", self.context);
        }
    }

    fn assert_equivalent(&self, what: &str, a: &BootstrapDocument, b: &BootstrapDocument) {
        assert_eq!(
            a.to_xml_string(),
            b.to_xml_string(),
            "{}: {what}",
            self.context
        );
        assert_eq!(a, b, "{}: {what}", self.context);
    }
}

/// Runs `body` once per parameter set and seed.
fn for_each_run(mut body: impl FnMut(&mut Run)) {
    for (name, p) in parameter_sets() {
        for seed in 0..NUM_ITERATIONS {
            let mut run = Run {
                context: format!("parameters={name} seed={seed}"),
                r: SeededRandom(StdRng::seed_from_u64(seed)),
                p: p.clone(),
            };
            body(&mut run);
        }
    }
}

fn copy(doc: &BootstrapDocument) -> BootstrapDocument {
    let mut copy = BootstrapDocument::new();
    copy.consume(&doc.as_operation()).unwrap();
    copy
}

#[test]
fn data_operation_equivalence() {
    for_each_run(|run| {
        let mut d1 = BootstrapDocument::new();
        for _ in 0..INITIAL_MUTATION_COUNT {
            let op = run.random_op(&d1);
            run.apply(&op, &mut d1);
            let d2 = copy(&d1);
            run.assert_equivalent("document differs from its own initialization", &d1, &d2);
        }
    });
}

#[test]
fn operation_inversion() {
    for_each_run(|run| {
        let mut d1 = run.initial_document();
        for _ in 0..FEATURE_ITERATION_COUNT {
            let backup = d1.clone();
            let op = run.random_op(&d1);
            run.apply(&op, &mut d1);
            let reverse = invert(&op).unwrap();
            run.apply(&reverse, &mut d1);
            run.assert_equivalent("inverse does not restore the document", &d1, &backup);
            assert_eq!(op, invert(&reverse).unwrap(), "{}", run.context);
        }
    });
}

fn simple_composition(empty_initial_state: bool) {
    for_each_run(|run| {
        let mut d1 = if empty_initial_state {
            BootstrapDocument::new()
        } else {
            run.initial_document()
        };
        let mut d2 = d1.clone();
        for _ in 0..FEATURE_ITERATION_COUNT {
            let op1 = run.random_op(&d1);
            run.apply(&op1, &mut d1);
            let op2 = run.random_op(&d1);
            run.apply(&op2, &mut d1);
            let composed = compose(&op1, &op2).unwrap();
            run.apply(&composed, &mut d2);
            run.assert_equivalent("composed op differs from sequential application", &d1, &d2);
        }
    });
}

#[test]
fn composition_on_initial_state() {
    simple_composition(true);
}

#[test]
fn composition_after_mutations() {
    simple_composition(false);
}

#[test]
fn composition_associativity() {
    for_each_run(|run| {
        let mut d1 = run.initial_document();
        let mut d2 = d1.clone();
        let mut d3 = d1.clone();
        for _ in 0..FEATURE_ITERATION_COUNT {
            let op1 = run.random_op(&d1);
            run.apply(&op1, &mut d1);
            let op2 = run.random_op(&d1);
            run.apply(&op2, &mut d1);
            let op3 = run.random_op(&d1);
            run.apply(&op3, &mut d1);
            let op12 = compose(&op1, &op2).unwrap();
            let op23 = compose(&op2, &op3).unwrap();
            run.apply(&op1, &mut d2);
            run.apply(&op23, &mut d2);
            run.apply(&op12, &mut d3);
            run.apply(&op3, &mut d3);
            run.assert_equivalent("op1;(op2;op3) differs from (op1;op2);op3", &d2, &d3);
            run.assert_equivalent("composition differs from sequential application", &d1, &d2);
            // Note that compose(op12, op3) and compose(op1, op23) are not
            // always syntactically equal: they can differ in annotation
            // changes that have no effect (also true of the original).
            let all = compose_all([&op1, &op2, &op3]).unwrap().unwrap();
            let mut d4 = d3.clone();
            // Rewind d4 to the state before the three ops and replay.
            run.apply(&invert(&all).unwrap(), &mut d4);
            run.apply(&all, &mut d4);
            run.assert_equivalent("compose_all differs from sequential application", &d1, &d4);
        }
    });
}

#[test]
fn transform_diamond_property() {
    for_each_run(|run| {
        let mut d1 = run.initial_document();
        let mut d2 = d1.clone();
        for _ in 0..FEATURE_ITERATION_COUNT {
            let original = d1.clone();
            let op1 = run.random_op(&original);
            let op2 = run.random_op(&original);
            run.apply(&op1, &mut d1);
            run.apply(&op2, &mut d2);
            let pair = transform(&op1, &op2)
                .unwrap_or_else(|e| panic!("{}: {e}\nclient: {op1}\nserver: {op2}", run.context));
            run.apply(&pair.server_op, &mut d1);
            run.apply(&pair.client_op, &mut d2);
            run.assert_equivalent("client and server diverge", &d1, &d2);
            // compose(c, s') and compose(s, c') have the same effect on the
            // original document.
            let mut d3 = original.clone();
            run.apply(&compose(&op1, &pair.server_op).unwrap(), &mut d3);
            let mut d4 = original.clone();
            run.apply(&compose(&op2, &pair.client_op).unwrap(), &mut d4);
            run.assert_equivalent("compose(c, s') differs from sequential", &d1, &d3);
            run.assert_equivalent("compose(s, c') differs from sequential", &d1, &d4);
        }
    });
}

// OpComparatorsLargeTest.testRandomDocOps. As the original notes, nothing
// guarantees that neighbouring seeds give different ops; it happens to hold
// for these seeds, here as in Java.
#[test]
fn random_doc_ops_equality() {
    let p = Parameters::default();
    for i in 0..200 {
        let mut doc = BootstrapDocument::new();
        for j in 0..20 {
            let mut ra = RandomProviderImpl::of_seed(i * 20 + j);
            let mut rb = RandomProviderImpl::of_seed(i * 20 + j + 1);
            let a = generate_random_op(&mut ra, &p, &doc);
            let b = generate_random_op(&mut rb, &p, &doc);
            doc.consume(&a).unwrap();
            assert_eq!(a, a.clone());
            assert_ne!(a, b, "i={i} j={j}");
        }
    }
}

#[test]
fn random_ops_are_normalized_and_have_consistent_lengths() {
    for_each_run(|run| {
        let mut doc = run.initial_document();
        for _ in 0..FEATURE_ITERATION_COUNT {
            let op = run.random_op(&doc);
            assert_eq!(
                doc.length(),
                initial_document_length(&op),
                "{}",
                run.context
            );
            assert_eq!(
                ValidationResult::Valid,
                validate(None, &NO_SCHEMA_CONSTRAINTS, &doc, &op),
                "{}",
                run.context
            );
            let (insertion, noninsertion) = decompose(&op).unwrap();
            let mut decomposed = doc.clone();
            run.apply(&insertion, &mut decomposed);
            run.apply(&noninsertion, &mut decomposed);
            run.apply(&op, &mut doc);
            assert_eq!(
                doc.length(),
                resulting_document_length(&op),
                "{}",
                run.context
            );
            run.assert_equivalent("decomposition differs from the op", &doc, &decomposed);
        }
    });
}

#[test]
fn supplementary_parameter_set_generates_surrogate_pairs() {
    let (_, p) = &parameter_sets()[1];
    let mut r = SeededRandom(StdRng::seed_from_u64(1));
    let mut doc = BootstrapDocument::new();
    let mut seen = false;
    for _ in 0..20 {
        let op = generate_random_op(&mut r, p, &doc);
        seen |= op.iter().any(
            |c| matches!(c, DocOpComponent::Characters(s) if s.chars().any(|c| c.len_utf16() == 2)),
        );
        doc.consume(&op).unwrap();
    }
    assert!(seen);
}

/// Returns a copy of `op` with one random structural or numeric defect.
fn mutate(r: &mut SeededRandom, op: &DocOp, donor: &DocOp) -> DocOp {
    let mut components = op.components().to_vec();
    let pick = |r: &mut SeededRandom, len: usize| r.next_int(len.max(1));
    let extreme = [0, 1, 2, 7, usize::MAX / 2, usize::MAX];
    match r.next_int(8) {
        0 if !components.is_empty() => {
            components.remove(pick(r, components.len()));
        }
        1 if !components.is_empty() => {
            let i = pick(r, components.len());
            components.insert(i, components[i].clone());
        }
        2 if components.len() > 1 => {
            let (i, j) = (pick(r, components.len()), pick(r, components.len()));
            components.swap(i, j);
        }
        3 => components.truncate(pick(r, components.len())),
        4 if !donor.is_empty() => {
            let component = donor.components()[pick(r, donor.len())].clone();
            components.insert(pick(r, components.len() + 1), component);
        }
        5 => {
            let count = extreme[r.next_int(extreme.len())];
            components.insert(pick(r, components.len() + 1), DocOpComponent::Retain(count));
        }
        6 => {
            let text = ["", "x", "😀", "a😀b", "\u{FFFE}"][r.next_int(5)].to_owned();
            let component = if r.next_boolean() {
                DocOpComponent::Characters(text)
            } else {
                DocOpComponent::DeleteCharacters(text)
            };
            components.insert(pick(r, components.len() + 1), component);
        }
        _ => {
            for component in &mut components {
                if let DocOpComponent::Retain(count) = component {
                    *count = extreme[r.next_int(extreme.len())];
                    break;
                }
            }
        }
    }
    DocOp::new_unchecked(components)
}

// Malformed and mismatched operations must produce errors, never panics.
#[test]
fn malformed_input_never_panics() {
    let mut errors = 0;
    let mut successes = 0;
    for_each_run(|run| {
        let doc = run.initial_document();
        for _ in 0..FEATURE_ITERATION_COUNT {
            let a = run.random_op(&doc);
            let b = run.random_op(&doc);
            let bad_a = mutate(&mut run.r, &a, &b);
            let bad_b = mutate(&mut run.r, &b, &a);
            let mut after_a = doc.clone();
            after_a.consume(&a).unwrap();
            let c = run.random_op(&after_a);
            let bad_c = mutate(&mut run.r, &c, &a);

            let mut count = |ok: bool| {
                if ok {
                    successes += 1;
                } else {
                    errors += 1;
                }
            };
            for op in [&bad_a, &bad_b, &bad_c] {
                let mut v = ViolationCollector::new();
                count(is_well_formed(Some(&mut v), op));
                let _ = v.to_string();
                count(validate(Some(&mut v), &NO_SCHEMA_CONSTRAINTS, &doc, op).is_valid());
                count(DocOp::new(op.components().to_vec()).is_ok());
                count(invert(op).is_ok());
                count(normalize(op).is_ok());
                count(decompose(op).is_ok());
                count(doc.clone().consume(op).is_ok());
                count(
                    as_initialization(op.clone())
                        .and_then(|init| to_xml_string(&init))
                        .is_ok(),
                );
                let _ = op.to_string();
                let _ = initial_document_length(op);
                let _ = resulting_document_length(op);
            }
            for (first, second) in [
                (&a, &bad_c),
                (&bad_a, &c),
                (&bad_a, &bad_c),
                (&bad_a, &bad_b),
            ] {
                count(compose(first, second).is_ok());
                count(compose_unchecked(first, second).is_ok());
            }
            for (client, server) in [
                (&a, &bad_b),
                (&bad_a, &b),
                (&bad_a, &bad_b),
                (&bad_a, &bad_c),
            ] {
                count(transform(client, server).is_ok());
            }
            count(compose_all([&a, &bad_c, &bad_b]).is_ok());
        }
    });
    // The mutations must actually exercise the error paths (and not only
    // those).
    assert!(errors > 1000, "errors: {errors}");
    assert!(successes > 1000, "successes: {successes}");
}
