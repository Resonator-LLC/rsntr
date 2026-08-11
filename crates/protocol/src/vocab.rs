//! The `rsntr:` vocabulary as IRI constants, plus the implied prefix block.
//!
//! Namespace: `http://resonator.network/v3/rsntr#`. The implied prefix block
//! for all envelope parsing is `rsntr:`, `xsd:`, `rdf:`, `rdfs:`; it is
//! registered on the parser (never transmitted) and stripped on write.

use oxrdf::NamedNodeRef;

/// The `rsntr:` namespace IRI.
pub const RSNTR_NS: &str = "http://resonator.network/v3/rsntr#";
/// The `xsd:` namespace IRI.
pub const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema#";
/// The `rdf:` namespace IRI.
pub const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
/// The `rdfs:` namespace IRI.
pub const RDFS_NS: &str = "http://www.w3.org/2000/01/rdf-schema#";

/// The implied prefix block: (prefix name, namespace IRI) pairs.
pub const IMPLIED_PREFIXES: [(&str, &str); 4] = [
    ("rsntr", RSNTR_NS),
    ("xsd", XSD_NS),
    ("rdf", RDF_NS),
    ("rdfs", RDFS_NS),
];

macro_rules! rsntr_iri {
    ($local:literal) => {
        NamedNodeRef::new_unchecked(concat!("http://resonator.network/v3/rsntr#", $local))
    };
}

/// Envelope classes.
pub mod cls {
    use super::NamedNodeRef;

    pub const QUERY: NamedNodeRef<'static> = rsntr_iri!("Query");
    pub const EXECUTE: NamedNodeRef<'static> = rsntr_iri!("Execute");
    pub const RESULT: NamedNodeRef<'static> = rsntr_iri!("Result");
    pub const ROW: NamedNodeRef<'static> = rsntr_iri!("Row");
    pub const DONE: NamedNodeRef<'static> = rsntr_iri!("Done");
    pub const DENIED: NamedNodeRef<'static> = rsntr_iri!("Denied");
    pub const ERROR: NamedNodeRef<'static> = rsntr_iri!("Error");
    pub const HELLO: NamedNodeRef<'static> = rsntr_iri!("Hello");
    pub const KNOCK: NamedNodeRef<'static> = rsntr_iri!("Knock");
    pub const PRESENCE: NamedNodeRef<'static> = rsntr_iri!("Presence");
    pub const DECISION: NamedNodeRef<'static> = rsntr_iri!("Decision");
    pub const HELP: NamedNodeRef<'static> = rsntr_iri!("Help");
    pub const MEDIA: NamedNodeRef<'static> = rsntr_iri!("Media");
    /// A frame whose remaining triples are an arbitrary payload graph
    /// (v3 addition; carries CONSTRUCT results for the sparql modulation).
    pub const GRAPH: NamedNodeRef<'static> = rsntr_iri!("Graph");
    /// Value node class, not an envelope object.
    pub const BLOB_REF: NamedNodeRef<'static> = rsntr_iri!("BlobRef");
    /// One chat message (chat protocol sec 2); never a frame-level
    /// object, only a node inside a `chat` modulation signal.
    pub const MESSAGE: NamedNodeRef<'static> = rsntr_iri!("Message");

    // Projection vocabulary (projection protocol sec 2).
    pub const PROJECTION: NamedNodeRef<'static> = rsntr_iri!("Projection");
    pub const RESONANCE_POINT: NamedNodeRef<'static> = rsntr_iri!("ResonancePoint");
    pub const EXCITABLE: NamedNodeRef<'static> = rsntr_iri!("Excitable");
    pub const RADIANT: NamedNodeRef<'static> = rsntr_iri!("Radiant");
    pub const SYMPATHETIC: NamedNodeRef<'static> = rsntr_iri!("Sympathetic");
    /// Input-contract node classes; never required as rdf:type on the wire
    /// (couplings and fields ride as untyped blank nodes).
    pub const COUPLING: NamedNodeRef<'static> = rsntr_iri!("Coupling");
    pub const FIELD: NamedNodeRef<'static> = rsntr_iri!("Field");
    pub const ENTRAIN: NamedNodeRef<'static> = rsntr_iri!("Entrain");
    pub const VIBRATION: NamedNodeRef<'static> = rsntr_iri!("Vibration");
    pub const DAMP: NamedNodeRef<'static> = rsntr_iri!("Damp");
}

