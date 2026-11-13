//! The search query language.
//!
//! Derived from `QueryHelper` and `TokenQueryType` in
//! `org.waveprotocol.box.server.waveserver` of Apache Wave, licensed under
//! the Apache License, Version 2.0.
//!
//! A query is a list of whitespace-separated tokens. `key:value` tokens
//! are the structured part, exactly as in the original: the keys are `in`,
//! `orderby`, `with`, `creator` and `id`, the same key may be given several
//! times, and a token with a colon that is not of that form (an unknown
//! key, no value, more than one colon) makes the whole query invalid.
//!
//! Tokens without a colon are free-text terms, which the original's
//! in-memory search provider rejects as invalid and its Solr provider hands
//! to the full-text index. A term may end in `*` for a prefix match, and a
//! double-quoted string (which may contain spaces and colons) is one
//! phrase.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use resonator_model::id::{InvalidParticipantAddress, ParticipantId, WaveId};
use thiserror::Error;

/// The keys of `key:value` tokens (`TokenQueryType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TokenQueryType {
    In,
    OrderBy,
    With,
    Creator,
    Id,
}

impl TokenQueryType {
    pub const ALL: [TokenQueryType; 5] = [
        TokenQueryType::In,
        TokenQueryType::OrderBy,
        TokenQueryType::With,
        TokenQueryType::Creator,
        TokenQueryType::Id,
    ];

    pub fn token(self) -> &'static str {
        match self {
            TokenQueryType::In => "in",
            TokenQueryType::OrderBy => "orderby",
            TokenQueryType::With => "with",
            TokenQueryType::Creator => "creator",
            TokenQueryType::Id => "id",
        }
    }

    /// `TokenQueryType.fromToken`; `None` where the original throws.
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.token() == token)
    }

    /// `TokenQueryType.hasToken`.
    pub fn has_token(token: &str) -> bool {
        Self::from_token(token).is_some()
    }
}

/// The values of `orderby:` (`QueryHelper.OrderByValueType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderBy {
    DateAsc,
    DateDesc,
    CreatedAsc,
    CreatedDesc,
    CreatorAsc,
    CreatorDesc,
}

impl OrderBy {
    pub const ALL: [OrderBy; 6] = [
        OrderBy::DateAsc,
        OrderBy::DateDesc,
        OrderBy::CreatedAsc,
        OrderBy::CreatedDesc,
        OrderBy::CreatorAsc,
        OrderBy::CreatorDesc,
    ];

    pub fn token(self) -> &'static str {
        match self {
            OrderBy::DateAsc => "dateasc",
            OrderBy::DateDesc => "datedesc",
            OrderBy::CreatedAsc => "createdasc",
            OrderBy::CreatedDesc => "createddesc",
            OrderBy::CreatorAsc => "creatorasc",
            OrderBy::CreatorDesc => "creatordesc",
        }
    }

    /// `OrderByValueType.fromToken`; `None` where the original throws.
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|order| order.token() == token)
    }
}

/// A query that cannot be parsed (`QueryHelper.InvalidQueryException`).
/// A search with such a query finds nothing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct InvalidQuery(pub String);

/// A free-text term or phrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextTerm {
    pub text: String,
    /// Whether words starting with the term match (`term*`).
    pub prefix: bool,
}

/// A parsed query.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedQuery {
    /// The values given for each key, without duplicates, in the order
    /// they were given (the map of `QueryHelper.parseQuery`).
    pub tokens: BTreeMap<TokenQueryType, Vec<String>>,
    /// The free-text terms; a wave must match all of them.
    pub text: Vec<TextTerm>,
}

impl ParsedQuery {
    pub fn values(&self, kind: TokenQueryType) -> &[String] {
        self.tokens.get(&kind).map_or(&[], Vec::as_slice)
    }

    pub fn has(&self, kind: TokenQueryType) -> bool {
        self.tokens.contains_key(&kind)
    }

