//! Statement fingerprints for the decision cache.
//!
//! A fingerprint keys one *shape* of request: the normalized statement
//! (literals blanked, case and whitespace folded) plus the action string
//! and the footprint. Bound parameters never appear in the statement
//! text and inline literals are blanked, so `?`-parameterized repeats
//! and literal-value variants of the same statement hit the same cache
//! row, while any structural change (different columns, tables, or
//! clauses) misses.

use sha2::{Digest, Sha256};

use crate::types::Footprint;

/// The normalized shape of one SQL/signal text: string and numeric
/// literals become `?`, ASCII case is folded, whitespace runs collapse
/// to one space, trailing semicolons drop.
pub fn normalize_statement(statement: &str) -> String {
    let mut out = String::with_capacity(statement.len());
    let bytes = statement.as_bytes();
    let mut i = 0;
    // Tracks whether the previous emitted byte continues an identifier,
    // so `col1` is not mistaken for the number `1`.
    let mut in_word = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '\'' => {
                // String literal; '' is the escaped quote.
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        if bytes.get(i + 1) == Some(&b'\'') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                out.push('?');
                in_word = false;
            }
            '"' | '`' => {
                // Quoted identifier: part of the shape, kept (folded).
                let quote = bytes[i];
                out.push(c);
                i += 1;
                while i < bytes.len() {
                    let b = bytes[i];
                    out.push((b as char).to_ascii_lowercase());
                    i += 1;
                    if b == quote {
                        break;
                    }
                }
                in_word = true;
            }
            '0'..='9' if !in_word => {
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_')
                {
                    i += 1;
                }
                out.push('?');
                in_word = false;
            }
            c if c.is_whitespace() => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                while i < bytes.len() && (bytes[i] as char).is_whitespace() {
                    i += 1;
                }
                in_word = false;
            }
            c => {
                out.push(c.to_ascii_lowercase());
                in_word = c.is_ascii_alphanumeric() || c == '_';
                i += 1;
            }
        }
    }
    while out.ends_with(' ') || out.ends_with(';') {
        out.pop();
    }
    out
}

/// The cache key for one request shape: hex sha256 over the action, the
/// normalized statement, and the footprint (kind + tables/columns).
/// The action is folded in so footprint-less actions (knock, media,
/// entrain) cannot collide with each other.
pub fn statement_fingerprint(action: &str, statement: &str, footprint: &Footprint) -> String {
    let mut hasher = Sha256::new();
    hasher.update(action.as_bytes());
    hasher.update([0u8]);
    hasher.update(normalize_statement(statement).as_bytes());
    hasher.update([0u8]);
    hasher.update(footprint.kind.as_str().as_bytes());
    // BTreeMap/BTreeSet iterate sorted: the encoding is canonical.
    for (table, cols) in &footprint.tables {
        hasher.update([0u8]);
        hasher.update(table.as_bytes());
        for col in cols {
            hasher.update([1u8]);
            hasher.update(col.as_bytes());
        }
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ActionKind;

    fn fp(tables: &[(&str, &[&str])]) -> Footprint {
        Footprint::from_tables(
            ActionKind::Read,
            tables.iter().map(|(t, cols)| (*t, cols.iter().copied())),
        )
    }

    #[test]
    fn normalization_blanks_literals_and_folds() {
        assert_eq!(
            normalize_statement("SELECT * FROM notes WHERE id = 42;"),
            "select * from notes where id = ?"
        );
        assert_eq!(
            normalize_statement("select  *\nfrom NOTES where name = 'Ada''s'"),
            "select * from notes where name = ?"
        );
        // Identifiers with digits are not literals.
        assert_eq!(
            normalize_statement("SELECT col1 FROM t2"),
            "select col1 from t2"
        );
    }

    #[test]
    fn fingerprint_ignores_params_but_not_shape() {
        let f = fp(&[("notes", &["id", "body"])]);
        let a = statement_fingerprint("read", "SELECT body FROM notes WHERE id = 1", &f);
        let b = statement_fingerprint("read", "SELECT body FROM notes WHERE id = 2", &f);
        let c = statement_fingerprint("read", "SELECT body FROM notes WHERE id = 'x'", &f);
        assert_eq!(a, b);
        assert_eq!(a, c);

        let d = statement_fingerprint("read", "SELECT id FROM notes WHERE id = 1", &f);
        assert_ne!(a, d, "a different column list is a different shape");
    }

    #[test]
    fn fingerprint_covers_footprint_and_action() {
        let sql = "SELECT * FROM notes";
        let a = statement_fingerprint("read", sql, &fp(&[("notes", &["id"])]));
        let b = statement_fingerprint("read", sql, &fp(&[("notes", &["id", "body"])]));
        assert_ne!(a, b, "the footprint is part of the key");

        let empty = Footprint::default();
        let knock = statement_fingerprint("knock", "", &empty);
        let media = statement_fingerprint("media", "", &empty);
        assert_ne!(knock, media, "footprint-less actions must not collide");
    }
}
