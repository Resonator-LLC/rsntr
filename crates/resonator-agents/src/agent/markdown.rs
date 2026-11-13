//! Simple Markdown, as models write it, mapped onto Wave: headings and
//! bullets become line types, bold / italic / code / links become
//! annotations, paragraphs become lines.
//!
//! The mapping is all or nothing per construct: a delimiter without its
//! partner on the same line stays as it is, so no half-converted markup is
//! ever left. Numbered list items keep their numbers as text (the Robots
//! API cannot set Wave's list style); a heading or bullet on the first
//! line of a blip, whose line element cannot be changed, is shown bold or
//! with a bullet character.

use resonator_robots::api::{Annotation, OperationRequest};
use serde_json::{Value, json};

/// A stretch of text with one style.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<String>,
}

impl Run {
    fn plain(text: &str) -> Run {
        Run {
            text: text.to_owned(),
            ..Run::default()
        }
    }

    fn same_style(&self, other: &Run) -> bool {
        self.bold == other.bold
            && self.italic == other.italic
            && self.code == other.code
            && self.link == other.link
    }

    /// The annotations of the run, for `bundled_annotations`.
    pub fn annotations(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if self.bold {
            out.push((Annotation::FONT_WEIGHT, "bold".to_owned()));
        }
        if self.italic {
            out.push((Annotation::FONT_STYLE, "italic".to_owned()));
        }
        if self.code {
            out.push((Annotation::FONT_FAMILY, "monospace".to_owned()));
        }
        if let Some(link) = &self.link {
            out.push((Annotation::LINK, link.clone()));
        }
        out
    }
}

/// One Wave line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Line {
    /// `h1`…`h4` or `li`.
    pub line_type: Option<&'static str>,
    pub indent: u32,
    pub runs: Vec<Run>,
}

