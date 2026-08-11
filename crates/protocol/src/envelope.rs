//! The typed envelope layer: Rust structs for every `rsntr:` wire class,
//! decoded from and encoded to the one-Turtle-document-per-frame format.
//!
//! Framing rules this module enforces:
//!
//! - a frame normally carries exactly one typed subject;
//! - a row-batch frame may carry several `rsntr:Row` subjects;
//! - Projection, Vibration, and Graph frames carry companion triples
//!   beside their typed subject;
//! - unknown predicates on a known class are silently dropped (the
//!   vocabulary is open-world);
//! - an unknown `rsntr:` class becomes [`EnvelopeObject::Generic`] (v3
//!   passthrough) instead of an error;
//! - a non-`rsntr:` class on the sole typed subject is still a protocol
//!   error.
//!
//! Parsing rides on oxttl's push-based Turtle parser with the implied
//! prefix block pre-registered, so prefixes never travel on the wire.

use oxrdf::vocab::{rdf, xsd};
use oxrdf::{NamedNode, NamedOrBlankNode, Term, Triple};
use oxttl::TurtleParser;

use crate::error::ProtocolError;
use crate::value::{
    Value, decode_base64, encode_base64, format_double, parse_double, parse_integer,
};
use crate::vocab::{
    COL_PREFIX, IMPLIED_PREFIXES, RDFS_COMMENT, RDFS_LABEL, RSNTR_NS, XSD_BASE64_BINARY, XSD_NS,
    cls, prop,
};

/// The body shared by `rsntr:Query` and `rsntr:Execute`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// ULID request id; doubles as idempotency key and response correlator.
    pub id: String,
    /// Which modulation interprets the signal, e.g. `"sql-sqlite"`.
    pub modulation: String,
    /// The statement text exactly as the caller wrote it.
    pub signal: String,
    /// Positional parameters in order; empty means the frame carried no
    /// `rsntr:params` list at all.
    pub params: Vec<Value>,
    /// Multi-database selector, reserved; carried through untouched.
    pub database: Option<String>,
    /// Row cap requested by the client; the server may clamp it.
    pub row_limit: Option<i64>,
    /// Byte cap requested by the client; the server may clamp it.
    pub byte_limit: Option<i64>,
    /// Timeout requested by the client; the server may clamp it.
    pub timeout_ms: Option<i64>,
}

/// `rsntr:Result`, the header that opens a row-streaming response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultHeader {
    pub id: String,
    /// Column names in result order. These are the authoritative names;
    /// the per-cell predicates are derived from them.
    pub columns: Vec<String>,
    /// Declared types per column, engine-native and advisory only. Empty
    /// means the frame carried no `rsntr:declType` list.
    pub decl_types: Vec<String>,
}

/// One `rsntr:Row` subject out of a row-batch frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Position of this row in the result (`rsntr:seq`).
    pub seq: i64,
    /// (column, value) cells in document order. A NULL cell is expressed
    /// on the wire by omitting the column, so decoded rows never contain
    /// one, and `Value::Null` cells are dropped when writing.
    pub cells: Vec<(String, Value)>,
}

/// `rsntr:Done`, the trailer that closes a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub id: String,
    pub row_count: Option<i64>,
    pub affected_rows: Option<i64>,
    pub last_insert_rowid: Option<i64>,
    /// Set when a limit cut the response short.
    pub truncated: bool,
}

/// `rsntr:Denied`: the request was understood and refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied {
    pub id: Option<String>,
    pub reason: Option<String>,
}

/// `rsntr:Error`: the request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorEnvelope {
    pub id: Option<String>,
    /// One of the protocol codes; see [`crate::error::ErrorCode`].
    pub code: String,
    pub reason: Option<String>,
}

/// `rsntr:Hello`, the capability advertisement opening every connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    /// Minor envelope version inside the ALPN major (`rsntr:ver`).
    pub envelope_version: String,
    /// Frame encodings on offer (`rsntr:enc`); `"turtle"` is always one.
    pub encodings: Vec<String>,
    /// Modulations the node works (`rsntr:mods`): it both serves them and
    /// can issue them. A serving node always lists `"help"`.
    pub mods: Vec<String>,
    /// One plain-text line pointing a human or AI reader at help
    /// (`rsntr:hint`). Optional on the wire but SHOULD be emitted.
    pub hint: Option<String>,
}

/// `rsntr:Knock`: how a stranger introduces itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Knock {
    /// Client-chosen ULID tying together the knock, its `_inbox` row, and
    /// the eventual `rsntr:Decision`. When absent or not a ULID the server
    /// mints one.
    pub id: Option<String>,
    pub message: String,
}

/// `rsntr:Presence`, the gossip beacon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presence {
    /// `xsd:dateTime` lexical form, uninterpreted by the codec.
    pub at: String,
    pub status: Option<String>,
    /// Self-declared author endpoint id (64-hex ed25519 key). Receivers
    /// trust the gossip-proven delivering author and use this field only
    /// as a cross-check.
    pub endpoint: Option<String>,
}

/// `rsntr:Decision`: an authorization outcome expressed as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub id: Option<String>,
    /// `allow`, `allow-narrowed`, `deny`.
    pub decision: String,
    /// `policy`, `script`, `ai`, `human`, `cache`.
    pub decided_by: String,
    pub reason: Option<String>,
    /// Optional `xsd:dateTime` lexical form.
    pub at: Option<String>,
}

/// `rsntr:Help`: plain-text usage guidance for humans and AIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Help {
    /// Correlates with the help query; optional so a node MAY volunteer
    /// help unprompted.
    pub id: Option<String>,
    /// The guidance text verbatim, possibly multi-line.
    pub signal: String,
    /// Drill-down topic names (`rsntr:topic`) in document order.
    pub topics: Vec<String>,
}

/// `rsntr:Media`: the go-ahead answering a `media`-modulation query.
/// After this frame the stream stops being frames: everything that
/// follows is the raw byte feed in the named format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    /// Correlates with the media query.
    pub id: String,
    /// Media type of the raw bytes to come, e.g. `"video/mp2t"`.
    pub content_type: String,
}

/// `rsntr:AudioDuplex`: the go-ahead answering an `audio-duplex` query.
/// After this frame the stream stops being frames IN BOTH DIRECTIONS:
/// downstream is the source's byte feed (when `content_type` is present),
/// upstream is the caller's audio in the `accepts` format, until either
/// side closes its half. The `id` is load-bearing for web clients: it
/// names the upstream `/duplex/{id}` endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDuplex {
    /// Correlates with the audio-duplex query.
    pub id: String,
    /// Media type of the downstream bytes; `None` = the source emits
    /// nothing (a pure talk sink).
    pub content_type: Option<String>,
    /// Media type the source's stdin accepts, e.g.
    /// `"audio/L16;rate=8000;channels=1"`.
    pub accepts: String,
}

