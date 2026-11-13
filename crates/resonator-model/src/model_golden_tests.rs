//! Differential tests of the document, conversation and supplement models
//! against the original Java implementation.
//!
//! `testdata/model_golden.txt` is written by
//! `testdata/ModelGoldenGenerator.java`, which drives the reference classes
//! through scripted scenarios. Each test here runs the same script against
//! the port and expects the same records; the record format is described in
//! the generator.

use std::collections::BTreeMap;
use std::error::Error;

use crate::conversation::WaveBasedConversationView;
use crate::data::WaveletData;
use crate::doc::{DocError, MutableDocument};
use crate::docop::to_concise_string;
use crate::id::{IdGeneratorImpl, ParticipantId, WaveId, WaveletId, WaveletName};
use crate::operation::{BlipOperation, WaveletOperation, WaveletOperationKind};
use crate::version::HashedVersion;
use crate::wavelet::{OpBasedWavelet, WaveView};

mod conversation;
mod digest;
mod doc;
mod nindo;

const GOLDEN: &str = include_str!("../testdata/model_golden.txt");

pub(crate) const TIME: i64 = 1234567890123;

pub(crate) type StepResult = Result<(), Box<dyn Error>>;

pub(crate) fn alice() -> ParticipantId {
    ParticipantId::of("alice@example.com").unwrap()
}

pub(crate) fn bob() -> ParticipantId {
    ParticipantId::of("bob@example.com").unwrap()
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
        } => format!("doc {blip_id} {}", to_concise_string(op)),
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

/// A wave whose wavelets record their operations, with the records
/// produced so far (the generator's `Wave`).
pub(crate) struct Wave {
    pub convs: WaveBasedConversationView<IdGeneratorImpl>,
    lines: Vec<String>,
    unordered: bool,
}

impl Wave {
    pub fn new(author: ParticipantId) -> Self {
        let mut view = WaveView::new(WaveId::of("example.com", "w+wave").unwrap(), author, TIME);
        view.bump_versions = true;
        let ids = IdGeneratorImpl::new("example.com", || "seed".to_owned());
        Wave {
            convs: WaveBasedConversationView::new(view, ids),
            lines: Vec::new(),
            unordered: false,
        }
    }

    fn record(&mut self, fields: &[&str]) {
        let escaped: Vec<String> = fields.iter().map(|field| escape(field)).collect();
        self.lines.push(escaped.join("\t"));
    }

    pub fn q(&mut self, answer: impl ToString) {
        self.record(&["Q", &answer.to_string()]);
    }

    pub fn wavelet_id(id: &str) -> WaveletId {
        WaveletId::of("example.com", id).unwrap()
    }

    /// The wavelet with an id, created empty (without participants) at
    /// version zero if missing.
    pub fn wavelet(&mut self, id: &str) -> &mut OpBasedWavelet {
        let wavelet_id = Self::wavelet_id(id);
        let view = self.convs.wave_mut();
        if view.wavelet(&wavelet_id).is_none() {
            let name = WaveletName::of(view.wave_id().clone(), wavelet_id.clone());
            let data = WaveletData::empty(
                &name,
                view.viewer().clone(),
                HashedVersion::unsigned(0),
                view.timestamp(),
            );
            view.add_wavelet(data);
        }
        view.wavelet_mut(&wavelet_id).unwrap()
    }

    pub fn edit(
        &mut self,
        wavelet: &str,
        doc: &str,
        edit: impl FnOnce(&mut MutableDocument<'_>) -> Result<(), DocError>,
    ) -> StepResult {
        Ok(self.wavelet(wavelet).edit_document(doc, edit)?)
    }

    /// A step of which only the number of operations is compared.
    pub fn step_unordered(&mut self, label: &str, step: impl FnOnce(&mut Wave) -> StepResult) {
        self.unordered = true;
        self.step(label, step);
        self.unordered = false;
    }

    pub fn step(&mut self, label: &str, step: impl FnOnce(&mut Wave) -> StepResult) {
        self.record(&["S", label]);
        // The generator's clock: a second per step.
        let time = self.convs.wave().timestamp() - self.convs.wave().timestamp() % 1000
            + 1000
            + TIME % 1000;
        self.convs.wave_mut().set_timestamp(time);
        let result = step(self);
        let mut ops = Vec::new();
        for wavelet in self.convs.wave_mut().wavelets_mut() {
            let id = wavelet.data().wavelet_id().id().to_owned();
            for op in wavelet.take_ops() {
                ops.push((id.clone(), describe(&op)));
            }
        }
        if self.unordered {
            self.record(&["C", &ops.len().to_string()]);
            ops.clear();
        }
        for (id, op) in ops {
            self.record(&["O", &id, &op]);
        }
        if result.is_err() {
            self.record(&["E"]);
        }
        self.state();
    }

    fn state(&mut self) {
        let mut records = Vec::new();
        let wavelets: BTreeMap<String, &OpBasedWavelet> = self
            .convs
            .wave()
            .wavelets()
            .map(|w| (w.data().wavelet_id().id().to_owned(), w))
            .collect();
        for (id, wavelet) in wavelets {
            let data = wavelet.data();
            for doc in data.documents() {
                let xml = doc.content().to_xml_string();
                if !xml.is_empty() {
                    records.push(vec![
                        "D".to_owned(),
                        id.clone(),
                        doc.id().to_owned(),
                        doc.last_modified_version().to_string(),
                        xml,
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
    pub fn check(&self, scenario: &str) {
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