/// Envelope properties.
pub mod prop {
    use super::NamedNodeRef;

    pub const ID: NamedNodeRef<'static> = rsntr_iri!("id");
    pub const MOD: NamedNodeRef<'static> = rsntr_iri!("mod");
    pub const SIGNAL: NamedNodeRef<'static> = rsntr_iri!("signal");
    pub const PARAMS: NamedNodeRef<'static> = rsntr_iri!("params");
    pub const DATABASE: NamedNodeRef<'static> = rsntr_iri!("database");
    pub const ROW_LIMIT: NamedNodeRef<'static> = rsntr_iri!("rowLimit");
    pub const BYTE_LIMIT: NamedNodeRef<'static> = rsntr_iri!("byteLimit");
    pub const TIMEOUT_MS: NamedNodeRef<'static> = rsntr_iri!("timeoutMs");
    pub const COLUMN: NamedNodeRef<'static> = rsntr_iri!("column");
    pub const DECL_TYPE: NamedNodeRef<'static> = rsntr_iri!("declType");
    pub const SEQ: NamedNodeRef<'static> = rsntr_iri!("seq");
    pub const ROW_COUNT: NamedNodeRef<'static> = rsntr_iri!("rowCount");
    pub const AFFECTED_ROWS: NamedNodeRef<'static> = rsntr_iri!("affectedRows");
    pub const LAST_INSERT_ROWID: NamedNodeRef<'static> = rsntr_iri!("lastInsertRowid");
    pub const TRUNCATED: NamedNodeRef<'static> = rsntr_iri!("truncated");
    pub const CODE: NamedNodeRef<'static> = rsntr_iri!("code");
    pub const REASON: NamedNodeRef<'static> = rsntr_iri!("reason");
    pub const VER: NamedNodeRef<'static> = rsntr_iri!("ver");
    pub const ENC: NamedNodeRef<'static> = rsntr_iri!("enc");
    /// Modulations a node works: it executes them for peers and can issue
    /// them itself.
    pub const MODS: NamedNodeRef<'static> = rsntr_iri!("mods");
    pub const HINT: NamedNodeRef<'static> = rsntr_iri!("hint");
    pub const TOPIC: NamedNodeRef<'static> = rsntr_iri!("topic");
    pub const CONTENT_TYPE: NamedNodeRef<'static> = rsntr_iri!("contentType");
    /// Upstream media type an audio-duplex source's stdin accepts.
    pub const ACCEPTS: NamedNodeRef<'static> = rsntr_iri!("accepts");
    pub const AT: NamedNodeRef<'static> = rsntr_iri!("at");
    pub const STATUS: NamedNodeRef<'static> = rsntr_iri!("status");
    /// A presence beacon's self-declared author endpoint id (64-hex ed25519).
    pub const ENDPOINT: NamedNodeRef<'static> = rsntr_iri!("endpoint");
    pub const MESSAGE: NamedNodeRef<'static> = rsntr_iri!("message");
    pub const DECISION: NamedNodeRef<'static> = rsntr_iri!("decision");
    pub const DECIDED_BY: NamedNodeRef<'static> = rsntr_iri!("decidedBy");
    pub const HASH: NamedNodeRef<'static> = rsntr_iri!("hash");
    pub const BYTES: NamedNodeRef<'static> = rsntr_iri!("bytes");
    /// The positional NULL marker individual used in parameter lists.
    pub const NULL: NamedNodeRef<'static> = rsntr_iri!("null");

    // Chat vocabulary (chat protocol sec 11); appears only inside chat
    // signal payloads, never in envelope frames.
    pub const FROM: NamedNodeRef<'static> = rsntr_iri!("from");
    pub const ROOM: NamedNodeRef<'static> = rsntr_iri!("room");
    pub const BODY: NamedNodeRef<'static> = rsntr_iri!("body");
    pub const ATTACHMENT: NamedNodeRef<'static> = rsntr_iri!("attachment");