/// `rsntr:Graph` (v3): a correlation header plus an arbitrary payload
/// graph in the same frame. The sparql modulation returns CONSTRUCT and
/// DESCRIBE results as a sequence of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graph {
    /// Correlates with the originating request.
    pub id: String,
    /// Position of this chunk in the response, counted from 0.
    pub seq: i64,
    /// Every triple in the frame whose subject is not the Graph header,
    /// in document order, kept verbatim.
    pub payload: Vec<Triple>,
}

/// A frame whose `rsntr:` class this codec has never heard of (v3).
/// Decoded as data rather than rejected, so responses can pass through
/// unmodified and the vocabulary can grow.
///
/// `props` lists the subject's (predicate, object) pairs in document
/// order with `rdf:type` removed. Blank-node objects keep their syntax
/// but not their own properties; a mod that needs structure should
/// define a proper class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generic {
    /// Local name inside the `rsntr:` namespace, e.g. `"Refused"`.
    pub class: String,
    /// (predicate, object) pairs of the typed subject, document order.
    pub props: Vec<(NamedNode, Term)>,
}

// ---------------------------------------------------------------------------
// Projection vocabulary
// ---------------------------------------------------------------------------

/// What a resonance point affords.
///
/// Any class the codec does not recognize (rsntr or foreign) becomes
/// [`PointKind::Other`] carrying the full IRI; the open-world contract is
/// that clients render such points inert (label only) and never error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointKind {
    /// `rsntr:Excitable`: can be driven, has effects (an action).
    Excitable,
    /// `rsntr:Radiant`: emits, can be read (a property).
    Radiant,
    /// `rsntr:Sympathetic`: signals, can be entrained (an event).
    Sympathetic,
    /// A plain `rsntr:ResonancePoint`, or no type at all: navigation only.
    Bare,
    /// Some other class; the IRI is kept so the point round-trips.
    Other(String),
}

/// One named input of a point's coupling (flat SHACL-lite).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointField {
    /// Binding name (`rsntr:name`); paramsOrder and templates refer to it.
    pub name: String,
    /// Expected datatype as a full IRI (typically xsd). Advisory.
    pub datatype: Option<String>,
    /// Whether the caller must supply the field; wire default false.
    pub required: bool,
    /// Prefill value (`rsntr:default`).
    pub default: Option<Value>,
    /// Allowed values (`rsntr:oneOf`) in order; empty means unconstrained.
    pub one_of: Vec<Value>,
    /// One-line prompt (`rsntr:hint`).
    pub hint: Option<String>,
}

/// One resonance point of a projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    /// Stable IRI minted by the serving node.
    pub iri: String,
    pub kind: PointKind,
    /// Display text (`rdfs:label`).
    pub label: Option<String>,
    /// Longer description (`rdfs:comment`).
    pub comment: Option<String>,
    /// Rendering hint: theme icon name; clients MAY ignore it.
    pub icon: Option<String>,
    /// Rendering hint: `default` or `destructive`.
    pub role: Option<String>,
    /// Path of the deeper projection behind this point (opaque and
    /// discovered-only; by convention the point's own IRI).
    pub projects: Option<String>,
    /// Input contract in prompt order; empty means zero-argument.
    pub coupling: Vec<PointField>,
    /// Envelope binding: modulation of the request the point carries.
    pub modulation: Option<String>,
    /// Envelope binding: the carried statement, positional placeholders.
    pub signal: Option<String>,
    /// Field names in positional order for `signal`.
    pub params_order: Vec<String>,
    /// Fallback binding: a `{name}`-template statement.
    pub signal_template: Option<String>,
    /// Fire binding: URN template, for antenna-hosted nodes.
    pub fires: Option<String>,
}

/// `rsntr:Projection`: the capability surface a node shows the caller.
/// One Projection graph per frame; `offers` is presentation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    /// Correlates with the projection query.
    pub id: Option<String>,
    pub offers: Vec<Point>,
}

/// `rsntr:Entrain`: start resonating with a Sympathetic point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entrain {
    pub id: String,
    /// IRI of the Sympathetic point.
    pub point: String,
}

/// `rsntr:Damp`: end an entrainment early, in-band.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Damp {
    pub id: Option<String>,
    /// IRI of the Sympathetic point.
    pub point: String,
}

/// `rsntr:Vibration`: one signal out of an entrained point.
///
/// Only the correlation skeleton (id, point, seq, at) is standardized;
/// the rest of the frame is the node's own payload, carried verbatim as
/// extra triples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vibration {
    /// The entrainment this vibration answers.
    pub id: String,
    /// IRI of the Sympathetic point that fired.
    pub point: String,
    /// Delivery position within the entrainment, from 0; counts delivered
    /// vibrations only (bursts may coalesce).
    pub seq: i64,
    /// `xsd:dateTime` lexical form, kept as a string.
    pub at: Option<String>,
    /// Domain triples riding along in the frame, in document order,
    /// preserved across decode/encode.
    pub payload: Vec<Triple>,
}

/// One decoded frame, dispatched on `rdf:type`. `Row` holds a whole
/// batch (a row frame carries one or more `rsntr:Row` subjects); a
/// `Projection` frame brings its point subjects along; `Vibration` and
/// `Graph` frames may bring payload triples. All other frames hold
/// exactly one typed subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeObject {
    Query(Statement),
    Execute(Statement),
    Result(ResultHeader),
    Row(Vec<Row>),
    Done(Done),
    Denied(Denied),
    Error(ErrorEnvelope),
    Hello(Hello),
    Knock(Knock),
    Presence(Presence),
    Decision(Decision),
    Help(Help),
    Media(Media),
    AudioDuplex(AudioDuplex),
    Projection(Projection),
    Entrain(Entrain),
    Vibration(Vibration),
    Damp(Damp),
    /// v3: a frame carrying an arbitrary payload graph (sparql CONSTRUCT).
    Graph(Graph),
    /// v3: passthrough for an unrecognized `rsntr:` class.
    Generic(Generic),
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// Incremental envelope decoder: push the frame payload in any byte
/// chunking, then [`finish`](Self::finish) to get the object.
///
/// Backed by `oxttl::TurtleParser::low_level()` with the implied prefix
/// block already registered.
pub struct EnvelopeParser {
    inner: oxttl::turtle::LowLevelTurtleParser,
    seen: Vec<Triple>,
}