    /// The orderings asked for, the primary one first; empty for the
    /// default.
    pub fn orderings(&self) -> Vec<OrderBy> {
        self.values(TokenQueryType::OrderBy)
            .iter()
            .filter_map(|value| OrderBy::from_token(value))
            .collect()
    }
}

/// Java's `String.split(":")`: empty strings at the end are dropped.
fn java_split_colon(token: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = token.split(':').collect();
    while parts.last().is_some_and(|part| part.is_empty()) {
        parts.pop();
    }
    parts
}

/// Whitespace as Java's regular expression `\s` sees it.
fn is_java_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// Splits a query into tokens at whitespace; a token starting with a
/// double quote runs to the closing quote (or the end) and comes out as a
/// phrase.
fn tokenize(query: &str) -> Vec<(String, bool)> {
    let mut tokens = Vec::new();
    let mut chars = query.chars().peekable();
    while let Some(&c) = chars.peek() {
        if is_java_space(c) {
            chars.next();
        } else if c == '"' {
            chars.next();
            let phrase: String = chars.by_ref().take_while(|&c| c != '"').collect();
            tokens.push((phrase, true));
        } else {
            let mut token = String::new();
            while let Some(&c) = chars.peek() {
                if is_java_space(c) {
                    break;
                }
                token.push(c);
                chars.next();
            }
            tokens.push((token, false));
        }
    }
    tokens
}

/// Parses a query (`QueryHelper.parseQuery`). The empty query has no
/// tokens.
pub fn parse_query(query: &str) -> Result<ParsedQuery, InvalidQuery> {
    // Java's `trim` strips everything up to U+0020.
    let query = query.trim_matches(|c: char| c <= ' ');
    let mut parsed = ParsedQuery::default();
    for (token, quoted) in tokenize(query) {
        if quoted {
            if !token.trim().is_empty() {
                parsed.text.push(TextTerm {
                    text: token,
                    prefix: false,
                });
            }
            continue;
        }
        if !token.contains(':') {
            let (text, prefix) = match token.strip_suffix('*') {
                Some(stem) => (stem, true),
                None => (token.as_str(), false),
            };
            if !text.is_empty() {
                parsed.text.push(TextTerm {
                    text: text.to_owned(),
                    prefix,
                });
            }
            continue;
        }
        let pair = java_split_colon(&token);
        let kind = match pair[..] {
            [key, _] => TokenQueryType::from_token(key),
            _ => None,
        };
        let Some(kind) = kind else {
            return Err(InvalidQuery(format!("Invalid query param: {token}")));
        };
        let value = pair[1];
        if kind == TokenQueryType::OrderBy && OrderBy::from_token(value).is_none() {
            return Err(InvalidQuery(format!(
                "Invalid orderby query value: {value}"
            )));
        }
        let values = parsed.tokens.entry(kind).or_default();
        if !values.iter().any(|existing| existing == value) {
            values.push(value.to_owned());
        }
    }
    Ok(parsed)
}

/// The participants named by the values of a `with:` or `creator:` filter
/// (`QueryHelper.buildValidatedParticipantIds`): a name without a domain
/// is of the local domain, and `@` is the shared domain participant.
pub fn build_validated_participant_ids(
    values: &[String],
    local_domain: &str,
) -> Result<Vec<ParticipantId>, InvalidParticipantAddress> {
    values
        .iter()
        .map(|token| {
            if !token.is_empty() && !token.contains('@') {
                ParticipantId::of(&format!("{token}@{local_domain}"))
            } else if token == "@" {
                ParticipantId::of(&format!("@{local_domain}"))
            } else {
                ParticipantId::of(token)
            }
        })
        .collect()
}

/// The address a wave without a root wavelet in view is sorted by
/// (`QueryHelper.UNKNOWN_CREATOR`).
pub const UNKNOWN_CREATOR: &str = "unknown@example.com";

/// What a wave is sorted by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKeys {
    pub wave_id: WaveId,
    /// The latest last-modified time of the conversational wavelets in
    /// view; -1 if there is none.
    pub last_modified: i64,
    /// The latest creation time of the wavelets in view.
    pub created: i64,
    /// The creator of the root wavelet, if it is in view.
    pub creator: Option<String>,
}

