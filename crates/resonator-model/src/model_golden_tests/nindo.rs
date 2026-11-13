//! Random non-invertible operations, converted to document operations by
//! the original and by [`Nindo::to_doc_op`]
//! (`testdata/NindoGoldenGenerator.java`).

use super::escape;
use crate::doc::{DocumentSink, Nindo, NindoBuilder, StandaloneDocument};
use crate::docop::{Attributes, normalize, to_concise_string};

const GOLDEN: &str = include_str!("../../testdata/nindo_golden.txt");

fn unescape(field: &str) -> String {
    let mut units: Vec<u16> = Vec::new();
    let mut rest = field;
    while let Some(c) = rest.chars().next() {
        if let Some(hex) = rest.strip_prefix("\\u") {
            units.push(u16::from_str_radix(&hex[..4], 16).unwrap());
            rest = &hex[4..];
        } else {
            units.push(c as u16);
            rest = &rest[1..];
        }
    }
    String::from_utf16(&units).unwrap()
}

fn component(b: &mut NindoBuilder, fields: &[String]) {
    let pairs = |fields: &[String]| -> Vec<(String, String)> {
        fields
            .chunks(2)
            .map(|pair| (pair[0].clone(), pair[1].clone()))
            .collect()
    };
    let attributes = |fields: &[String]| Attributes::from_pairs(pairs(fields)).unwrap();
    let value = |field: &str| field.strip_prefix('=').map(str::to_owned);
    match fields[0].as_str() {
        "K" => b.skip(fields[1].parse().unwrap()),
        "C" => b.characters(&fields[1]),
        "ES" => b.element_start(&fields[1], attributes(&fields[2..])),
        "EE" => b.element_end(),
        "DC" => b.delete_characters(fields[1].parse().unwrap()),
        "DS" => b.delete_element_start(),
        "DE" => b.delete_element_end(),
        "RA" => b.replace_attributes(attributes(&fields[1..])),
        "UA" => b.update_attributes(
            pairs(&fields[1..])
                .into_iter()
                .map(|(name, new_value)| (name, value(&new_value)))
                .collect(),
        ),
        "SA" => b.start_annotation(&fields[1], value(&fields[2]).as_deref()),
        "EA" => b.end_annotation(&fields[1]),
        other => panic!("unknown component {other}"),
    }
}

#[test]
fn random_nindos_convert_as_in_the_original() {
    let mut doc = StandaloneDocument::default();
    let mut builder = Nindo::builder();
    let mut run = String::new();
    let mut checked = 0;
    for (number, line) in GOLDEN.lines().enumerate() {
        let fields: Vec<String> = line.split('\t').map(unescape).collect();
        let context = format!("run {run}, line {}", number + 1);
        match fields[0].as_str() {
            "R" => {
                run = fields[1].clone();
                doc = StandaloneDocument::default();
            }
            "n" => component(&mut builder, &fields[1..]),
            "o" => {
                let nindo = std::mem::take(&mut builder).build();
                let op = nindo
                    .to_doc_op(doc.document())
                    .unwrap_or_else(|e| panic!("{context}: {e}"));
                let op = normalize(&op).unwrap();
                assert_eq!(
                    line.split('\t').nth(1).unwrap(),
                    escape(&to_concise_string(&op)),
                    "{context}"
                );
                doc.consume(op).unwrap_or_else(|e| panic!("{context}: {e}"));
                checked += 1;
            }
            "d" => assert_eq!(fields[1], doc.document().to_xml_string(), "{context}"),
            other => panic!("unknown record {other}"),
        }
    }
    assert_eq!(40 * 30, checked);
}