impl EnvelopeParser {
    /// A parser that already knows the implied prefixes.
    pub fn new() -> Self {
        let mut parser = TurtleParser::new();
        for (name, iri) in IMPLIED_PREFIXES {
            parser = parser
                .with_prefix(name, iri)
                .expect("implied prefix IRIs are static and valid");
        }
        Self {
            inner: parser.low_level(),
            seen: Vec::new(),
        }
    }

    /// Feeds the next chunk of the frame payload.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), ProtocolError> {
        self.inner.extend_from_slice(chunk);
        self.pump()
    }

    /// Marks end of input and assembles the envelope object.
    pub fn finish(mut self) -> Result<EnvelopeObject, ProtocolError> {
        self.inner.end();
        self.pump()?;
        decode(self.seen)
    }

    fn pump(&mut self) -> Result<(), ProtocolError> {
        while let Some(next) = self.inner.parse_next() {
            self.seen.push(next?);
        }
        Ok(())
    }
}

impl Default for EnvelopeParser {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvelopeObject {
    /// Decodes one complete Turtle document (one frame payload).
    pub fn from_turtle(doc: &str) -> Result<Self, ProtocolError> {
        let mut parser = EnvelopeParser::new();
        parser.push(doc.as_bytes())?;
        parser.finish()
    }

    /// Encodes to a Turtle document. The implied prefix block is never
    /// written; receivers register it out-of-band.
    pub fn to_turtle(&self) -> Result<String, ProtocolError> {
        encode(self)
    }
}

fn bad(msg: impl Into<String>) -> ProtocolError {
    ProtocolError::malformed(msg)
}

/// One frame's triples, document order intact, with typed accessors.
struct Doc {
    triples: Vec<Triple>,
}

impl Doc {
    /// (predicate, object) pairs of `at`, in document order.
    fn pairs<'a>(
        &'a self,
        at: &'a NamedOrBlankNode,
    ) -> impl Iterator<Item = (&'a NamedNode, &'a Term)> + 'a {
        self.triples
            .iter()
            .filter(move |t| &t.subject == at)
            .map(|t| (&t.predicate, &t.object))
    }

    /// First object of `pred` on `at`, if any.
    fn one<'a>(
        &'a self,
        at: &'a NamedOrBlankNode,
        pred: oxrdf::NamedNodeRef<'_>,
    ) -> Option<&'a Term> {
        self.pairs(at)
            .find_map(|(p, o)| (p.as_ref() == pred).then_some(o))
    }

    /// Optional literal property, as its lexical value.
    fn text(
        &self,
        at: &NamedOrBlankNode,
        pred: oxrdf::NamedNodeRef<'_>,
    ) -> Result<Option<String>, ProtocolError> {
        match self.one(at, pred) {
            None => Ok(None),
            Some(Term::Literal(lit)) => Ok(Some(lit.value().to_owned())),
            Some(other) => Err(bad(format!(
                "expected a literal object for <{pred}>, got {other}"
            ))),
        }
    }

    /// Mandatory literal property.
    fn need_text(
        &self,
        at: &NamedOrBlankNode,
        pred: oxrdf::NamedNodeRef<'_>,
    ) -> Result<String, ProtocolError> {
        self.text(at, pred)?
            .ok_or_else(|| bad(format!("missing required property <{pred}>")))
    }

    /// Optional integer property.
    fn int(
        &self,
        at: &NamedOrBlankNode,
        pred: oxrdf::NamedNodeRef<'_>,
    ) -> Result<Option<i64>, ProtocolError> {
        self.text(at, pred)?.map(|s| parse_integer(&s)).transpose()
    }

    /// Optional boolean property; accepts true/false and 1/0.
    fn boolean(
        &self,
        at: &NamedOrBlankNode,
        pred: oxrdf::NamedNodeRef<'_>,
    ) -> Result<Option<bool>, ProtocolError> {
        let Some(lex) = self.text(at, pred)? else {
            return Ok(None);
        };
        match lex.trim() {
            "true" | "1" => Ok(Some(true)),
            "false" | "0" => Ok(Some(false)),
            other => Err(bad(format!("invalid boolean literal {other:?}"))),
        }
    }

    /// Every literal value of a repeated predicate, document order.
    fn texts(
        &self,
        at: &NamedOrBlankNode,
        pred: oxrdf::NamedNodeRef<'_>,
    ) -> Result<Vec<String>, ProtocolError> {
        self.pairs(at)
            .filter(|(p, _)| p.as_ref() == pred)
            .map(|(_, o)| match o {
                Term::Literal(lit) => Ok(lit.value().to_owned()),
                other => Err(bad(format!(
                    "expected a literal object for <{pred}>, got {other}"
                ))),
            })
            .collect()
    }

    /// Optional IRI-valued property.
    fn iri(
        &self,
        at: &NamedOrBlankNode,
        pred: oxrdf::NamedNodeRef<'_>,
    ) -> Result<Option<String>, ProtocolError> {
        match self.one(at, pred) {
            None => Ok(None),
            Some(Term::NamedNode(n)) => Ok(Some(n.as_str().to_owned())),
            Some(other) => Err(bad(format!(
                "expected an IRI object for <{pred}>, got {other}"
            ))),
        }
    }

    /// Mandatory IRI-valued property.
    fn need_iri(
        &self,
        at: &NamedOrBlankNode,
        pred: oxrdf::NamedNodeRef<'_>,
    ) -> Result<String, ProtocolError> {
        self.iri(at, pred)?
            .ok_or_else(|| bad(format!("missing required IRI property <{pred}>")))
    }

    /// Walks an rdf:List from its head; `rdf:nil` is empty. The iteration
    /// bound exists only to stop malicious cycles; frames cap at 256 KiB
    /// so real lists sit far below it.
    fn list(&self, head: &Term) -> Result<Vec<Term>, ProtocolError> {
        let mut items = Vec::new();
        let mut here = head.clone();
        for _ in 0..1_000_000 {
            let node: NamedOrBlankNode = match &here {
                Term::NamedNode(n) if n.as_ref() == rdf::NIL => return Ok(items),
                Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n.clone()),
                Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b.clone()),
                other => return Err(bad(format!("invalid rdf:List node: {other}"))),
            };
            items.push(
                self.one(&node, rdf::FIRST)
                    .ok_or_else(|| bad("rdf:List node without rdf:first"))?
                    .clone(),
            );
            here = self
                .one(&node, rdf::REST)
                .ok_or_else(|| bad("rdf:List node without rdf:rest"))?
                .clone();
        }
        Err(bad("rdf:List too long or cyclic"))
    }

    /// An ordered rdf:List of plain strings (rsntr:column, rsntr:declType).
    fn text_list(
        &self,
        at: &NamedOrBlankNode,
        pred: oxrdf::NamedNodeRef<'_>,
    ) -> Result<Vec<String>, ProtocolError> {
        let Some(head) = self.one(at, pred) else {
            return Ok(Vec::new());
        };
        self.list(&head.clone())?
            .iter()
            .map(|t| match t {
                Term::Literal(lit) => Ok(lit.value().to_owned()),
                other => Err(bad(format!(
                    "expected a string in <{pred}> list, got {other}"
                ))),
            })
            .collect()
    }

    /// An ordered rdf:List of values (rsntr:params, rsntr:oneOf).
    fn value_list(
        &self,
        at: &NamedOrBlankNode,
        pred: oxrdf::NamedNodeRef<'_>,
    ) -> Result<Vec<Value>, ProtocolError> {
        let Some(head) = self.one(at, pred) else {
            return Ok(Vec::new());
        };
        self.list(&head.clone())?
            .iter()
            .map(|t| self.value_of(t))
            .collect()
    }

    /// One RDF term in value position, mapped per the literal mapping.
    fn value_of(&self, term: &Term) -> Result<Value, ProtocolError> {
        match term {
            Term::NamedNode(n) if n.as_ref() == prop::NULL => Ok(Value::Null),
            Term::NamedNode(n) => Err(bad(format!("unexpected IRI in value position: <{n}>"))),
            Term::BlankNode(b) => {
                // The only structured value is a BlobRef node.
                let at = NamedOrBlankNode::BlankNode(b.clone());
                match self.one(&at, rdf::TYPE) {
                    Some(Term::NamedNode(c)) if c.as_ref() == cls::BLOB_REF => {
                        let hash = self.need_text(&at, prop::HASH)?;
                        let lex = self.need_text(&at, prop::BYTES)?;
                        let bytes = lex.trim().parse::<u64>().map_err(|e| {
                            bad(format!("invalid rsntr:bytes literal {lex:?}: {e}"))
                        })?;
                        Ok(Value::BlobRef { hash, bytes })
                    }
                    _ => Err(bad("blank node in value position is not a rsntr:BlobRef")),
                }
            }
            Term::Literal(lit) => {
                let dt = lit.datatype();
                if dt == xsd::STRING {
                    Ok(Value::Text(lit.value().to_owned()))
                } else if dt == xsd::INTEGER {
                    Ok(Value::Integer(parse_integer(lit.value())?))
                } else if dt == xsd::DOUBLE || dt == xsd::FLOAT || dt == xsd::DECIMAL {
                    Ok(Value::Real(parse_double(lit.value())?))
                } else if dt == XSD_BASE64_BINARY {
                    Ok(Value::Blob(decode_base64(lit.value())?))
                } else {
                    Err(bad(format!(
                        "unsupported literal datatype <{dt}> in value position"
                    )))
                }
            }
            #[allow(unreachable_patterns)]
            other => Err(bad(format!("unsupported term in value position: {other}"))),
        }
    }
}