fn compare_by(order: OrderBy, a: &SortKeys, b: &SortKeys) -> Ordering {
    let creator = |keys: &'_ SortKeys| -> String {
        keys.creator
            .clone()
            .unwrap_or_else(|| UNKNOWN_CREATOR.to_owned())
    };
    match order {
        OrderBy::DateAsc => a.last_modified.cmp(&b.last_modified),
        OrderBy::DateDesc => b.last_modified.cmp(&a.last_modified),
        OrderBy::CreatedAsc => a.created.cmp(&b.created),
        OrderBy::CreatedDesc => b.created.cmp(&a.created),
        OrderBy::CreatorAsc => creator(a).cmp(&creator(b)),
        OrderBy::CreatorDesc => creator(b).cmp(&creator(a)),
    }
}

/// The ordering of search results (`QueryHelper.computeSorter`): the given
/// orderings compounded, the first being the primary one; by last-modified
/// time, latest first, if none is given; and always by wave id in the end,
/// for stability.
pub fn compare(orderings: &[OrderBy], a: &SortKeys, b: &SortKeys) -> Ordering {
    let default = [OrderBy::DateDesc];
    let orderings = if orderings.is_empty() {
        &default[..]
    } else {
        orderings
    };
    orderings
        .iter()
        .map(|&order| compare_by(order, a, b))
        .find(|ordering| ordering.is_ne())
        .unwrap_or_else(|| {
            (a.wave_id.domain(), a.wave_id.id()).cmp(&(b.wave_id.domain(), b.wave_id.id()))
        })
}

#[cfg(test)]
mod tests {
    //! The parsing tests are those of `QueryHelperTest`.

    use super::*;

    fn values(parsed: &ParsedQuery, kind: TokenQueryType) -> Vec<&str> {
        parsed.values(kind).iter().map(String::as_str).collect()
    }

    #[test]
    fn valid_query() {
        let parsed = parse_query("in:inbox orderby:dateasc").unwrap();
        assert_eq!(parsed.tokens.len(), 2);
        assert_eq!(values(&parsed, TokenQueryType::In), ["inbox"]);
        assert_eq!(values(&parsed, TokenQueryType::OrderBy), ["dateasc"]);
        assert!(parsed.text.is_empty());
    }

    #[test]
    fn invalid_query_is_reported() {
        assert_eq!(
            parse_query("in:inbox orderby:dateasc blabla:value").unwrap_err(),
            InvalidQuery("Invalid query param: blabla:value".to_owned())
        );
    }

    #[test]
    fn invalid_orderby_is_reported() {
        assert_eq!(
            parse_query("orderby:blabla").unwrap_err(),
            InvalidQuery("Invalid orderby query value: blabla".to_owned())
        );
        for order in OrderBy::ALL {
            let parsed = parse_query(&format!("orderby:{}", order.token())).unwrap();
            assert_eq!(parsed.orderings(), [order]);
        }
    }

    #[test]
    fn multiple_values_per_key_keep_their_order_without_duplicates() {
        let parsed =
            parse_query("  with:bob with:alice@example.com\twith:bob creator:carol \n").unwrap();
        assert_eq!(
            values(&parsed, TokenQueryType::With),
            ["bob", "alice@example.com"]
        );
        assert_eq!(values(&parsed, TokenQueryType::Creator), ["carol"]);
        let parsed = parse_query("orderby:creatorasc orderby:datedesc").unwrap();
        assert_eq!(parsed.orderings(), [OrderBy::CreatorAsc, OrderBy::DateDesc]);
    }

    #[test]
    fn the_empty_query_has_no_tokens() {
        for query in ["", "   ", "\t\n"] {
            assert_eq!(parse_query(query).unwrap(), ParsedQuery::default());
        }
    }

