//! Differential tests against the original Java implementation.
//!
//! `testdata/wave_golden.txt` was produced by
//! `testdata/WaveGoldenGenerator.java` running the Apache Wave code with the
//! Java protobuf runtime. The scenarios are replayed here with the same
//! random number sequence, so the operations are the same and ids, hashes,
//! protobuf bytes, transforms and wavelet state must match record by record.

use std::collections::HashMap;

use crate::data::WaveletData;
use crate::docop::testing::{Parameters, RandomProvider, RandomProviderImpl, generate_random_op};
use crate::docop::{BootstrapDocument, DocOp, DocOpComponent};
use crate::id::waveref::{encode_to_uri_path_segment, encode_to_uri_query_string};
use crate::id::*;
use crate::operation::*;
use crate::proto::federation::{ProtocolSignature, ProtocolSignedDelta};
use crate::proto::*;
use crate::version::{HashedVersion, HashedVersionFactory};

const GOLDEN: &str = include_str!("../testdata/wave_golden.txt");

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

/// The golden records, each a list of unescaped fields.
struct Golden {
    records: Vec<Vec<String>>,
    next: usize,
}

impl Golden {
    fn load() -> Self {
        let records = GOLDEN
            .lines()
            .map(|line| line.split('\t').map(unescape).collect())
            .collect();
        Golden { records, next: 0 }
    }

    /// The next record if it has the given tag.
    fn take(&mut self, tag: &str) -> Option<(usize, Vec<String>)> {
        let record = self.records.get(self.next)?;
        if record[0] != tag {
            return None;
        }
        self.next += 1;
        Some((self.next, record[1..].to_vec()))
    }

    /// Checks that the next record has the given tag and fields.
    fn expect(&mut self, tag: &str, fields: &[String]) {
        let (line, expected) = self
            .take(tag)
            .unwrap_or_else(|| panic!("expected {tag} at golden line {}", self.next + 1));
        assert_eq!(fields, &expected[..], "golden line {line} ({tag})");
    }
}

fn result(r: Result<Vec<String>, impl std::fmt::Debug>) -> Vec<String> {
    match r {
        Ok(mut fields) => {
            fields.insert(0, "OK".to_owned());
            fields
        }
        Err(_) => vec!["ERR".to_owned()],
    }
}

fn name_parts(n: &WaveletName) -> Vec<String> {
    vec![
        n.wave_id.domain().to_owned(),
        n.wave_id.id().to_owned(),
        n.wavelet_id.domain().to_owned(),
        n.wavelet_id.id().to_owned(),
    ]
}

/// Checks all consecutive records with `tag`: `check` maps the input fields
/// to the expected remaining fields; `inputs` is the number of input fields.
fn check_all(
    golden: &mut Golden,
    tag: &str,
    inputs: usize,
    check: impl Fn(&[String]) -> Vec<String>,
) -> usize {
    let mut count = 0;
    while let Some((line, fields)) = golden.take(tag) {
        let actual = check(&fields[..inputs]);
        assert_eq!(
            actual,
            &fields[inputs..],
            "golden line {line} ({tag} {:?})",
            &fields[..inputs]
        );
        count += 1;
    }
    count
}