    // Projection vocabulary (projection protocol sec 2).
    pub const OFFERS: NamedNodeRef<'static> = rsntr_iri!("offers");
    pub const PROJECTS: NamedNodeRef<'static> = rsntr_iri!("projects");
    pub const COUPLING: NamedNodeRef<'static> = rsntr_iri!("coupling");
    pub const FIELD: NamedNodeRef<'static> = rsntr_iri!("field");
    pub const NAME: NamedNodeRef<'static> = rsntr_iri!("name");
    pub const DATATYPE: NamedNodeRef<'static> = rsntr_iri!("datatype");
    pub const REQUIRED: NamedNodeRef<'static> = rsntr_iri!("required");
    pub const DEFAULT: NamedNodeRef<'static> = rsntr_iri!("default");
    pub const ONE_OF: NamedNodeRef<'static> = rsntr_iri!("oneOf");
    pub const ICON: NamedNodeRef<'static> = rsntr_iri!("icon");
    pub const ROLE: NamedNodeRef<'static> = rsntr_iri!("role");
    pub const FIRES: NamedNodeRef<'static> = rsntr_iri!("fires");
    pub const SIGNAL_TEMPLATE: NamedNodeRef<'static> = rsntr_iri!("signalTemplate");
    pub const PARAMS_ORDER: NamedNodeRef<'static> = rsntr_iri!("paramsOrder");
    pub const POINT: NamedNodeRef<'static> = rsntr_iri!("point");
}

/// `rdfs:label` (display text on resonance points).
pub const RDFS_LABEL: NamedNodeRef<'static> =
    NamedNodeRef::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label");
/// `rdfs:comment` (description text on resonance points).
pub const RDFS_COMMENT: NamedNodeRef<'static> =
    NamedNodeRef::new_unchecked("http://www.w3.org/2000/01/rdf-schema#comment");

/// The well-known Sympathetic point every node SHOULD expose: vibrates
/// whenever the caller's projection changes (projection protocol sec 3).
pub const PROJECTION_CHANGED: &str = "urn:rsntr:projection-changed";

/// Whether a requested mod tag matches an advertised one (envelope spec).
/// Advertised tags SHOULD carry their engine version suffixed into the tag
/// (`sql-sqlite-3.46.0`); a request naming the base tag matches any version
/// of it, and a request naming a versioned tag exactly is a version pin.
pub fn mod_matches(requested: &str, advertised: &str) -> bool {
    advertised == requested
        || advertised
            .strip_prefix(requested)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
}

/// `xsd:base64Binary` (not provided by `oxrdf::vocab::xsd`).
pub const XSD_BASE64_BINARY: NamedNodeRef<'static> =
    NamedNodeRef::new_unchecked("http://www.w3.org/2001/XMLSchema#base64Binary");

/// The prefix used for minted column predicates: `rsntr:col_<name>`.
pub const COL_PREFIX: &str = "col_";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_is_v3() {
        assert_eq!(RSNTR_NS, "http://resonator.network/v3/rsntr#");
        assert!(cls::QUERY.as_str().starts_with(RSNTR_NS));
        assert!(prop::ID.as_str().starts_with(RSNTR_NS));
    }

    #[test]
    fn mod_matching() {
        assert!(mod_matches("sql-sqlite", "sql-sqlite"));
        assert!(mod_matches("sql-sqlite", "sql-sqlite-3.46.0"));
        assert!(mod_matches("sql-sqlite-3.46.0", "sql-sqlite-3.46.0"));
        assert!(!mod_matches("sql-sqlite-3.47", "sql-sqlite-3.46.0"));
        assert!(!mod_matches("sql", "sql-sqlite-3.46.0"));
        assert!(!mod_matches("sql-sqlite", "sql-sqlite-extra"));
        assert!(!mod_matches("sql-sqlite", "sparql"));
        assert!(mod_matches("help", "help"));
    }
}