/// Assembles the envelope object out of one frame's triples.
fn decode(triples: Vec<Triple>) -> Result<EnvelopeObject, ProtocolError> {
    let doc = Doc { triples };

    // Scan for typed subjects, keeping document order and dropping
    // duplicates from repeated type triples. BlobRef nodes are values,
    // never envelope objects. A subject typed with a non-rsntr class is
    // noted but tolerated only where the protocol says so: as an
    // unknown-subclass point in a Projection frame, or as payload inside
    // a Vibration or Graph frame.
    let mut typed: Vec<(NamedOrBlankNode, String)> = Vec::new();
    let mut alien: Option<String> = None;
    for t in &doc.triples {
        if t.predicate.as_ref() != rdf::TYPE {
            continue;
        }
        let Term::NamedNode(class) = &t.object else {
            continue;
        };
        match class.as_str().strip_prefix(RSNTR_NS) {
            None => {
                alien.get_or_insert_with(|| class.as_str().to_owned());
            }
            Some("BlobRef") => {}
            Some(local) => {
                if !typed.iter().any(|(s, _)| s == &t.subject) {
                    typed.push((t.subject.clone(), local.to_owned()));
                }
            }
        }
    }

    if typed.is_empty() {
        return Err(bad("frame contains no envelope object"));
    }

    // A batch of rows and nothing else.
    if typed.iter().all(|(_, c)| c == "Row") {
        if let Some(class) = alien {
            return Err(bad(format!("subject typed with non-rsntr class <{class}>")));
        }
        let rows = typed
            .iter()
            .map(|(s, _)| read_row(&doc, s))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(EnvelopeObject::Row(rows));
    }

    // Frames whose typed subject travels with companions. Points of any
    // class ride inside a Projection frame and are reached only through
    // the offers list; Vibration and Graph frames carry payload triples
    // about arbitrary subjects.
    if let Some(s) = only(&typed, "Projection")? {
        return read_projection(&doc, s);
    }
    if let Some(s) = only(&typed, "Vibration")? {
        return read_vibration(&doc, s);
    }
    if let Some(s) = only(&typed, "Graph")? {
        return read_graph(&doc, s);
    }

    if let Some(class) = alien {
        return Err(bad(format!("subject typed with non-rsntr class <{class}>")));
    }
    if typed.len() > 1 {
        return Err(bad(format!(
            "frame contains {} typed subjects; exactly one is allowed (only rsntr:Row may repeat)",
            typed.len()
        )));
    }

    let (s, class) = &typed[0];
    match class.as_str() {
        "Query" => Ok(EnvelopeObject::Query(read_statement(&doc, s)?)),
        "Execute" => Ok(EnvelopeObject::Execute(read_statement(&doc, s)?)),
        "Result" => Ok(EnvelopeObject::Result(ResultHeader {
            id: doc.need_text(s, prop::ID)?,
            columns: doc.text_list(s, prop::COLUMN)?,
            decl_types: doc.text_list(s, prop::DECL_TYPE)?,
        })),
        "Done" => Ok(EnvelopeObject::Done(Done {
            id: doc.need_text(s, prop::ID)?,
            row_count: doc.int(s, prop::ROW_COUNT)?,
            affected_rows: doc.int(s, prop::AFFECTED_ROWS)?,
            last_insert_rowid: doc.int(s, prop::LAST_INSERT_ROWID)?,
            truncated: doc.boolean(s, prop::TRUNCATED)?.unwrap_or(false),
        })),
        "Denied" => Ok(EnvelopeObject::Denied(Denied {
            id: doc.text(s, prop::ID)?,
            reason: doc.text(s, prop::REASON)?,
        })),
        "Error" => Ok(EnvelopeObject::Error(ErrorEnvelope {
            id: doc.text(s, prop::ID)?,
            code: doc.need_text(s, prop::CODE)?,
            reason: doc.text(s, prop::REASON)?,
        })),
        "Hello" => Ok(EnvelopeObject::Hello(Hello {
            envelope_version: doc.need_text(s, prop::VER)?,
            encodings: doc.texts(s, prop::ENC)?,
            mods: doc.texts(s, prop::MODS)?,
            hint: doc.text(s, prop::HINT)?,
        })),
        "Knock" => Ok(EnvelopeObject::Knock(Knock {
            id: doc.text(s, prop::ID)?,
            message: doc.need_text(s, prop::MESSAGE)?,
        })),
        "Presence" => Ok(EnvelopeObject::Presence(Presence {
            at: doc.need_text(s, prop::AT)?,
            status: doc.text(s, prop::STATUS)?,
            endpoint: doc.text(s, prop::ENDPOINT)?,
        })),
        "Decision" => Ok(EnvelopeObject::Decision(Decision {
            id: doc.text(s, prop::ID)?,
            decision: doc.need_text(s, prop::DECISION)?,
            decided_by: doc.need_text(s, prop::DECIDED_BY)?,
            reason: doc.text(s, prop::REASON)?,
            at: doc.text(s, prop::AT)?,
        })),
        "Help" => Ok(EnvelopeObject::Help(Help {
            id: doc.text(s, prop::ID)?,
            signal: doc.need_text(s, prop::SIGNAL)?,
            topics: doc.texts(s, prop::TOPIC)?,
        })),
        "Media" => Ok(EnvelopeObject::Media(Media {
            id: doc.need_text(s, prop::ID)?,
            content_type: doc.need_text(s, prop::CONTENT_TYPE)?,
        })),
        "AudioDuplex" => Ok(EnvelopeObject::AudioDuplex(AudioDuplex {
            id: doc.need_text(s, prop::ID)?,
            content_type: doc.text(s, prop::CONTENT_TYPE)?,
            accepts: doc.need_text(s, prop::ACCEPTS)?,
        })),
        "Entrain" => Ok(EnvelopeObject::Entrain(Entrain {
            id: doc.need_text(s, prop::ID)?,
            point: doc.need_iri(s, prop::POINT)?,
        })),
        "Damp" => Ok(EnvelopeObject::Damp(Damp {
            id: doc.text(s, prop::ID)?,
            point: doc.need_iri(s, prop::POINT)?,
        })),
        other => Ok(EnvelopeObject::Generic(Generic {
            class: other.to_owned(),
            props: doc
                .pairs(s)
                .filter(|(p, _)| p.as_ref() != rdf::TYPE)
                .map(|(p, o)| (p.clone(), o.clone()))
                .collect(),
        })),
    }
}