fn string_cases(golden: &mut Golden) {
    let escaper = &DEFAULT_ESCAPER;
    // Records for one input string come in a fixed order.
    while let Some((line, fields)) = golden.take("IDV") {
        let s = &fields[0];
        assert_eq!(is_valid_identifier(s).to_string(), fields[1], "line {line}");
        check_all(golden, "DOM", 2, |f| {
            vec![is_valid_domain(f[0].parse().unwrap(), &f[1]).to_string()]
        });
        golden.expect("ENC", &[s.clone(), IdUriCodec::encode(s)]);
        check_all(golden, "DEC", 1, |f| {
            result(IdUriCodec::decode(&f[0]).map(|d| vec![d]))
        });
        check_all(golden, "ESC", 1, |f| {
            let mut out = vec![escaper.escape(&f[0])];
            out.extend(result(escaper.unescape(&f[0]).map(|u| vec![u])));
            out
        });
        golden.expect(
            "ESP",
            &[
                s.clone(),
                escaper.has_escape_characters(s).to_string(),
                escaper.is_escaped_properly('+', s).to_string(),
            ],
        );
        check_all(golden, "SPL", 1, |f| {
            result(escaper.split('+', &f[0]).map(|tokens| {
                let mut out = vec![tokens.len().to_string()];
                out.extend(tokens);
                out
            }))
        });
        golden.expect(
            "PTH",
            &[
                s.clone(),
                encode_to_uri_path_segment(s),
                encode_to_uri_query_string(s),
            ],
        );
        let ok = |is_ok: bool| if is_ok { "OK" } else { "ER" }.to_owned();
        let n = check_all(golden, "IDS", 2, |f| {
            assert_eq!(
                WaveId::of(&f[0], &f[1]).is_ok(),
                WaveletId::of(&f[0], &f[1]).is_ok()
            );
            assert_eq!(
                WaveId::of_legacy(&f[0], &f[1]).is_ok(),
                WaveletId::of_legacy(&f[0], &f[1]).is_ok()
            );
            vec![
                ok(WaveId::of(&f[0], &f[1]).is_ok()),
                ok(WaveletId::of_legacy(&f[0], &f[1]).is_ok()),
            ]
        });
        assert_eq!(n, 3);
    }
}

fn id_cases(golden: &mut Golden) {
    let pair = |domain: &str, id: &str| vec![domain.to_owned(), id.to_owned()];
    while golden.records[golden.next][0] == "WMOD" {
        check_all(golden, "WMOD", 1, |f| {
            let wave = ModernIdSerialiser.deserialise_wave_id(&f[0]);
            let wavelet = ModernIdSerialiser.deserialise_wavelet_id(&f[0]);
            assert_eq!(wave.is_ok(), wavelet.is_ok());
            if let Ok(id) = &wave {
                assert_eq!(ModernIdSerialiser.serialise_wave_id(id).unwrap(), f[0]);
            }
            result(wave.map(|id| pair(id.domain(), id.id())))
        });
        check_all(golden, "WLEG", 1, |f| {
            result(
                LegacyIdSerialiser
                    .deserialise_wave_id(&f[0])
                    .map(|id| pair(id.domain(), id.id())),
            )
        });
        check_all(golden, "WDUAL", 1, |f| {
            result(WaveId::deserialise(&f[0]).map(|id| pair(id.domain(), id.id())))
        });
        check_all(golden, "WLID", 1, |f| {
            result(
                WaveletId::deserialise(&f[0])
                    .map(|id| vec![id.domain().to_owned(), id.id().to_owned(), id.serialise()]),
            )
        });
        check_all(golden, "NAME", 1, |f| {
            result(ModernIdSerialiser.deserialise_wavelet_name(&f[0]).map(|n| {
                let mut out = name_parts(&n);
                out.push(ModernIdSerialiser.serialise_wavelet_name(&n).unwrap());
                out.push(IdUriCodec::wavelet_name_to_uri(&n));
                let zero = HashedVersionFactory::create_version_zero(&n);
                assert_eq!(zero.version(), 0);
                out.push(hex::encode(zero.history_hash()));
                out
            }))
        });
    }
    let n = check_all(golden, "URI", 1, |f| {
        result(IdUriCodec::uri_to_wavelet_name(&f[0]).map(|n| name_parts(&n)))
    });
    assert!(n > 10);
    while golden.records[golden.next][0] == "PID" {
        check_all(golden, "PID", 1, |f| {
            result(ParticipantId::of(&f[0]).map(|p| {
                vec![
                    p.address().to_owned(),
                    p.name().to_owned(),
                    p.domain().to_owned(),
                ]
            }))
        });
        check_all(golden, "PIN", 1, |f| {
            vec![
                ParticipantId::new_unchecked(&f[0]).address().to_owned(),
                ParticipantId::is_normalized_address(&f[0]).to_string(),
                ParticipantId::is_domain_address(&f[0]).to_string(),
            ]
        });
    }
    let n = check_all(golden, "PCMP", 2, |f| {
        let a = ParticipantId::new_unchecked(&f[0]);
        let b = ParticipantId::new_unchecked(&f[1]);
        vec![(a.cmp(&b) as i32).to_string()]
    });
    assert!(n > 100);
    while let Some((line, fields)) = golden.take("WREF") {
        let decoded = WaveRef::decode_from_path(&fields[0]);
        let actual = result(decoded.as_ref().map(|r| {
            let mut out = pair(r.wave_id().domain(), r.wave_id().id());
            if let Some(wavelet_id) = r.wavelet_id() {
                out.extend(pair(wavelet_id.domain(), wavelet_id.id()));
            }
            out.extend(r.document_id().map(str::to_owned));
            out
        }));
        assert_eq!(
            actual,
            &fields[1..],
            "golden line {line} (WREF {})",
            fields[0]
        );
        if let Ok(r) = decoded {
            golden.expect(
                "WREFE",
                &[
                    r.encode_to_uri_path_segment(),
                    r.encode_to_uri_query_string(),
                ],
            );
        }
    }
    let n = check_all(golden, "TOK", 1, |f| {
        vec![base64_encode_int(f[0].parse().unwrap())]
    });
    assert!(n > 10);
}

