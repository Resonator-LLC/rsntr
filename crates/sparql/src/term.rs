//! RDF term model matching the rdf_terms storage row:
//! kind 0 = IRI, 1 = blank node, 2 = literal; empty string means
//! "no datatype" / "no language tag".

pub const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
pub const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
pub const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
pub const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";

pub const K_IRI: i64 = 0;
pub const K_BNODE: i64 = 1;
pub const K_LIT: i64 = 2;

/// One RDF term. Field order defines term ordering (kind, lex, dtype,
/// lang), which the Turtle serializer relies on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Term {
    pub kind: i64,
    pub lex: String,
    pub dtype: String,
    pub lang: String,
}

impl Term {
    pub fn iri(lex: impl Into<String>) -> Self {
        Term {
            kind: K_IRI,
            lex: lex.into(),
            dtype: String::new(),
            lang: String::new(),
        }
    }

    pub fn bnode(label: impl Into<String>) -> Self {
        Term {
            kind: K_BNODE,
            lex: label.into(),
            dtype: String::new(),
            lang: String::new(),
        }
    }

    pub fn lit(lex: impl Into<String>) -> Self {
        Term {
            kind: K_LIT,
            lex: lex.into(),
            dtype: String::new(),
            lang: String::new(),
        }
    }

    pub fn lit_dt(lex: impl Into<String>, dtype: impl Into<String>) -> Self {
        Term {
            kind: K_LIT,
            lex: lex.into(),
            dtype: dtype.into(),
            lang: String::new(),
        }
    }

    pub fn lit_lang(lex: impl Into<String>, lang: impl Into<String>) -> Self {
        Term {
            kind: K_LIT,
            lex: lex.into(),
            dtype: String::new(),
            lang: lang.into(),
        }
    }

    pub fn is_iri(&self) -> bool {
        self.kind == K_IRI
    }

    pub fn is_literal(&self) -> bool {
        self.kind == K_LIT
    }
}

/// Datatypes treated as numeric by FILTER comparisons. ORDER BY uses the
/// shorter integer/decimal/double/float/long/int set.
pub fn is_numeric_dtype(dtype: &str) -> bool {
    matches!(
        dtype.strip_prefix(XSD),
        Some(
            "integer"
                | "decimal"
                | "double"
                | "float"
                | "long"
                | "int"
                | "short"
                | "byte"
                | "nonNegativeInteger"
                | "positiveInteger"
                | "negativeInteger"
                | "nonPositiveInteger"
                | "unsignedLong"
                | "unsignedInt"
                | "unsignedShort"
                | "unsignedByte"
        )
    )
}