/// The single subject carrying `class`, if any; more than one is an error.
fn only<'a>(
    typed: &'a [(NamedOrBlankNode, String)],
    class: &str,
) -> Result<Option<&'a NamedOrBlankNode>, ProtocolError> {
    let mut hits = typed.iter().filter(|(_, c)| c == class);
    match (hits.next(), hits.next()) {
        (None, _) => Ok(None),
        (Some((s, _)), None) => Ok(Some(s)),
        (Some(_), Some(_)) => Err(bad(format!(
            "frame contains more than one rsntr:{class} subject"
        ))),
    }
}

fn read_statement(doc: &Doc, s: &NamedOrBlankNode) -> Result<Statement, ProtocolError> {
    Ok(Statement {
        id: doc.need_text(s, prop::ID)?,
        modulation: doc.need_text(s, prop::MOD)?,
        signal: doc.need_text(s, prop::SIGNAL)?,
        params: doc.value_list(s, prop::PARAMS)?,
        database: doc.text(s, prop::DATABASE)?,
        row_limit: doc.int(s, prop::ROW_LIMIT)?,
        byte_limit: doc.int(s, prop::BYTE_LIMIT)?,
        timeout_ms: doc.int(s, prop::TIMEOUT_MS)?,
    })
}

fn read_row(doc: &Doc, s: &NamedOrBlankNode) -> Result<Row, ProtocolError> {
    let mut seq: Option<i64> = None;
    let mut cells = Vec::new();
    for (p, o) in doc.pairs(s) {
        if p.as_ref() == rdf::TYPE {
            continue;
        }
        if p.as_ref() == prop::SEQ {
            let Term::Literal(lit) = o else {
                return Err(bad(format!("rsntr:seq must be a literal, got {o}")));
            };
            seq = Some(parse_integer(lit.value())?);
            continue;
        }
        // Cell predicates are rsntr:col_<encoded-name>; anything else on
        // a Row is an unknown predicate and is skipped (open world).
        if let Some(encoded) = p
            .as_str()
            .strip_prefix(RSNTR_NS)
            .and_then(|local| local.strip_prefix(COL_PREFIX))
        {
            cells.push((decode_column_name(encoded)?, doc.value_of(o)?));
        }
    }
    Ok(Row {
        seq: seq.ok_or_else(|| bad("rsntr:Row without rsntr:seq"))?,
        cells,
    })
}

/// A term where a subject-position node is required.
fn as_node(term: &Term) -> Result<NamedOrBlankNode, ProtocolError> {
    match term {
        Term::NamedNode(n) => Ok(NamedOrBlankNode::NamedNode(n.clone())),
        Term::BlankNode(b) => Ok(NamedOrBlankNode::BlankNode(b.clone())),
        other => Err(bad(format!("expected a node, got {other}"))),
    }
}

fn read_projection(doc: &Doc, s: &NamedOrBlankNode) -> Result<EnvelopeObject, ProtocolError> {
    let id = doc.text(s, prop::ID)?;
    let mut offers = Vec::new();
    if let Some(head) = doc.one(s, prop::OFFERS) {
        for term in doc.list(&head.clone())? {
            let Term::NamedNode(iri) = term else {
                return Err(bad(format!(
                    "rsntr:offers entries must be point IRIs, got {term}"
                )));
            };
            offers.push(read_point(doc, &iri)?);
        }
    }
    Ok(EnvelopeObject::Projection(Projection { id, offers }))
}