fn version_cases(golden: &mut Golden) {
    let mut versions = Vec::new();
    while let Some((line, fields)) = golden.take("HVS") {
        let version =
            HashedVersion::of(fields[0].parse().unwrap(), hex::decode(&fields[1]).unwrap());
        assert_eq!(version.to_string(), fields[2], "golden line {line}");
        assert_eq!(HashedVersion::from_string(&fields[2]).unwrap(), version);
        versions.push(version);
    }
    let n = check_all(golden, "HVC", 2, |f| {
        let a = &versions[f[0].parse::<usize>().unwrap()];
        let b = &versions[f[1].parse::<usize>().unwrap()];
        vec![(a.cmp(b) as i32).to_string()]
    });
    assert!(n > 100);
}

const NAMES: [[&str; 4]; 5] = [
    ["example.com", "w+abc", "example.com", "conv+root"],
    ["example.com", "w+abc", "other.org", "user+a@other.org"],
    ["a.b.c", "w+caf\u{E9}", "a.b.c", "conv+\u{4E2D}\u{6587}"],
    [
        "example.com",
        "prof+j\u{F6}rg@t\u{16B}dali\u{146}.lv",
        "example.com",
        "conv+x*y",
    ],
    ["x.org", "w+x\u{10000}y", "y.org", "conv+~~a"],
];
const POOL: [&str; 4] = [
    "a@example.com",
    "b@example.com",
    "c@other.org",
    "@example.com",
];
const DOCS: [&str; 5] = ["b+1", "b+2", "attach+x", "conversation", "tr+1"];

fn pid(address: &str) -> ParticipantId {
    ParticipantId::of(address).unwrap()
}

fn ctx(creator: &ParticipantId) -> WaveletOperationContext {
    WaveletOperationContext::new(creator.clone(), NO_TIMESTAMP, 1)
}

fn list(participants: &[ParticipantId]) -> String {
    participants.iter().map(|p| format!("{p},")).collect()
}

