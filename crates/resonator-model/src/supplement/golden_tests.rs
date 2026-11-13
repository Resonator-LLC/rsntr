//! Differential tests of the adt, supplement and schema modules against the
//! original Java implementation.
//!
//! `testdata/supplement_golden.txt` is written by
//! `testdata/SupplementGoldenGenerator.java`, which drives the reference
//! classes through scripted scenarios. Each test here runs the same script
//! against the port and expects the same records; the record format is
//! described in the generator. For the schema scenarios the golden records
//! are themselves the script: each holds the question and the answer.

mod adt;
mod schema;
mod supplement;

use std::collections::BTreeMap;
use std::error::Error;

use crate::data::WaveletData;
use crate::doc::{DocError, MutableDocument, XmlFragment};
use crate::docop::{
    ValidationResult, ViolationCollector, normalize, to_concise_string, validate_initialization,
};
use crate::id::{ParticipantId, WaveId, WaveletId, WaveletName};
use crate::operation::{BlipOperation, WaveletOperation, WaveletOperationKind};
use crate::schema::{SchemaCollection, SchemaProvider};
use crate::version::HashedVersion;
use crate::wavelet::{OpBasedWavelet, WaveView};

const GOLDEN: &str = include_str!("../../testdata/supplement_golden.txt");

const TIME: i64 = 1234567890123;
const ROOT: &str = "conv+root";
const OTHER: &str = "conv+other";
const UDW: &str = "user+alice@example.com";

type StepResult = Result<(), Box<dyn Error>>;

fn alice() -> ParticipantId {
    ParticipantId::of("alice@example.com").unwrap()
}

fn bob() -> ParticipantId {
    ParticipantId::of("bob@example.com").unwrap()
}

fn wid(id: &str) -> WaveletId {
    WaveletId::of("example.com", id).unwrap()
}

/// The escaping of the generator's `line()`.
fn escape(field: &str) -> String {
    let mut escaped = String::new();
    for unit in field.encode_utf16() {
        if (0x20..0x7F).contains(&unit) && unit != u16::from(b'\\') {
            escaped.push(unit as u8 as char);
        } else {
            escaped.push_str(&format!("\\u{unit:04X}"));
        }
    }
    escaped
}

fn unescape(field: &str) -> String {
    let mut units: Vec<u16> = Vec::new();
    let mut rest = field;
    while let Some(at) = rest.find("\\u") {
        units.extend(rest[..at].encode_utf16());
        units.push(u16::from_str_radix(&rest[at + 2..at + 6], 16).unwrap());
        rest = &rest[at + 6..];
    }
    units.extend(rest.encode_utf16());
    String::from_utf16(&units).unwrap()
}

fn describe(op: &WaveletOperation) -> String {
    match op.kind() {
        WaveletOperationKind::AddParticipant { participant, .. } => format!("add {participant}"),
        WaveletOperationKind::RemoveParticipant { participant } => {
            format!("remove {participant}")
        }
        WaveletOperationKind::NoOp => "noop".to_owned(),
        WaveletOperationKind::Blip {
            blip_id,
            op: BlipOperation::Content { op, .. },
        } => {
            // Normalized, as in the generator.
            let op = normalize(op).expect("a valid operation");
            format!("doc {blip_id} {}", to_concise_string(&op))
        }
        other => panic!("unexpected op {other:?}"),
    }
}

/// The lines the generator wrote for a scenario, without the `N` line.
fn golden_scenario(name: &str) -> Vec<&'static str> {
    let header = format!("N\t{name}");
    let lines: Vec<&str> = GOLDEN
        .lines()
        .skip_while(|line| *line != header)
        .skip(1)
        .take_while(|line| !line.starts_with("N\t"))
        .collect();
    assert!(!lines.is_empty(), "no golden scenario {name}");
    lines
}

/// Java's `String.valueOf` of a nullable value.
fn nullable<T: ToString>(value: Option<T>) -> String {
    value.map_or("null".to_owned(), |v| v.to_string())
}

/// Java's `toString` of a collection.
fn list<T: ToString>(items: impl IntoIterator<Item = T>) -> String {
    let items: Vec<String> = items.into_iter().map(|item| item.to_string()).collect();
    format!("[{}]", items.join(", "))
}

fn result_name(result: ValidationResult) -> &'static str {
    match result {
        ValidationResult::IllFormed => "ILL_FORMED",
        ValidationResult::InvalidDocument => "INVALID_DOCUMENT",
        ValidationResult::InvalidSchema => "INVALID_SCHEMA",
        ValidationResult::Valid => "VALID",
    }
}

/// A wave whose wavelets record their operations, with the records
/// produced so far (the generator's `Wave`).
struct Wave {
    view: WaveView,
    schemas: SchemaCollection,
    lines: Vec<String>,
}