impl Line {
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// Whether a text has anything [`to_lines`] would change.
pub fn has_markup(text: &str) -> bool {
    text.contains(['*', '_', '`', '#', '[', '-', '+', '>'])
        || text.contains("http://")
        || text.contains("https://")
}

fn push(runs: &mut Vec<Run>, run: Run) {
    if run.text.is_empty() {
        return;
    }
    match runs.last_mut() {
        Some(last) if last.same_style(&run) => last.text.push_str(&run.text),
        _ => runs.push(run),
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

/// Parses inline markup of one line.
pub fn inline(text: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    parse_inline(text, &Run::default(), &mut runs, 0);
    runs
}

fn parse_inline(text: &str, style: &Run, out: &mut Vec<Run>, depth: usize) {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut plain_start = 0usize;
    let mut i = 0usize;
    let flush = |out: &mut Vec<Run>, from: usize, to: usize| {
        if from < to {
            push(
                out,
                Run {
                    text: text[from..to].to_owned(),
                    ..style.clone()
                },
            );
        }
    };
    while i < chars.len() {
        let (at, c) = chars[i];
        let rest = &text[at..];
        // Inline code.
        if c == '`'
            && let Some(close) = rest[1..].find('`')
            && close > 0
        {
            flush(out, plain_start, at);
            push(
                out,
                Run {
                    text: rest[1..1 + close].to_owned(),
                    code: true,
                    ..style.clone()
                },
            );
            let end = at + close + 2;
            plain_start = end;
            while i < chars.len() && chars[i].0 < end {
                i += 1;
            }
            continue;
        }
        // Links [text](url).
        if c == '['
            && let Some(close) = rest.find("](")
            && let Some(paren) = rest[close + 2..].find(')')
        {
            let label = &rest[1..close];
            let url = rest[close + 2..close + 2 + paren].trim();
            if !label.is_empty()
                && !label.contains('[')
                && (url.starts_with("http://")
                    || url.starts_with("https://")
                    || url.starts_with("mailto:"))
                && !url.contains(char::is_whitespace)
            {
                flush(out, plain_start, at);
                let linked = Run {
                    link: Some(url.to_owned()),
                    ..style.clone()
                };
                if depth < 4 {
                    parse_inline(label, &linked, out, depth + 1);
                } else {
                    push(
                        out,
                        Run {
                            text: label.to_owned(),
                            ..linked
                        },
                    );
                }
                let end = at + close + 2 + paren + 1;
                plain_start = end;
                while i < chars.len() && chars[i].0 < end {
                    i += 1;
                }
                continue;
            }
        }
        // Bare URLs.
        if style.link.is_none() && (rest.starts_with("https://") || rest.starts_with("http://")) {
            let len = rest
                .find(|c: char| c.is_whitespace() || c == ')' || c == '>' || c == '"')
                .unwrap_or(rest.len());
            let url = rest[..len].trim_end_matches(['.', ',', ';', ':', '!', '?']);
            if url.len() > "https://".len() {
                flush(out, plain_start, at);
                push(
                    out,
                    Run {
                        text: url.to_owned(),
                        link: Some(url.to_owned()),
                        ..style.clone()
                    },
                );
                let end = at + url.len();
                plain_start = end;
                while i < chars.len() && chars[i].0 < end {
                    i += 1;
                }
                continue;
            }
        }
        // Emphasis.
        if (c == '*' || c == '_') && depth < 4 {
            let double = rest.starts_with(if c == '*' { "**" } else { "__" });
            let delimiter = if double { &rest[..2] } else { &rest[..1] };
            let before_ok = c == '*' || at == 0 || !text[..at].chars().last().is_some_and(is_word);
            let inner_start = delimiter.len();
            let opens = rest[inner_start..]
                .chars()
                .next()
                .is_some_and(|n| !n.is_whitespace());
            if before_ok
                && opens
                && let Some(close) = find_closing(&rest[inner_start..], delimiter, c)
            {
                let inner = &rest[inner_start..inner_start + close];
                flush(out, plain_start, at);
                let styled = Run {
                    bold: style.bold || double,
                    italic: style.italic || !double,
                    ..style.clone()
                };
                parse_inline(inner, &styled, out, depth + 1);
                let end = at + inner_start + close + delimiter.len();
                plain_start = end;
                while i < chars.len() && chars[i].0 < end {
                    i += 1;
                }
                continue;
            }
        }
        i += 1;
    }
    flush(out, plain_start, text.len());
}

/// Where the closing delimiter is in `text` (after the opening one): not
/// preceded by whitespace, and for `_` not followed by a word character.
fn find_closing(text: &str, delimiter: &str, c: char) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = text[from..].find(delimiter) {
        let at = from + found;
        let before = text[..at].chars().last();
        let after = text[at + delimiter.len()..].chars().next();
        let closes = at > 0
            && before.is_some_and(|b| !b.is_whitespace())
            && (c == '*' || !after.is_some_and(is_word))
            // `**` must not be read as the closing of `*`.
            && !(delimiter.len() == 1 && after == Some(c));
        if closes {
            // `***` closing `**`: the delimiter is the last two of the run.
            let run = text[at..].chars().take_while(|&r| r == c).count();
            return Some(at + run.saturating_sub(delimiter.len()));
        }
        from = at + delimiter.len();
    }
    None
}

/// Splits Markdown into Wave lines.
pub fn to_lines(markdown: &str) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut in_code = false;
    let mut blank = false;
    for raw in markdown.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            if blank && !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.push(Line {
                runs: vec![Run {
                    text: raw.trim_end().to_owned(),
                    code: true,
                    ..Run::default()
                }],
                ..Line::default()
            });
            blank = false;
            continue;
        }
        if trimmed.is_empty() {
            blank = true;
            continue;
        }
        let leading = raw.len() - raw.trim_start().len();
        let mut line = Line::default();
        if let Some(rest) = heading(trimmed) {
            line.line_type = Some(rest.0);
            line.runs = inline(rest.1);
        } else if let Some(rest) = bullet(trimmed) {
            line.line_type = Some("li");
            line.indent = (leading / 2) as u32;
            line.runs = inline(rest);
        } else if trimmed
            .chars()
            .all(|c| c == '-' || c == '*' || c == '_' || c == ' ')
            && trimmed.chars().filter(|c| !c.is_whitespace()).count() >= 3
        {
            // A horizontal rule: a paragraph break.
            blank = true;
            continue;
        } else if let Some(rest) = trimmed.strip_prefix('>') {
            line.indent = 1;
            line.runs = inline(rest.trim_start());
        } else {
            line.indent = if numbered(trimmed) {
                (leading / 2) as u32
            } else {
                0
            };
            line.runs = inline(trimmed);
        }
        // One empty line between paragraphs, none between list items.
        let list = line.line_type == Some("li") || numbered(trimmed);
        let previous_list = lines
            .last()
            .is_some_and(|l| l.line_type == Some("li") || numbered(&l.text()));
        if blank && !lines.is_empty() && !(list && previous_list) {
            lines.push(Line::default());
        }
        blank = false;
        lines.push(line);
    }
    lines
}

