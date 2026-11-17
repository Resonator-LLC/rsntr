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
//!
//! # Additions to the original
//!
//! * `label:<n>`: the wave carries label `n` in the user's supplement (the
//!   folder ids of the original, used for ensembles).
//! * `after:YYYY-MM-DD` and `before:YYYY-MM-DD`: last modified on or after
//!   the start of that day (UTC), or before it.
//! * `title:word`: a free-text term that must be in the title.
//! * A leading `-` negates a token: `-with:bob`, `-in:mute`, `-word`. It
//!   applies to `in`, `with`, `creator`, `id`, `label`, `title` and free
//!   text; a negated `orderby`, `after` or `before` is invalid. In the
//!   original a leading `-` on a free-text term was punctuation.
//! * The bare token `OR` splits a query into groups; a wave matches if it
//!   matches any group ([`parse_queries`]). In the original `OR` was a
//!   free-text term.

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
    Label,
    After,
    Before,
    Title,
}

impl TokenQueryType {
    pub const ALL: [TokenQueryType; 9] = [
        TokenQueryType::In,
        TokenQueryType::OrderBy,
        TokenQueryType::With,
        TokenQueryType::Creator,
        TokenQueryType::Id,
        TokenQueryType::Label,
        TokenQueryType::After,
        TokenQueryType::Before,
        TokenQueryType::Title,
    ];

    pub fn token(self) -> &'static str {
        match self {
            TokenQueryType::In => "in",
            TokenQueryType::OrderBy => "orderby",
            TokenQueryType::With => "with",
            TokenQueryType::Creator => "creator",
            TokenQueryType::Id => "id",
            TokenQueryType::Label => "label",
            TokenQueryType::After => "after",
            TokenQueryType::Before => "before",
            TokenQueryType::Title => "title",
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
    /// The terms of `title:`; the title must have all of them.
    pub title: Vec<TextTerm>,
    /// The values of negated tokens (`-with:bob`): a wave matching any of
    /// them is left out.
    pub negated: BTreeMap<TokenQueryType, Vec<String>>,
    /// Negated free-text terms (`-word`).
    pub negated_text: Vec<TextTerm>,
    /// Negated title terms (`-title:word`).
    pub negated_title: Vec<TextTerm>,
}

impl ParsedQuery {
    pub fn values(&self, kind: TokenQueryType) -> &[String] {
        self.tokens.get(&kind).map_or(&[], Vec::as_slice)
    }

    pub fn has(&self, kind: TokenQueryType) -> bool {
        self.tokens.contains_key(&kind)
    }

    /// The values of a negated key.
    pub fn negated_values(&self, kind: TokenQueryType) -> &[String] {
        self.negated.get(&kind).map_or(&[], Vec::as_slice)
    }

    /// The labels of `label:` tokens, or of negated ones.
    pub fn labels(&self, negated: bool) -> Vec<i64> {
        let values = if negated {
            self.negated_values(TokenQueryType::Label)
        } else {
            self.values(TokenQueryType::Label)
        };
        values.iter().filter_map(|v| v.parse().ok()).collect()
    }

    /// The latest `after:` as milliseconds since the epoch.
    pub fn after(&self) -> Option<i64> {
        self.values(TokenQueryType::After)
            .iter()
            .filter_map(|v| parse_date_millis(v))
            .max()
    }