fn expect_dump(golden: &mut Golden, w: &WaveletData) {
    golden.expect(
        "W",
        &[
            w.version().to_string(),
            format!(
                "{}:{}",
                w.hashed_version().version(),
                hex::encode(w.hashed_version().history_hash())
            ),
            w.last_modified_time().to_string(),
            w.creator().to_string(),
            w.creation_time().to_string(),
            list(w.participants()),
        ],
    );
    for d in w.documents() {
        golden.expect(
            "WD",
            &[
                d.id().to_owned(),
                d.author().to_string(),
                list(d.contributors()),
                d.last_modified_time().to_string(),
                d.last_modified_version().to_string(),
                d.content().to_xml_string(),
            ],
        );
    }
}

fn expect_failing_delta(
    golden: &mut Golden,
    wavelet: &mut WaveletData,
    author: &ParticipantId,
    v: &HashedVersion,
    ops: Vec<WaveletOperation>,
) {
    let delta = TransformedWaveletDelta::clone_operations(
        author.clone(),
        HashedVersion::unsigned(v.version() + ops.len() as u64),
        2_000_000,
        &ops,
    )
    .unwrap();
    let outcome = match apply_wavelet_delta(&delta, wavelet) {
        Ok(()) => "OK",
        Err(WaveletOperationError::DeltaFailed { .. }) => "OperationException",
        Err(e) => panic!("unexpected error {e}"),
    };
    golden.expect("F", &[outcome.to_owned()]);
    expect_dump(golden, wavelet);
}

