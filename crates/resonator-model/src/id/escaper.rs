//! Prefix escaping of separator characters in ids (`SimplePrefixEscaper`).

use std::borrow::Cow;

use thiserror::Error;

/// Failure to escape, unescape, join or split (an
/// `IllegalArgumentException` in the original).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct EscapeError(pub String);

/// Escapes a set of characters by prefixing them with a prefix character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimplePrefixEscaper {
    prefix: char,
    /// Always contains the prefix itself.
    needs_escaping: Cow<'static, [char]>,
}

/// The escaper used for ids: prefix `~`, escaping `~`, `+` and `!`.
pub static DEFAULT_ESCAPER: SimplePrefixEscaper = SimplePrefixEscaper {
    prefix: '~',
    needs_escaping: Cow::Borrowed(&['~', '+', '!']),
};

impl SimplePrefixEscaper {
    pub fn new(prefix: char, needs_escaping: &[char]) -> Self {
        let mut all = vec![prefix];
        all.extend_from_slice(needs_escaping);
        SimplePrefixEscaper {
            prefix,
            needs_escaping: Cow::Owned(all),
        }
    }

    fn needs_escaping(&self, c: char) -> bool {
        self.needs_escaping.contains(&c)
    }

    pub fn escape(&self, to_escape: &str) -> String {
        let mut out = String::with_capacity(to_escape.len());
        for c in to_escape.chars() {
            if self.needs_escaping(c) {
                out.push(self.prefix);
            }
            out.push(c);
        }
        out
    }

    pub fn unescape(&self, to_unescape: &str) -> Result<String, EscapeError> {
        let mut out = String::with_capacity(to_unescape.len());
        let mut chars = to_unescape.chars();
        while let Some(c) = chars.next() {
            if c == self.prefix {
                match chars.next() {
                    None => {
                        return Err(EscapeError(format!(
                            "The value to unescape cannot be terminated with the prefix: {}",
                            self.prefix
                        )));
                    }
                    Some(next) if !self.needs_escaping(next) => {
                        return Err(EscapeError(format!(
                            "The value to unescape is not a properly escaped value. The prefix \
                             charater is not followed by a character at needs prefixing: \
                             {to_unescape}"
                        )));
                    }
                    Some(next) => out.push(next),
                }
            } else if self.needs_escaping(c) {
                return Err(EscapeError(format!(
                    "The value to unescape is not a properly escaped value. Some chars are \
                     found unescaped: {to_unescape}"
                )));
            } else {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// Escapes the tokens and joins them with `separator`, which must be an
    /// escaped character other than the prefix.
    pub fn join(&self, separator: char, tokens: &[&str]) -> Result<String, EscapeError> {
        if separator == self.prefix {
            return Err(EscapeError(
                "It's unsafe to join strings together using the prefixchar.".to_owned(),
            ));
        }
        // "" is then unambiguously the join of a single empty token.
        if tokens.is_empty() {
            return Err(EscapeError(
                "Must have at least 1 token to use join.".to_owned(),
            ));
        }
        if !self.needs_escaping(separator) {
            return Err(EscapeError(format!(
                "It's unsafe to join strings together using a [separator:{separator}] that is \
                 not in the characters that are escaped."
            )));
        }
        let mut out = String::new();
        for (i, token) in tokens.iter().enumerate() {
            if i > 0 {
                out.push(separator);
            }
            out.push_str(&self.escape(token));
        }
        Ok(out)
    }

    /// Splits on unescaped separators and unescapes each token.
    pub fn split(&self, separator: char, to_split: &str) -> Result<Vec<String>, EscapeError> {
        self.split_without_unescaping(separator, to_split)?
            .into_iter()
            .map(|token| self.unescape(token))
            .collect()
    }

    /// Splits on unescaped separators, leaving the tokens escaped.
    pub fn split_without_unescaping<'a>(
        &self,
        separator: char,
        to_split: &'a str,
    ) -> Result<Vec<&'a str>, EscapeError> {
        if separator == self.prefix {
            return Err(EscapeError(
                "It's unsafe to split strings together the prefix char.".to_owned(),
            ));
        }
        let mut out = Vec::new();
        let mut start = 0;
        let mut chars = to_split.char_indices();
        while let Some((i, c)) = chars.next() {
            if c == separator {
                out.push(&to_split[start..i]);
                start = i + c.len_utf8();
            } else if c == self.prefix {
                // Skip over the escaped char.
                chars.next();
            }
        }
        out.push(&to_split[start..]);
        Ok(out)
    }

    /// Whether every escaped character other than `separator` is preceded by
    /// the prefix.
    pub fn is_escaped_properly(&self, separator: char, escaped_value: &str) -> bool {
        let mut chars = escaped_value.chars();
        while let Some(c) = chars.next() {
            if c == self.prefix {
                match chars.next() {
                    Some(next) if self.needs_escaping(next) => {}
                    _ => return false,
                }
            } else if c != separator && self.needs_escaping(c) {
                return false;
            }
        }
        true
    }

    /// Whether `unescaped` contains a character that would be escaped.
    pub fn has_escape_characters(&self, unescaped: &str) -> bool {
        unescaped.chars().any(|c| self.needs_escaping(c))
    }
}