    /// The earliest `before:` as milliseconds since the epoch.
    pub fn before(&self) -> Option<i64> {
        self.values(TokenQueryType::Before)
            .iter()
            .filter_map(|v| parse_date_millis(v))
            .min()
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

/// Parses a query (`QueryHelper.parseQuery`) without splitting it at
/// `OR`, which is then a free-text term. The empty query has no tokens.
pub fn parse_query(query: &str) -> Result<ParsedQuery, InvalidQuery> {
    parse_tokens(tokenize(trim_java(query)))
}

/// Parses a query that may have `OR` groups: one [`ParsedQuery`] per
/// group, at least one. An empty group (`OR` at either end, or two in a
/// row) makes the query invalid.
pub fn parse_queries(query: &str) -> Result<Vec<ParsedQuery>, InvalidQuery> {
    let mut groups = vec![Vec::new()];
    for (token, quoted) in tokenize(trim_java(query)) {
        if !quoted && token == "OR" {
            groups.push(Vec::new());
        } else if let Some(group) = groups.last_mut() {
            group.push((token, quoted));
        }
    }
    if groups.len() > 1 && groups.iter().any(Vec::is_empty) {
        return Err(InvalidQuery("Empty group around OR".to_owned()));
    }
    groups.into_iter().map(parse_tokens).collect()
}

/// Java's `trim` strips everything up to U+0020.
fn trim_java(query: &str) -> &str {
    query.trim_matches(|c: char| c <= ' ')
}

/// A free-text term, `None` if it is empty.
fn text_term(token: &str) -> Option<TextTerm> {
    let (text, prefix) = match token.strip_suffix('*') {
        Some(stem) => (stem, true),
        None => (token, false),
    };
    (!text.is_empty()).then(|| TextTerm {
        text: text.to_owned(),
        prefix,
    })
}

fn parse_tokens(tokens: Vec<(String, bool)>) -> Result<ParsedQuery, InvalidQuery> {
    let mut parsed = ParsedQuery::default();
    for (token, quoted) in tokens {
        if quoted {
            if !token.trim().is_empty() {
                parsed.text.push(TextTerm {
                    text: token,
                    prefix: false,
                });
            }
            continue;
        }
        if token == "-" {
            // A lone minus negates nothing.
            continue;
        }
        let (negated, token) = match token.strip_prefix('-') {
            Some(rest) if !rest.is_empty() => (true, rest.to_owned()),
            _ => (false, token),
        };
        if !token.contains(':') {
            if let Some(term) = text_term(&token) {
                if negated {
                    parsed.negated_text.push(term);
                } else {
                    parsed.text.push(term);
                }
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
        match kind {
            TokenQueryType::OrderBy if OrderBy::from_token(value).is_none() => {
                return Err(InvalidQuery(format!(
                    "Invalid orderby query value: {value}"
                )));
            }
            TokenQueryType::Label if value.parse::<i32>().is_err() => {
                return Err(InvalidQuery(format!("Invalid label: {value}")));
            }
            TokenQueryType::After | TokenQueryType::Before
                if parse_date_millis(value).is_none() =>
            {
                return Err(InvalidQuery(format!(
                    "Invalid date: {value} (use YYYY-MM-DD)"
                )));
            }
            _ => {}
        }
        if negated
            && matches!(
                kind,
                TokenQueryType::OrderBy | TokenQueryType::After | TokenQueryType::Before
            )
        {
            return Err(InvalidQuery(format!("Cannot negate {}:", kind.token())));
        }
        if kind == TokenQueryType::Title {
            let Some(term) = text_term(value) else {
                continue;
            };
            if negated {
                parsed.negated_title.push(term);
            } else {
                parsed.title.push(term);
            }
            continue;
        }
        let map = if negated {
            &mut parsed.negated
        } else {
            &mut parsed.tokens
        };
        let values = map.entry(kind).or_default();
        if !values.iter().any(|existing| existing == value) {
            values.push(value.to_owned());
        }
    }
    Ok(parsed)
}

/// `YYYY-MM-DD` as milliseconds since the epoch at the start of that day
/// in UTC; `None` if it is not a valid date.
pub fn parse_date_millis(text: &str) -> Option<i64> {
    let mut parts = text.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let year: i64 = year.parse().ok()?;
    let month: i64 = month.parse().ok()?;
    let day: i64 = day.parse().ok()?;
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if day < 1 || day > days_in_month {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400_000)
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
            "label:x",
            "after:2026-13-01",
            "before:yesterday",
        ] {
            assert!(parse_query(bad).is_err(), "{bad}");
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

    #[test]
    fn unknown_keys_report_the_token() {
        assert_eq!(
            parse_query("tag:x").unwrap_err(),
            InvalidQuery("Invalid query param: tag:x".to_owned())
        );
    }

    #[test]
    fn labels_dates_and_titles() {
        let parsed =
            parse_query("label:3 label:-2 after:2026-11-01 before:2026-11-16 title:relay*")
                .unwrap();
        assert_eq!(parsed.labels(false), [3, -2]);
        assert_eq!(parsed.after(), Some(1_793_491_200_000));
        assert_eq!(parsed.before(), Some(1_794_787_200_000));
        assert_eq!(
            parsed.title,
            [TextTerm {
                text: "relay".to_owned(),
                prefix: true
            }]
        );
        assert!(parsed.text.is_empty());
        assert_eq!(parse_date_millis("1970-01-01"), Some(0));
        assert_eq!(parse_date_millis("2024-02-29"), Some(1_709_164_800_000));
        assert_eq!(parse_date_millis("2023-02-29"), None);
        assert_eq!(parse_date_millis("2026-1-01"), None);
    }

    #[test]
    fn negation() {
        let parsed =
            parse_query("-with:bob -in:mute -label:4 -spam -title:draft hello - -").unwrap();
        assert_eq!(parsed.negated_values(TokenQueryType::With), ["bob"]);
        assert_eq!(parsed.negated_values(TokenQueryType::In), ["mute"]);
        assert_eq!(parsed.labels(true), [4]);
        assert_eq!(parsed.negated_text[0].text, "spam");
        assert_eq!(parsed.negated_title[0].text, "draft");
        assert_eq!(parsed.text.len(), 1);
        assert!(parsed.tokens.is_empty());
        for bad in [
            "-orderby:dateasc",
            "-after:2026-01-01",
            "-before:2026-01-01",
        ] {
            assert!(parse_query(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn or_groups() {
        let groups = parse_queries("in:inbox label:3 OR in:inbox with:bob relay").unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].labels(false), [3]);
        assert_eq!(values(&groups[1], TokenQueryType::With), ["bob"]);
        assert_eq!(parse_queries("relay").unwrap().len(), 1);
        assert_eq!(parse_queries("").unwrap(), [ParsedQuery::default()]);
        // A quoted or lower-case or is text.
        assert_eq!(parse_queries("a \"OR\" or b").unwrap().len(), 1);
        for bad in ["OR a", "a OR", "a OR OR b", "OR"] {
            assert!(parse_queries(bad).is_err(), "{bad}");
        }
        // Without groups, OR is a word.
        assert_eq!(parse_query("a OR b").unwrap().text.len(), 3);
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