    #[test]
    fn malformed_pairs_are_invalid_as_in_java() {
        for bad in [
            "in:",
            ":inbox",
            "in:inbox:x",
            "::",
            ":",
            "IN:inbox",
            "tag:x",
        ] {
            assert_eq!(
                parse_query(bad).unwrap_err(),
                InvalidQuery(format!("Invalid query param: {bad}")),
                "{bad}"
            );
        }
        // `"in:inbox:".split(":")` drops the empty string at the end.
        let parsed = parse_query("in:inbox:").unwrap();
        assert_eq!(values(&parsed, TokenQueryType::In), ["inbox"]);
        // Any folder name parses.
        assert!(parse_query("in:whatever id:w+abc").is_ok());
    }

    #[test]
    fn tokens_without_a_colon_are_free_text() {
        let parsed = parse_query("hello in:inbox w\u{f6}rld* \"two words: here\" \"\" *").unwrap();
        assert_eq!(values(&parsed, TokenQueryType::In), ["inbox"]);
        assert_eq!(
            parsed.text,
            [
                TextTerm {
                    text: "hello".to_owned(),
                    prefix: false
                },
                TextTerm {
                    text: "w\u{f6}rld".to_owned(),
                    prefix: true
                },
                TextTerm {
                    text: "two words: here".to_owned(),
                    prefix: false
                },
            ]
        );
        // An unterminated quote runs to the end.
        assert_eq!(parse_query("\"a b").unwrap().text[0].text, "a b");
    }

    #[test]
    fn participant_filters_are_completed_and_validated() {
        let build = |tokens: &[&str]| {
            let tokens: Vec<String> = tokens.iter().map(|t| (*t).to_owned()).collect();
            build_validated_participant_ids(&tokens, "example.com").map(|ids| {
                ids.iter()
                    .map(|id| id.address().to_owned())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            build(&["bob", "@", "carol@other.org", "@other.org"]).unwrap(),
            [
                "bob@example.com",
                "@example.com",
                "carol@other.org",
                "@other.org"
            ]
        );
        assert_eq!(build(&[]).unwrap(), Vec::<String>::new());
        assert!(build(&["a@b@c"]).is_err());
        assert!(build(&["bob@"]).is_err());
    }

    fn keys(id: &str, last_modified: i64, created: i64, creator: Option<&str>) -> SortKeys {
        SortKeys {
            wave_id: WaveId::of("example.com", id).unwrap(),
            last_modified,
            created,
            creator: creator.map(str::to_owned),
        }
    }

    fn sorted(orderings: &[OrderBy], mut waves: Vec<SortKeys>) -> Vec<String> {
        waves.sort_by(|a, b| compare(orderings, a, b));
        waves.iter().map(|k| k.wave_id.id().to_owned()).collect()
    }

    #[test]
    fn orderings() {
        let waves = vec![
            keys("w+b", 30, 1, Some("bob@example.com")),
            keys("w+a", 10, 3, Some("alice@example.com")),
            keys("w+c", 20, 2, None),
            keys("w+d", 20, 2, Some("zed@example.com")),
        ];
        let check = |orderings: &[OrderBy], expected: [&str; 4]| {
            assert_eq!(sorted(orderings, waves.clone()), expected, "{orderings:?}");
        };
        // The default: last modified first; ties by wave id.
        check(&[], ["w+b", "w+c", "w+d", "w+a"]);
        check(&[OrderBy::DateDesc], ["w+b", "w+c", "w+d", "w+a"]);
        check(&[OrderBy::DateAsc], ["w+a", "w+c", "w+d", "w+b"]);
        check(&[OrderBy::CreatedAsc], ["w+b", "w+c", "w+d", "w+a"]);
        check(&[OrderBy::CreatedDesc], ["w+a", "w+c", "w+d", "w+b"]);
        // A wave without a root wavelet sorts as unknown@example.com.
        check(&[OrderBy::CreatorAsc], ["w+a", "w+b", "w+c", "w+d"]);
        check(&[OrderBy::CreatorDesc], ["w+d", "w+c", "w+b", "w+a"]);
        // Compound: the first is the primary ordering.
        check(
            &[OrderBy::DateAsc, OrderBy::CreatorDesc],
            ["w+a", "w+d", "w+c", "w+b"],
        );
    }
}