fn scenarios(golden: &mut Golden) {
    let p = Parameters::default();
    for seed in 0..12 {
        let mut r = RandomProviderImpl::of_seed(seed);
        let n = NAMES[seed as usize % NAMES.len()];
        let name = WaveletName::of_parts(n[0], n[1], n[2], n[3]).unwrap();
        let mut v = HashedVersionFactory::create_version_zero(&name);
        golden.expect("SCN", &[seed.to_string(), hex::encode(v.history_hash())]);
        let mut mirror: HashMap<&str, BootstrapDocument> = HashMap::new();
        let mut participants: Vec<&str> = Vec::new();
        let mut wavelet: Option<WaveletData> = None;
        for i in 0..6 {
            let author = pid(POOL[r.next_int(3)]);
            let nops = 1 + r.next_int(3);
            let mut ops = Vec::new();
            for _ in 0..nops {
                let kind = r.next_int(6);
                if kind == 0 {
                    let who = POOL[r.next_int(4)];
                    if let Some(index) = participants.iter().position(|p| *p == who) {
                        participants.remove(index);
                        ops.push(WaveletOperation::remove_participant(ctx(&author), pid(who)));
                    } else {
                        participants.push(who);
                        ops.push(WaveletOperation::add_participant(ctx(&author), pid(who)));
                    }
                } else if kind == 1 {
                    ops.push(WaveletOperation::no_op(ctx(&author)));
                } else {
                    let doc_id = DOCS[r.next_int(5)];
                    let doc = mirror.entry(doc_id).or_default();
                    let op = generate_random_op(&mut r, &p, doc);
                    doc.consume(&op).unwrap();
                    ops.push(WaveletOperation::blip_content(ctx(&author), doc_id, op));
                }
            }
            let ts = 1_000_000 + i * 1000;
            let delta = WaveletDelta::new(author.clone(), v.clone(), ops);
            let delta_message = serialize_delta(&delta).unwrap();
            let delta_bytes = encode(&delta_message).unwrap();
            golden.expect("D", &[hex::encode(&delta_bytes)]);
            // The delta survives a round trip through its bytes.
            let parsed = deserialize_delta(&decode(&delta_bytes).unwrap()).unwrap();
            assert_eq!(parsed, delta);
            assert!(DeltaPair::are_same(&parsed, &delta));

            let mut signed = ProtocolSignedDelta {
                delta: Some(delta_bytes),
                signature: Vec::new(),
            };
            if i % 3 == 2 {
                signed.signature.push(ProtocolSignature {
                    signature_bytes: Some(vec![1, 2, 3]),
                    signer_id: Some(vec![9, 8]),
                    signature_algorithm: Some(1),
                });
            }
            let applied = build_applied_delta(signed, &v, nops as u32, ts).unwrap();
            golden.expect("A", &[hex::encode(applied.bytes())]);
            let reparsed = ByteStringMessage::parse_from(applied.bytes()).unwrap();
            assert_eq!(reparsed, applied);
            assert_eq!(reparsed.message(), applied.message());
            assert_eq!(get_hashed_version_applied_at(&applied).unwrap(), v);
            let v2 = calculate_resulting_hashed_version(&applied).unwrap();
            assert_eq!(
                v2,
                HashedVersionFactory::create(applied.bytes(), &v, nops as u32)
            );
            golden.expect(
                "V",
                &[v2.version().to_string(), hex::encode(v2.history_hash())],
            );

            let td = build_transformed_delta(&applied, &delta).unwrap();
            assert_eq!(td.resulting_version(), &v2);
            let stored = encode(&serialize_stored_delta(&td).unwrap()).unwrap();
            golden.expect("T", &[hex::encode(&stored)]);
            assert_eq!(
                deserialize_stored_delta(&decode(&stored).unwrap()).unwrap(),
                td
            );
            let update = serialize_transformed_delta(&td).unwrap();
            golden.expect("U", &[hex::encode(encode(&update).unwrap())]);
            assert_eq!(
                deserialize_transformed_delta(&update, v2.clone(), ts).unwrap(),
                td
            );

            let wavelet = wavelet.get_or_insert_with(|| {
                WaveletData::empty(&name, author.clone(), HashedVersion::unsigned(0), ts)
            });
            apply_wavelet_delta(&td, wavelet).unwrap();
            expect_dump(golden, wavelet);
            let snapshot = serialize_wavelet(wavelet, &v2).unwrap();
            let snapshot_bytes = encode(&snapshot).unwrap();
            golden.expect("S", &[hex::encode(&snapshot_bytes)]);
            let restored =
                deserialize_wavelet(&decode(&snapshot_bytes).unwrap(), wavelet.wave_id()).unwrap();
            assert_eq!(&restored, wavelet);
            v = v2;
        }

        // Deltas whose last operation fails, to exercise the rollback.
        let wavelet = wavelet.as_mut().unwrap();
        let author = pid(POOL[0]);
        let nobody = pid("nobody@nowhere.com");
        let empty = BootstrapDocument::new();
        let op = generate_random_op(&mut r, &p, mirror.get(DOCS[0]).unwrap_or(&empty));
        expect_failing_delta(
            golden,
            wavelet,
            &author,
            &v,
            vec![
                WaveletOperation::blip_content(ctx(&author), DOCS[0], op),
                WaveletOperation::no_op(ctx(&author)),
                WaveletOperation::remove_participant(ctx(&author), nobody.clone()),
            ],
        );
        let op = generate_random_op(&mut r, &p, &empty);
        expect_failing_delta(
            golden,
            wavelet,
            &author,
            &v,
            vec![
                WaveletOperation::blip_content(ctx(&author), "b+new", op),
                WaveletOperation::add_participant(ctx(&author), nobody.clone()),
                WaveletOperation::remove_participant(ctx(&author), nobody.clone()),
                WaveletOperation::remove_participant(ctx(&author), nobody.clone()),
            ],
        );
        let retain = DocOp::new(vec![DocOpComponent::Retain(5)]).unwrap();
        expect_failing_delta(
            golden,
            wavelet,
            &author,
            &v,
            vec![WaveletOperation::blip_content(
                ctx(&author),
                "b+bad",
                retain,
            )],
        );
    }
}

fn describe(op: &WaveletOperation) -> String {
    match op.kind() {
        WaveletOperationKind::VersionUpdate { .. } => format!("VU:{op}"),
        _ => hex::encode(encode(&serialize_op(op).unwrap()).unwrap()),
    }
}

fn describe_all(ops: &[WaveletOperation]) -> String {
    ops.iter().map(|op| describe(op) + ",").collect()
}

