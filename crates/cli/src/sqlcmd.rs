//! `rsntr sql`: run owner-channel SQL from an argument or a file.
//!
//! The generic scaffolding runner example mods document (e.g.
//! examples/shop-mod/seed.sql): each statement rides the owner channel,
//! writes as Executes (DDL permitted, everything lands in `_audit`) and
//! reads as Queries whose rows come back in the report. Input is split
//! on the statement separator, skipping semicolons inside string
//! literals, quoted identifiers, and comments - a media type like
//! `'audio/L16;rate=8000'` is one value, not three statements.

use std::path::Path;

use anyhow::{Context, Result, bail};

use resonator_protocol::{Done, RequestKind, Row};

use crate::channel::{self, OwnerChannel, Prefer};
use crate::client::classify_sql;

/// What a run applied, for the human and `--json` reports.
#[derive(Debug)]
pub struct SqlOutcome {
    pub statements: usize,
    pub affected: i64,
    /// The last read statement's result, when any statement was a read:
    /// `(columns, rows, done)` — the same shapes `rsntr query` reports.
    pub rows: Option<(Vec<String>, Vec<Row>, Done)>,
}

/// Splits SQL on statement separators, ignoring semicolons inside
/// `'literals'`, `"identifiers"`, `` `identifiers` ``, `[identifiers]`,
/// `-- line comments`, and `/* block comments */`. Doubled quotes are
/// the SQL escape and stay inside their run.
fn split_statements(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ';' => {
                out.push(std::mem::take(&mut cur));
            }
            '\'' | '"' | '`' => {
                let close = c;
                cur.push(c);
                while let Some(q) = chars.next() {
                    cur.push(q);
                    if q == close {
                        // A doubled quote escapes itself; stay inside.
                        if chars.peek() == Some(&close) {
                            cur.push(chars.next().expect("peeked"));
                            continue;
                        }
                        break;
                    }
                }
            }
            '[' => {
                cur.push(c);
                for q in chars.by_ref() {
                    cur.push(q);
                    if q == ']' {
                        break;
                    }
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                cur.push(c);
                for q in chars.by_ref() {
                    cur.push(q);
                    if q == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                cur.push(c);
                cur.push(chars.next().expect("peeked"));
                let mut prev = '\0';
                for q in chars.by_ref() {
                    cur.push(q);
                    if prev == '*' && q == '/' {
                        break;
                    }
                    prev = q;
                }
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Splits, then runs every statement over one owner channel: reads as
/// Queries (their rows reported; the last read wins), writes as
/// Executes.
pub fn run_sql(dir: &Path, source: &str, prefer: Prefer) -> Result<SqlOutcome> {
    let split = split_statements(source);
    let statements: Vec<&str> = split
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !comment_only(s))
        .collect();
    if statements.is_empty() {
        bail!("no SQL statements in the input");
    }
    let count = statements.len();
    let (affected, rows) = channel::block_on(async move {
        let ch = OwnerChannel::open(dir, prefer).await?;
        let mut affected = 0i64;
        let mut last_read = None;
        for stmt in &statements {
            if classify_sql(stmt) == RequestKind::Query {
                let read = channel::query_rows(&ch, stmt, vec![])
                    .await
                    .with_context(|| format!("statement failed: {}", first_line(stmt)))?;
                last_read = Some(read);
            } else {
                let done = channel::execute(&ch, stmt, vec![])
                    .await
                    .with_context(|| format!("statement failed: {}", first_line(stmt)))?;
                affected += done.affected_rows.unwrap_or(0);
            }
        }
        Ok::<_, anyhow::Error>((affected, last_read))
    })??;
    Ok(SqlOutcome {
        statements: count,
        affected,
        rows,
    })
}

/// True when a split segment holds only `--` comments and whitespace
/// (e.g. a seed file's trailing comment block).
fn comment_only(segment: &str) -> bool {
    segment
        .lines()
        .all(|l| l.trim().is_empty() || l.trim_start().starts_with("--"))
}

/// The first non-comment line of a statement, for error messages.
fn first_line(stmt: &str) -> String {
    stmt.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("--"))
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_only_segments_are_skipped() {
        assert!(comment_only("-- just a comment\n  -- another"));
        assert!(comment_only("  \n"));
        assert!(!comment_only("-- lead-in\nCREATE TABLE t (x)"));
    }

    #[test]
    fn semicolons_inside_literals_do_not_split() {
        let sql = "INSERT INTO _media (name, accepts) VALUES \
                   ('door-talk', 'audio/L16;rate=8000;channels=1'); SELECT 1";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(parts[0].contains("audio/L16;rate=8000;channels=1"));
        assert_eq!(parts[1].trim(), "SELECT 1");
    }

    #[test]
    fn quotes_comments_and_escapes_are_respected() {
        let sql = "SELECT 'it''s; fine', \"odd;name\" -- trailing; comment\n; \
                   /* block; comment */ SELECT 2";
        let parts: Vec<String> = split_statements(sql)
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect();
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(parts[0].contains("it''s; fine"));
        assert!(parts[1].contains("SELECT 2"));
    }

    #[test]
    fn first_line_skips_comments() {
        assert_eq!(
            first_line("-- what this does\nINSERT INTO t VALUES (1)"),
            "INSERT INTO t VALUES (1)"
        );
    }
}