fn heading(line: &str) -> Option<(&'static str, &str)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = line[hashes..].strip_prefix(' ')?;
    let kind = match hashes {
        1 => "h1",
        2 => "h2",
        3 => "h3",
        _ => "h4",
    };
    Some((kind, rest.trim().trim_end_matches('#').trim_end()))
}

fn bullet(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest.trim_start());
        }
    }
    None
}

fn numbered(line: &str) -> bool {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    digits > 0
        && digits < 4
        && (line[digits..].starts_with(". ") || line[digits..].starts_with(") "))
}

/// The operations that turn a blip of the API text length `length` into
/// the formatted lines: delete everything after the first line break,
/// then append line by line, run by run.
pub fn operations(
    lines: &[Line],
    wave: &str,
    wavelet: &str,
    blip: &str,
    length: usize,
) -> Vec<OperationRequest> {
    let mut ops: Vec<Value> = Vec::new();
    let params = |extra: Value| -> Value {
        let mut base = json!({"waveId": wave, "waveletId": wavelet, "blipId": blip});
        if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }
        base
    };
    if length > 1 {
        ops.push(
            json!({"id": "md-delete", "method": "document.modify", "params": params(json!({
                "range": {"start": 1, "end": length},
                "modifyAction": {"modifyHow": "DELETE"}
            }))}),
        );
    }
    for (index, line) in lines.iter().enumerate() {
        let mut runs = line.runs.clone();
        if index > 0 {
            let mut properties = serde_json::Map::new();
            if let Some(kind) = line.line_type {
                properties.insert("lineType".to_owned(), json!(kind));
            }
            if line.indent > 0 {
                properties.insert("indent".to_owned(), json!(line.indent.to_string()));
            }
            ops.push(json!({"id": format!("md-l{index}"), "method": "document.modify", "params": params(json!({
                "modifyAction": {"modifyHow": "INSERT_AFTER",
                                 "elements": [{"type": "LINE", "properties": properties}]}
            }))}));
        } else {
            match line.line_type {
                Some("li") => runs.insert(0, Run::plain("• ")),
                Some(_) => {
                    for run in &mut runs {
                        run.bold = true;
                    }
                }
                None => {}
            }
        }
        for (r, run) in runs.iter().enumerate() {
            let bundled: Vec<Value> = run
                .annotations()
                .into_iter()
                .map(|(key, value)| json!({"key": key, "value": value}))
                .collect();
            ops.push(json!({"id": format!("md-{index}-{r}"), "method": "document.modify", "params": params(json!({
                "modifyAction": {"modifyHow": "INSERT_AFTER", "values": [run.text],
                                 "bundledAnnotations": bundled}
            }))}));
        }
    }
    ops.iter()
        .filter_map(|op| OperationRequest::from_value(op).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(text: &str) -> Vec<(String, &'static str)> {
        inline(text)
            .into_iter()
            .map(|run| {
                let style = match (run.bold, run.italic, run.code, run.link.is_some()) {
                    (true, true, _, _) => "bi",
                    (true, _, _, _) => "b",
                    (_, true, _, _) => "i",
                    (_, _, true, _) => "c",
                    (_, _, _, true) => "a",
                    _ => "",
                };
                (run.text, style)
            })
            .collect()
    }

    fn s(text: &str, style: &'static str) -> (String, &'static str) {
        (text.to_owned(), style)
    }

    #[test]
    fn inline_styles() {
        assert_eq!(
            styled("a **bold** and *it* and `co*de*`"),
            vec![
                s("a ", ""),
                s("bold", "b"),
                s(" and ", ""),
                s("it", "i"),
                s(" and ", ""),
                s("co*de*", "c")
            ]
        );
        assert_eq!(
            styled("__b__ _i_"),
            vec![s("b", "b"), s(" ", ""), s("i", "i")]
        );
        assert_eq!(
            styled("**bold *both***"),
            vec![s("bold ", "b"), s("both", "bi")]
        );
        let runs = inline("see [the docs](https://x.org/a) or https://y.org/b.");
        assert_eq!(runs[1].text, "the docs");
        assert_eq!(runs[1].link.as_deref(), Some("https://x.org/a"));
        assert_eq!(runs[3].text, "https://y.org/b");
        assert_eq!(runs[4].text, ".");
    }

    #[test]
    fn unmatched_markup_is_left_alone() {
        for text in [
            "2 * 3 = 6",
            "**not closed",
            "snake_case_name",
            "a * b * c",
            "`open",
            "[label](not a url)",
            "price: 5*",
        ] {
            assert_eq!(
                inline(text),
                vec![Run::plain(text)],
                "{text}: {:?}",
                inline(text)
            );
        }
    }

    #[test]
    fn lines_and_blocks() {
        let lines = to_lines(
            "# Title\n\nSome *text*.\nMore.\n\n- one\n- two\n  - nested\n\n1. first\n2. second\n\n```\nlet x = 1;\n```\n> quoted\n---\nend",
        );
        let summary: Vec<(Option<&str>, u32, String)> = lines
            .iter()
            .map(|l| (l.line_type, l.indent, l.text()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (Some("h1"), 0, "Title".to_owned()),
                (None, 0, String::new()),
                (None, 0, "Some text.".to_owned()),
                (None, 0, "More.".to_owned()),
                (None, 0, String::new()),
                (Some("li"), 0, "one".to_owned()),
                (Some("li"), 0, "two".to_owned()),
                (Some("li"), 1, "nested".to_owned()),
                (None, 0, "1. first".to_owned()),
                (None, 0, "2. second".to_owned()),
                (None, 0, String::new()),
                (None, 0, "let x = 1;".to_owned()),
                (None, 1, "quoted".to_owned()),
                (None, 0, String::new()),
                (None, 0, "end".to_owned()),
            ]
        );
        assert!(lines[11].runs[0].code);
    }

    #[test]
    fn operations_rebuild_the_blip() {
        let lines = to_lines("## Hi\n**x**");
        let ops = operations(&lines, "example.com/w+a", "example.com/conv+root", "b+1", 7);
        let methods: Vec<String> = ops
            .iter()
            .map(|op| op.to_value()["method"].to_string())
            .collect();
        assert_eq!(methods.len(), 4, "{methods:?}");
        let values: Vec<Value> = ops.iter().map(OperationRequest::to_value).collect();
        assert_eq!(values[0]["params"]["range"]["end"], 7);
        assert_eq!(values[1]["params"]["modifyAction"]["values"][0], "Hi");
        assert_eq!(
            values[1]["params"]["modifyAction"]["bundledAnnotations"][0]["value"],
            "bold"
        );
        assert_eq!(
            values[2]["params"]["modifyAction"]["elements"][0]["type"],
            "LINE"
        );
        assert!(!has_markup("plain words only"));
        assert!(has_markup("a **b**"));
    }
}
