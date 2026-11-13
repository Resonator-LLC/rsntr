//! Differential test against the original Java implementation.
//!
//! `testdata/docop_golden.txt` was produced by `testdata/GoldenGenerator.java`
//! running the Apache Wave code. The random operation generator here consumes
//! random numbers in the same order as the original, so replaying a scenario
//! with the same seed must reproduce the same operations, and compose,
//! transform, invert and document application must give the same results.

use crate::docop::testing::{Parameters, RandomProviderImpl, generate_random_op};
use crate::docop::*;

const GOLDEN: &str = include_str!("../../../testdata/docop_golden.txt");

struct Golden {
    lines: std::iter::Enumerate<std::str::Lines<'static>>,
    checked: usize,
}

impl Golden {
    /// Checks that the next record has the given tag and payload.
    fn expect(&mut self, tag: &str, actual: &str) {
        let (number, line) = self.lines.next().expect("golden file ended early");
        let (line_tag, payload) = line.split_once(' ').unwrap_or((line, ""));
        assert_eq!(line_tag, tag, "golden line {}", number + 1);
        assert_eq!(actual, payload, "golden line {} ({tag})", number + 1);
        self.checked += 1;
    }

    fn expect_op(&mut self, tag: &str, op: &DocOp) {
        self.expect(tag, &to_concise_string(op));
    }
}

#[test]
fn matches_original_implementation() {
    let mut golden = Golden {
        lines: GOLDEN.lines().enumerate(),
        checked: 0,
    };
    let p = Parameters::default();
    let scenarios = GOLDEN.lines().filter(|l| l.starts_with("SEED ")).count();
    let groups = GOLDEN.lines().filter(|l| l.starts_with("C ")).count();
    assert_eq!(scenarios, 30);
    assert_eq!(groups, 30 * 6);
    for seed in 0..scenarios {
        golden.expect("SEED", &seed.to_string());
        let mut r = RandomProviderImpl::of_seed(seed as i32);
        let mut doc = BootstrapDocument::new();
        let mut collector = DocOpCollector::new();
        for _ in 0..3 {
            let op = generate_random_op(&mut r, &p, &doc);
            golden.expect_op("G", &op);
            doc.consume(&op).unwrap();
            collector.add(op).unwrap();
        }
        golden.expect("D", &doc.to_xml_string());
        for _ in 0..groups / scenarios {
            let client = generate_random_op(&mut r, &p, &doc);
            let server = generate_random_op(&mut r, &p, &doc);
            golden.expect_op("C", &client);
            golden.expect_op("S", &server);
            let pair = transform(&client, &server).unwrap();
            golden.expect_op("TC", &pair.client_op);
            golden.expect_op("TS", &pair.server_op);
            golden.expect_op("CS", &compose(&client, &pair.server_op).unwrap());
            golden.expect_op("SC", &compose(&server, &pair.client_op).unwrap());
            golden.expect_op("IC", &invert(&client).unwrap());
            doc.consume(&client).unwrap();
            doc.consume(&pair.server_op).unwrap();
            collector.add(client).unwrap();
            collector.add(pair.server_op).unwrap();
            golden.expect("D", &doc.to_xml_string());
        }
        golden.expect_op("CA", &collector.compose_all().unwrap().unwrap());
    }
    assert!(golden.lines.next().is_none(), "golden file has extra lines");
    assert_eq!(golden.checked, GOLDEN.lines().count());
}