impl Wave {
    fn new() -> Self {
        let mut view = WaveView::new(WaveId::of("example.com", "w+wave").unwrap(), alice(), TIME);
        view.bump_versions = true;
        Wave {
            view,
            schemas: SchemaCollection::standard(),
            lines: Vec::new(),
        }
    }

    fn record(&mut self, fields: &[&str]) {
        let escaped: Vec<String> = fields.iter().map(|field| escape(field)).collect();
        self.lines.push(escaped.join("\t"));
    }

    fn q(&mut self, answer: impl ToString) {
        self.record(&["Q", &answer.to_string()]);
    }

    /// The wavelet with an id, created empty (without participants) at
    /// version zero if missing.
    fn wavelet(&mut self, id: &str) -> &mut OpBasedWavelet {
        let wavelet_id = wid(id);
        if self.view.wavelet(&wavelet_id).is_none() {
            let name = WaveletName::of(self.view.wave_id().clone(), wavelet_id.clone());
            let data = WaveletData::empty(&name, alice(), HashedVersion::unsigned(0), TIME);
            self.view.add_wavelet(data);
        }
        self.view.wavelet_mut(&wavelet_id).unwrap()
    }

    fn edit<R>(
        &mut self,
        wavelet: &str,
        doc: &str,
        edit: impl FnOnce(&mut MutableDocument<'_>) -> Result<R, DocError>,
    ) -> Result<R, DocError> {
        self.wavelet(wavelet).edit_document(doc, edit)
    }

    /// Appends content to a document.
    fn append(&mut self, wavelet: &str, doc: &str, xml: &str) -> StepResult {
        let xml = XmlFragment::parse(xml)?;
        self.edit(wavelet, doc, |doc| {
            doc.insert_xml(doc.size(), &xml).map(drop)
        })?;
        Ok(())
    }

    fn step(&mut self, label: &str, step: impl FnOnce(&mut Wave) -> StepResult) {
        self.record(&["S", label]);
        let result = step(self);
        let mut ops = Vec::new();
        for wavelet in self.view.wavelets_mut() {
            let id = wavelet.data().wavelet_id().id().to_owned();
            for op in wavelet.take_ops() {
                ops.push((id.clone(), describe(&op)));
            }
        }
        // Stable: operations of one wavelet stay in emission order.
        ops.sort_by(|a, b| a.0.cmp(&b.0));
        if label.starts_with('~') {
            self.record(&["U", &ops.len().to_string()]);
        } else {
            for (id, op) in ops {
                self.record(&["O", &id, &op]);
            }
        }
        if result.is_err() {
            self.record(&["E"]);
        }
        self.state();
    }

    fn state(&mut self) {
        let mut records = Vec::new();
        let wavelets: BTreeMap<String, &OpBasedWavelet> = self
            .view
            .wavelets()
            .map(|w| (w.data().wavelet_id().id().to_owned(), w))
            .collect();
        for (id, wavelet) in wavelets {
            let data = wavelet.data();
            for doc in data.documents() {
                let xml = doc.content().to_xml_string();
                if xml.is_empty() {
                    continue;
                }
                records.push(vec![
                    "D".to_owned(),
                    id.clone(),
                    doc.id().to_owned(),
                    doc.last_modified_version().to_string(),
                    xml,
                ]);
                if let Some(schema) = self.schemas.schema_for_id(data.wavelet_id(), doc.id()) {
                    let mut violations = ViolationCollector::new();
                    let result = validate_initialization(
                        Some(&mut violations),
                        schema,
                        &doc.content().as_operation(),
                    );
                    records.push(vec![
                        "V".to_owned(),
                        id.clone(),
                        doc.id().to_owned(),
                        result_name(result).to_owned(),
                        nullable(violations.first_description()),
                    ]);
                }
            }
            let participants: Vec<&str> = data.participants().iter().map(|p| p.address()).collect();
            records.push(vec![
                "P".to_owned(),
                id,
                data.version().to_string(),
                participants.join(" "),
            ]);
        }
        for record in records {
            let fields: Vec<&str> = record.iter().map(String::as_str).collect();
            self.record(&fields);
        }
    }

    /// Compares the records produced with the golden ones of a scenario.
    fn check(&self, scenario: &str) {
        let golden = golden_scenario(scenario);
        let mut step = "";
        for (index, expected) in golden.iter().enumerate() {
            if expected.starts_with("S\t") {
                step = expected;
            }
            let actual = self.lines.get(index).map(String::as_str);
            assert_eq!(
                Some(*expected),
                actual,
                "scenario {scenario}, record {index}, in step {step:?}"
            );
        }
        assert_eq!(
            golden.len(),
            self.lines.len(),
            "scenario {scenario}: extra records, first: {:?}",
            self.lines.get(golden.len())
        );
    }
}