fn op_pool(
    who: &ParticipantId,
    r: &mut RandomProviderImpl,
    p: &Parameters,
    base: &BootstrapDocument,
) -> Vec<WaveletOperation> {
    let mut ops = vec![
        WaveletOperation::add_participant(ctx(who), pid("x@example.com")),
        WaveletOperation::add_participant(ctx(who), pid("a@example.com")),
        WaveletOperation::remove_participant(ctx(who), pid("a@example.com")),
        WaveletOperation::remove_participant(ctx(who), pid("b@example.com")),
        WaveletOperation::remove_participant(ctx(who), pid("x@example.com")),
        WaveletOperation::no_op(ctx(who)),
    ];
    for doc_id in ["b+1", "b+1", "b+2"] {
        let op = generate_random_op(r, p, base);
        ops.push(WaveletOperation::blip_content(ctx(who), doc_id, op));
    }
    ops
}

fn transforms(golden: &mut Golden) {
    let p = Parameters::default();
    let a = pid("a@example.com");
    let b = pid("b@example.com");
    for seed in 100..106 {
        let mut r = RandomProviderImpl::of_seed(seed);
        let mut base = BootstrapDocument::new();
        for _ in 0..3 {
            let op = generate_random_op(&mut r, &p, &base);
            base.consume(&op).unwrap();
        }
        let client = op_pool(&a, &mut r, &p, &base);
        let server = op_pool(&b, &mut r, &p, &base);
        golden.expect("XS", &[seed.to_string()]);
        for (i, c) in client.iter().enumerate() {
            for (j, s) in server.iter().enumerate() {
                let mut fields = vec![i.to_string(), j.to_string()];
                match transform(c, s) {
                    Ok(pair) => {
                        assert_eq!(pair.client_op.context(), c.context());
                        assert_eq!(pair.server_op.context(), s.context());
                        fields.push("OK".to_owned());
                        fields.push(describe(&pair.client_op));
                        fields.push(describe(&pair.server_op));
                    }
                    Err(WaveTransformError::RemovedAuthor(_)) => fields.push("RA".to_owned()),
                    Err(_) => fields.push("TE".to_owned()),
                }
                golden.expect("X", &fields);
            }
        }

        let mut cdoc = base.clone();
        let mut sdoc = base.clone();
        let mut cd = Vec::new();
        let mut sd = Vec::new();
        for _ in 0..2 {
            let c = generate_random_op(&mut r, &p, &cdoc);
            cdoc.consume(&c).unwrap();
            cd.push(WaveletOperation::blip_content(ctx(&a), "b+1", c));
            let s = generate_random_op(&mut r, &p, &sdoc);
            sdoc.consume(&s).unwrap();
            sd.push(WaveletOperation::blip_content(ctx(&b), "b+1", s));
        }
        cd.push(client[0].clone());
        sd.push(server[0].clone());
        sd.push(server[5].clone());
        let pair = DeltaPair::new(cd.clone(), sd).transform().unwrap();
        golden.expect(
            "DP",
            &[describe_all(&pair.client), describe_all(&pair.server)],
        );
        let same = cd
            .iter()
            .map(|op| {
                op.clone_op(WaveletOperationContext::new(a.clone(), 5, 1))
                    .unwrap()
            })
            .collect();
        let pair = DeltaPair::new(cd, same).transform().unwrap();
        golden.expect(
            "DP",
            &[describe_all(&pair.client), describe_all(&pair.server)],
        );
    }
}

#[test]
fn matches_original_implementation() {
    let mut golden = Golden::load();
    string_cases(&mut golden);
    id_cases(&mut golden);
    version_cases(&mut golden);
    scenarios(&mut golden);
    transforms(&mut golden);
    assert_eq!(
        golden.next,
        golden.records.len(),
        "unchecked golden records"
    );
    assert_eq!(golden.records.len(), 2893);
}