fn read_point(doc: &Doc, iri: &NamedNode) -> Result<Point, ProtocolError> {
    let s = NamedOrBlankNode::NamedNode(iri.clone());
    // Open world: an unrecognized class (rsntr or foreign) is Other, a
    // missing type is Bare, and neither is ever an error.
    let kind = match doc.one(&s, rdf::TYPE) {
        None => PointKind::Bare,
        Some(Term::NamedNode(c)) if c.as_ref() == cls::EXCITABLE => PointKind::Excitable,
        Some(Term::NamedNode(c)) if c.as_ref() == cls::RADIANT => PointKind::Radiant,
        Some(Term::NamedNode(c)) if c.as_ref() == cls::SYMPATHETIC => PointKind::Sympathetic,
        Some(Term::NamedNode(c)) if c.as_ref() == cls::RESONANCE_POINT => PointKind::Bare,
        Some(Term::NamedNode(c)) => PointKind::Other(c.as_str().to_owned()),
        Some(other) => {
            return Err(bad(format!("point type must be an IRI, got {other}")));
        }
    };
    let coupling = match doc.one(&s, prop::COUPLING) {
        None => Vec::new(),
        Some(t) => read_coupling(doc, &t.clone())?,
    };
    Ok(Point {
        iri: iri.as_str().to_owned(),
        kind,
        label: doc.text(&s, RDFS_LABEL)?,
        comment: doc.text(&s, RDFS_COMMENT)?,
        icon: doc.text(&s, prop::ICON)?,
        role: doc.text(&s, prop::ROLE)?,
        projects: doc.text(&s, prop::PROJECTS)?,
        coupling,
        modulation: doc.text(&s, prop::MOD)?,
        signal: doc.text(&s, prop::SIGNAL)?,
        params_order: doc.text_list(&s, prop::PARAMS_ORDER)?,
        signal_template: doc.text(&s, prop::SIGNAL_TEMPLATE)?,
        fires: doc.text(&s, prop::FIRES)?,
    })
}

fn read_coupling(doc: &Doc, t: &Term) -> Result<Vec<PointField>, ProtocolError> {
    let node = as_node(t)?;
    let Some(head) = doc.one(&node, prop::FIELD) else {
        return Ok(Vec::new());
    };
    doc.list(&head.clone())?
        .iter()
        .map(|ft| read_field(doc, ft))
        .collect()
}

fn read_field(doc: &Doc, t: &Term) -> Result<PointField, ProtocolError> {
    let node = as_node(t)?;
    let default = match doc.one(&node, prop::DEFAULT) {
        None => None,
        Some(o) => Some(doc.value_of(&o.clone())?),
    };
    Ok(PointField {
        name: doc.need_text(&node, prop::NAME)?,
        datatype: doc.iri(&node, prop::DATATYPE)?,
        required: doc.boolean(&node, prop::REQUIRED)?.unwrap_or(false),
        default,
        one_of: doc.value_list(&node, prop::ONE_OF)?,
        hint: doc.text(&node, prop::HINT)?,
    })
}

/// The triples of a frame that are not about the header subject itself.
fn companions(doc: &Doc, s: &NamedOrBlankNode) -> Vec<Triple> {
    doc.triples
        .iter()
        .filter(|t| &t.subject != s)
        .cloned()
        .collect()
}

fn read_vibration(doc: &Doc, s: &NamedOrBlankNode) -> Result<EnvelopeObject, ProtocolError> {
    Ok(EnvelopeObject::Vibration(Vibration {
        id: doc.need_text(s, prop::ID)?,
        point: doc.need_iri(s, prop::POINT)?,
        seq: doc
            .int(s, prop::SEQ)?
            .ok_or_else(|| bad("rsntr:Vibration without rsntr:seq"))?,
        at: doc.text(s, prop::AT)?,
        payload: companions(doc, s),
    }))
}

fn read_graph(doc: &Doc, s: &NamedOrBlankNode) -> Result<EnvelopeObject, ProtocolError> {
    Ok(EnvelopeObject::Graph(Graph {
        id: doc.need_text(s, prop::ID)?,
        seq: doc
            .int(s, prop::SEQ)?
            .ok_or_else(|| bad("rsntr:Graph without rsntr:seq"))?,
        payload: companions(doc, s),
    }))
}

// ---------------------------------------------------------------------------
// Column predicate minting: rsntr:col_<name>, percent-encoded
// ---------------------------------------------------------------------------

