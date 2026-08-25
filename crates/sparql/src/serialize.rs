//! Result serializers: SPARQL 1.1 JSON results (SELECT/ASK), Turtle and
//! N-Triples (CONSTRUCT, dump). Pure string builders, no SQLite dependency.

use crate::term::{K_BNODE, K_IRI, Term, XSD};

pub fn json_escape(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

fn json_term(out: &mut String, t: &Term) {
    out.push_str("{\"type\":\"");
    out.push_str(match t.kind {
        K_IRI => "uri",
        K_BNODE => "bnode",
        _ => "literal",
    });
    out.push_str("\",\"value\":\"");
    json_escape(out, &t.lex);
    out.push('"');
    if t.kind == 2 && !t.lang.is_empty() {
        out.push_str(",\"xml:lang\":\"");
        json_escape(out, &t.lang);
        out.push('"');
    } else if t.kind == 2 && !t.dtype.is_empty() {
        out.push_str(",\"datatype\":\"");
        json_escape(out, &t.dtype);
        out.push('"');
    }
    out.push('}');
}

/// SPARQL 1.1 JSON results document for SELECT.
pub fn select_results_json(vars: &[String], rows: &[Vec<Option<Term>>]) -> String {
    let mut out = String::from("{\"head\":{\"vars\":[");
    for (i, v) in vars.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        json_escape(&mut out, v);
        out.push('"');
    }
    out.push_str("]},\"results\":{\"bindings\":[");
    for (r, row) in rows.iter().enumerate() {
        if r > 0 {
            out.push(',');
        }
        out.push('{');
        let mut first = true;
        for (i, cell) in row.iter().enumerate() {
            let Some(t) = cell else { continue };
            if !first {
                out.push(',');
            }
            first = false;
            out.push('"');
            json_escape(&mut out, &vars[i]);
            out.push_str("\":");
            json_term(&mut out, t);
        }
        out.push('}');
    }
    out.push_str("]}}");
    out
}

/// SPARQL 1.1 JSON results document for ASK.
pub fn ask_json(result: bool) -> String {
    format!("{{\"head\":{{}},\"boolean\":{result}}}")
}

/// One solution as a flat {"var":"lexical value"} JSON object (the sparql()
/// table function's binding column).
pub fn binding_object(vars: &[String], row: &[Option<Term>]) -> String {
    let mut out = String::from("{");
    let mut first = true;
    for (i, cell) in row.iter().enumerate() {
        let Some(t) = cell else { continue };
        if !first {
            out.push(',');
        }
        first = false;
        out.push('"');
        json_escape(&mut out, &vars[i]);
        out.push_str("\":\"");
        json_escape(&mut out, &t.lex);
        out.push('"');
    }
    out.push('}');
    out
}

fn local_ok_for_prefix(rest: &str) -> bool {
    if rest.starts_with('.') || rest.ends_with('.') {
        return false;
    }
    rest.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn turtle_write_iri(out: &mut String, iri: &str, prefixes: &[(String, String)]) {
    let mut best: Option<(&str, usize)> = None;
    for (name, piri) in prefixes {
        if iri.starts_with(piri.as_str())
            && piri.len() > best.map_or(0, |(_, l)| l)
            && local_ok_for_prefix(&iri[piri.len()..])
        {
            best = Some((name, piri.len()));
        }
    }
    match best {
        Some((name, len)) => out.push_str(&format!("{}:{}", name, &iri[len..])),
        None => out.push_str(&format!("<{iri}>")),
    }
}

fn write_quoted_literal(out: &mut String, lex: &str) {
    out.push('"');
    for ch in lex.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn lex_is_integer(s: &str) -> bool {
    let d = s.strip_prefix(['+', '-']).unwrap_or(s);
    !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
}

fn lex_is_decimal(s: &str) -> bool {
    let d = s.strip_prefix(['+', '-']).unwrap_or(s);
    let Some((int, frac)) = d.split_once('.') else {
        return false;
    };
    !frac.is_empty()
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit())
}

fn turtle_write_term(out: &mut String, t: &Term, prefixes: &[(String, String)]) {
    match t.kind {
        K_IRI => turtle_write_iri(out, &t.lex, prefixes),
        K_BNODE => out.push_str(&format!("_:{}", t.lex)),
        _ => {
            let dt = if t.dtype.is_empty() {
                None
            } else {
                Some(t.dtype.as_str())
            };
            let lg = if t.lang.is_empty() {
                None
            } else {
                Some(t.lang.as_str())
            };
            if let (Some(dt), None) = (dt, lg) {
                if dt == format!("{XSD}integer") && lex_is_integer(&t.lex) {
                    out.push_str(&t.lex);
                    return;
                }
                if dt == format!("{XSD}decimal") && lex_is_decimal(&t.lex) {
                    out.push_str(&t.lex);
                    return;
                }
                if dt == format!("{XSD}boolean") && (t.lex == "true" || t.lex == "false") {
                    out.push_str(&t.lex);
                    return;
                }
            }
            write_quoted_literal(out, &t.lex);
            if let Some(lg) = lg {
                out.push_str(&format!("@{lg}"));
            } else if let Some(dt) = dt {
                out.push_str("^^");
                turtle_write_iri(out, dt, prefixes);
            }
        }
    }
}

/// Serialize triples as Turtle, sorted, grouped with ';' and ','.
pub fn serialize_turtle(triples: &mut [[Term; 3]], prefixes: &[(String, String)]) -> String {
    let mut out = String::new();
    for (name, iri) in prefixes {
        out.push_str(&format!("@prefix {name}: <{iri}> .\n"));
    }
    if !prefixes.is_empty() {
        out.push('\n');
    }
    triples.sort();
    for i in 0..triples.len() {
        let cur = &triples[i];
        let same_subj = i > 0 && cur[0] == triples[i - 1][0];
        let same_pred = same_subj && cur[1] == triples[i - 1][1];
        if !same_subj {
            if i > 0 {
                out.push_str(" .\n");
            }
            turtle_write_term(&mut out, &cur[0], prefixes);
            out.push(' ');
            turtle_write_term(&mut out, &cur[1], prefixes);
            out.push(' ');
            turtle_write_term(&mut out, &cur[2], prefixes);
        } else if !same_pred {
            out.push_str(" ;\n    ");
            turtle_write_term(&mut out, &cur[1], prefixes);
            out.push(' ');
            turtle_write_term(&mut out, &cur[2], prefixes);
        } else {
            out.push_str(", ");
            turtle_write_term(&mut out, &cur[2], prefixes);
        }
    }
    if !triples.is_empty() {
        out.push_str(" .\n");
    }
    out
}

fn nt_write_term(out: &mut String, t: &Term) {
    match t.kind {
        K_IRI => out.push_str(&format!("<{}>", t.lex)),
        K_BNODE => out.push_str(&format!("_:{}", t.lex)),
        _ => {
            write_quoted_literal(out, &t.lex);
            if !t.lang.is_empty() {
                out.push_str(&format!("@{}", t.lang));
            } else if !t.dtype.is_empty() {
                out.push_str(&format!("^^<{}>", t.dtype));
            }
        }
    }
}

/// Serialize triples as N-Triples, sorted.
pub fn serialize_ntriples(triples: &mut [[Term; 3]]) -> String {
    triples.sort();
    let mut out = String::new();
    for t in triples.iter() {
        nt_write_term(&mut out, &t[0]);
        out.push(' ');
        nt_write_term(&mut out, &t[1]);
        out.push(' ');
        nt_write_term(&mut out, &t[2]);
        out.push_str(" .\n");
    }
    out
}