/// Encodes a column name for a `rsntr:col_<name>` predicate: every byte
/// outside `[A-Za-z0-9_]` becomes `%XX` (uppercase hex), which is valid
/// both in an IRI and in a Turtle prefixed local name (PLX).
pub fn encode_column_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Inverse of [`encode_column_name`].
pub fn decode_column_name(encoded: &str) -> Result<String, ProtocolError> {
    fn hex_pair(hi: u8, lo: u8) -> Option<u8> {
        let digit = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
        Some(digit(hi)? * 16 + digit(lo)?)
    }
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut input = encoded.bytes();
    while let Some(b) = input.next() {
        if b != b'%' {
            bytes.push(b);
            continue;
        }
        let (Some(hi), Some(lo)) = (input.next(), input.next()) else {
            return Err(bad(format!(
                "truncated percent escape in column predicate {encoded:?}"
            )));
        };
        match hex_pair(hi, lo) {
            Some(v) => bytes.push(v),
            None => {
                return Err(bad(format!(
                    "invalid percent escape in column predicate {encoded:?}"
                )));
            }
        }
    }
    String::from_utf8(bytes).map_err(|_| {
        bad(format!(
            "column predicate {encoded:?} does not decode to UTF-8"
        ))
    })
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Escapes for a Turtle double-quoted (short) string literal.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Escapes for a triple-quoted long-string literal. Newlines and tabs
/// stay literal (the reason to use the long form); every `"` is escaped
/// so no embedded `"""` can close the literal early; `\` doubles; other
/// control characters become `\uXXXX`.
fn quote_long(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 6);
    out.push_str("\"\"\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\t' => out.push(c),
            '\r' => out.push_str("\\r"),
            _ if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push_str("\"\"\"");
    out
}

/// Picks the literal form for possibly-multiline text: long-string when a
/// newline is present, short otherwise. Both round-trip losslessly.
fn quote_text(s: &str) -> String {
    if s.contains('\n') {
        quote_long(s)
    } else {
        quote(s)
    }
}

fn iri_term(iri: &str) -> String {
    format!("<{iri}>")
}

/// One value in Turtle object position.
fn value_term(v: &Value) -> String {
    match v {
        Value::Null => "rsntr:null".to_owned(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => format!("{}^^xsd:double", quote(&format_double(*f))),
        Value::Text(s) => quote(s),
        Value::Blob(b) => format!("{}^^xsd:base64Binary", quote(&encode_base64(b))),
        Value::BlobRef { hash, bytes } => format!(
            "[ a rsntr:BlobRef ; rsntr:hash {} ; rsntr:bytes {bytes} ]",
            quote(hash)
        ),
    }
}

/// An rdf collection `( ... )` of quoted strings.
fn text_collection(items: &[String]) -> String {
    let terms: Vec<String> = items.iter().map(|s| quote(s)).collect();
    format!("({})", terms.join(" "))
}

/// An rdf collection `( ... )` of values.
fn value_collection(items: &[Value]) -> String {
    let terms: Vec<String> = items.iter().map(value_term).collect();
    format!("({})", terms.join(" "))
}

/// Builds one `subject a class ; pred obj ; ... .` block in the doc
/// style: each pair on its own indented continuation line.
struct Block {
    text: String,
}

impl Block {
    /// A blank-node subject typed `rsntr:<class>`.
    fn anon(class: &str) -> Self {
        Self {
            text: format!("[] a rsntr:{class}"),
        }
    }

    /// A named subject with an arbitrary class term.
    fn named(subject_iri: &str, class_term: &str) -> Self {
        Self {
            text: format!("{} a {class_term}", iri_term(subject_iri)),
        }
    }

    /// Appends one predicate-object pair.
    fn pair(mut self, pred: &str, obj: &str) -> Self {
        self.text.push_str(" ;\n   ");
        self.text.push_str(pred);
        self.text.push(' ');
        self.text.push_str(obj);
        self
    }

    /// A quoted string pair, when the value is present.
    fn opt_text(self, pred: &str, v: &Option<String>) -> Self {
        match v {
            Some(v) => self.pair(pred, &quote(v)),
            None => self,
        }
    }

    /// An integer pair, when the value is present.
    fn opt_int(self, pred: &str, v: &Option<i64>) -> Self {
        match v {
            Some(v) => self.pair(pred, &v.to_string()),
            None => self,
        }
    }

    /// A repeated-object pair (comma-separated), skipped when empty.
    fn repeated(self, pred: &str, items: &[String]) -> Self {
        if items.is_empty() {
            return self;
        }
        let objs: Vec<String> = items.iter().map(|s| quote(s)).collect();
        self.pair(pred, &objs.join(", "))
    }

    /// An `xsd:dateTime` pair, when the value is present.
    fn opt_datetime(self, pred: &str, v: &Option<String>) -> Self {
        match v {
            Some(v) => self.pair(pred, &format!("{}^^xsd:dateTime", quote(v))),
            None => self,
        }
    }

    /// Terminates the block.
    fn close(mut self) -> String {
        self.text.push_str(" .\n");
        self.text
    }
}

fn statement_block(class: &str, st: &Statement) -> String {
    let mut b = Block::anon(class)
        .pair("rsntr:id", &quote(&st.id))
        .pair("rsntr:mod", &quote(&st.modulation))
        .pair("rsntr:signal", &quote_text(&st.signal));
    if !st.params.is_empty() {
        b = b.pair("rsntr:params", &value_collection(&st.params));
    }
    b.opt_text("rsntr:database", &st.database)
        .opt_int("rsntr:rowLimit", &st.row_limit)
        .opt_int("rsntr:byteLimit", &st.byte_limit)
        .opt_int("rsntr:timeoutMs", &st.timeout_ms)
        .close()
}

/// Appends companion triples one per line in N-Triples-shaped Turtle
/// (full IRIs; the oxrdf Display impls emit valid syntax).
fn append_payload(out: &mut String, payload: &[Triple]) {
    for t in payload {
        out.push_str(&format!("{} {} {} .\n", t.subject, t.predicate, t.object));
    }
}

fn encode(obj: &EnvelopeObject) -> Result<String, ProtocolError> {
    Ok(match obj {
        EnvelopeObject::Query(st) => statement_block("Query", st),
        EnvelopeObject::Execute(st) => statement_block("Execute", st),
        EnvelopeObject::Result(h) => {
            let mut b = Block::anon("Result")
                .pair("rsntr:id", &quote(&h.id))
                .pair("rsntr:column", &text_collection(&h.columns));
            if !h.decl_types.is_empty() {
                b = b.pair("rsntr:declType", &text_collection(&h.decl_types));
            }
            b.close()
        }
        EnvelopeObject::Row(rows) => {
            if rows.is_empty() {
                return Err(bad("a row frame must contain at least one rsntr:Row"));
            }
            let mut out = String::new();
            for row in rows {
                out.push_str(&format!("[] a rsntr:Row ; rsntr:seq {}", row.seq));
                for (name, v) in &row.cells {
                    // A NULL cell is written as column omission.
                    if matches!(v, Value::Null) {
                        continue;
                    }
                    out.push_str(&format!(
                        " ; rsntr:{COL_PREFIX}{} {}",
                        encode_column_name(name),
                        value_term(v)
                    ));
                }
                out.push_str(" .\n");
            }
            out
        }
        EnvelopeObject::Done(d) => Block::anon("Done")
            .pair("rsntr:id", &quote(&d.id))
            .opt_int("rsntr:rowCount", &d.row_count)
            .opt_int("rsntr:affectedRows", &d.affected_rows)
            .opt_int("rsntr:lastInsertRowid", &d.last_insert_rowid)
            .pair("rsntr:truncated", &d.truncated.to_string())
            .close(),
        EnvelopeObject::Denied(d) => Block::anon("Denied")
            .opt_text("rsntr:id", &d.id)
            .opt_text("rsntr:reason", &d.reason)
            .close(),
        EnvelopeObject::Error(e) => Block::anon("Error")
            .opt_text("rsntr:id", &e.id)
            .pair("rsntr:code", &quote(&e.code))
            .opt_text("rsntr:reason", &e.reason)
            .close(),
        EnvelopeObject::Hello(h) => Block::anon("Hello")
            .pair("rsntr:ver", &quote(&h.envelope_version))
            .repeated("rsntr:enc", &h.encodings)
            .repeated("rsntr:mods", &h.mods)
            .opt_text("rsntr:hint", &h.hint)
            .close(),
        EnvelopeObject::Knock(k) => Block::anon("Knock")
            .opt_text("rsntr:id", &k.id)
            .pair("rsntr:message", &quote(&k.message))
            .close(),
        EnvelopeObject::Presence(p) => Block::anon("Presence")
            .pair("rsntr:at", &format!("{}^^xsd:dateTime", quote(&p.at)))
            .opt_text("rsntr:status", &p.status)
            .opt_text("rsntr:endpoint", &p.endpoint)
            .close(),
        EnvelopeObject::Decision(d) => Block::anon("Decision")
            .opt_text("rsntr:id", &d.id)
            .pair("rsntr:decision", &quote(&d.decision))
            .pair("rsntr:decidedBy", &quote(&d.decided_by))
            .opt_text("rsntr:reason", &d.reason)
            .opt_datetime("rsntr:at", &d.at)
            .close(),
        EnvelopeObject::Help(h) => {
            let mut b = Block::anon("Help").opt_text("rsntr:id", &h.id);
            if !h.topics.is_empty() {
                let objs: Vec<String> = h.topics.iter().map(|s| quote(s)).collect();
                b = b.pair("rsntr:topic", &objs.join(", "));
            }
            b.pair("rsntr:signal", &quote_text(&h.signal)).close()
        }
        EnvelopeObject::Media(m) => Block::anon("Media")
            .pair("rsntr:id", &quote(&m.id))
            .pair("rsntr:contentType", &quote(&m.content_type))
            .close(),
        EnvelopeObject::AudioDuplex(d) => {
            let mut b = Block::anon("AudioDuplex").pair("rsntr:id", &quote(&d.id));
            if let Some(ct) = &d.content_type {
                b = b.pair("rsntr:contentType", &quote(ct));
            }
            b.pair("rsntr:accepts", &quote(&d.accepts)).close()
        }
        EnvelopeObject::Graph(gr) => {
            let mut out = Block::anon("Graph")
                .pair("rsntr:id", &quote(&gr.id))
                .pair("rsntr:seq", &gr.seq.to_string())
                .close();
            append_payload(&mut out, &gr.payload);
            out
        }
        EnvelopeObject::Generic(g) => {
            let plain = !g.class.is_empty()
                && g.class
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_');
            if !plain {
                return Err(bad(format!(
                    "generic class {:?} is not a plain rsntr local name",
                    g.class
                )));
            }
            let mut b = Block::anon(&g.class);
            for (p, o) in &g.props {
                b = b.pair(&p.to_string(), &o.to_string());
            }
            b.close()
        }
        EnvelopeObject::Projection(p) => {
            let mut b = Block::anon("Projection").opt_text("rsntr:id", &p.id);
            if !p.offers.is_empty() {
                let iris: Vec<String> = p.offers.iter().map(|pt| iri_term(&pt.iri)).collect();
                b = b.pair("rsntr:offers", &format!("({})", iris.join(" ")));
            }
            let mut out = b.close();
            for pt in &p.offers {
                out.push('\n');
                out.push_str(&point_block(pt));
            }
            out
        }
        EnvelopeObject::Entrain(e) => Block::anon("Entrain")
            .pair("rsntr:id", &quote(&e.id))
            .pair("rsntr:point", &iri_term(&e.point))
            .close(),
        EnvelopeObject::Damp(d) => Block::anon("Damp")
            .opt_text("rsntr:id", &d.id)
            .pair("rsntr:point", &iri_term(&d.point))
            .close(),
        EnvelopeObject::Vibration(v) => {
            let mut out = Block::anon("Vibration")
                .pair("rsntr:id", &quote(&v.id))
                .pair("rsntr:point", &iri_term(&v.point))
                .pair("rsntr:seq", &v.seq.to_string())
                .opt_datetime("rsntr:at", &v.at)
                .close();
            append_payload(&mut out, &v.payload);
            out
        }
    })
}

// ---------------------------------------------------------------------------
// Projection encoding
// ---------------------------------------------------------------------------

/// The class term for a point: a prefixed name for the known kinds, the
/// full IRI for an unknown one (round-trip fidelity).
fn point_class_term(kind: &PointKind) -> String {
    match kind {
        PointKind::Excitable => "rsntr:Excitable".into(),
        PointKind::Radiant => "rsntr:Radiant".into(),
        PointKind::Sympathetic => "rsntr:Sympathetic".into(),
        PointKind::Bare => "rsntr:ResonancePoint".into(),
        PointKind::Other(iri) => iri_term(iri),
    }
}

/// An xsd datatype prints prefixed; anything else prints as a full IRI.
fn datatype_term(iri: &str) -> String {
    match iri.strip_prefix(XSD_NS) {
        Some(local) => format!("xsd:{local}"),
        None => iri_term(iri),
    }
}

/// A coupling field as an inline `[ ... ]` blank node.
fn field_term(f: &PointField) -> String {
    let mut parts = vec![format!("rsntr:name {}", quote(&f.name))];
    if let Some(dt) = &f.datatype {
        parts.push(format!("rsntr:datatype {}", datatype_term(dt)));
    }
    if f.required {
        parts.push("rsntr:required true".into());
    }
    if let Some(d) = &f.default {
        parts.push(format!("rsntr:default {}", value_term(d)));
    }
    if !f.one_of.is_empty() {
        parts.push(format!("rsntr:oneOf {}", value_collection(&f.one_of)));
    }
    if let Some(h) = &f.hint {
        parts.push(format!("rsntr:hint {}", quote(h)));
    }
    format!("[ {} ]", parts.join(" ; "))
}

fn point_block(pt: &Point) -> String {
    let mut b = Block::named(&pt.iri, &point_class_term(&pt.kind))
        .opt_text("rdfs:label", &pt.label)
        .opt_text("rdfs:comment", &pt.comment)
        .opt_text("rsntr:icon", &pt.icon)
        .opt_text("rsntr:role", &pt.role)
        .opt_text("rsntr:projects", &pt.projects);
    if !pt.coupling.is_empty() {
        let fields: Vec<String> = pt.coupling.iter().map(field_term).collect();
        b = b.pair(
            "rsntr:coupling",
            &format!("[ rsntr:field ({}) ]", fields.join(" ")),
        );
    }
    b = b.opt_text("rsntr:mod", &pt.modulation);
    if let Some(t) = &pt.signal {
        b = b.pair("rsntr:signal", &quote_text(t));
    }
    if !pt.params_order.is_empty() {
        b = b.pair("rsntr:paramsOrder", &text_collection(&pt.params_order));
    }
    if let Some(t) = &pt.signal_template {
        b = b.pair("rsntr:signalTemplate", &quote_text(t));
    }
    b.opt_text("rsntr:fires", &pt.fires).close()
}
