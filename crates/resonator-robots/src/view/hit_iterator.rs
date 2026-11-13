//! The places in a blip that `document.modify` acts on
//! (`DocumentHitIterator`).

use std::collections::BTreeMap;

use super::api_view::ApiView;
use crate::api::{ElementType, Range};

/// Yields the ranges of a blip's text to modify, one at a time, looking at
/// the blip again for every range so that earlier modifications are taken
/// into account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentHitIterator {
    /// One given range (`Singleshot`).
    Singleshot { range: Range, called: bool },
    /// The occurrences of a text (`TextMatcher`).
    TextMatcher {
        search_for: Vec<u16>,
        from: i64,
        hits_left: i32,
    },
    /// The elements of a type whose properties have given values
    /// (`ElementMatcher`).
    ElementMatcher {
        element_type: ElementType,
        restrictions: BTreeMap<String, String>,
        index: i64,
        hits_left: i32,
    },
}

impl DocumentHitIterator {
    pub fn singleshot(range: Range) -> Self {
        DocumentHitIterator::Singleshot {
            range,
            called: false,
        }
    }

    /// A negative `max_hits` means no limit.
    pub fn text_matcher(search_for: &str, max_hits: i32) -> Self {
        DocumentHitIterator::TextMatcher {
            search_for: search_for.encode_utf16().collect(),
            from: -1,
            hits_left: max_hits,
        }
    }

    /// A negative `max_res` means no limit.
    pub fn element_matcher(
        element_type: ElementType,
        restrictions: BTreeMap<String, String>,
        max_res: i32,
    ) -> Self {
        DocumentHitIterator::ElementMatcher {
            element_type,
            restrictions,
            index: -1,
            hits_left: max_res,
        }
    }

    /// The next range, or `None` when there are no more (`next`).
    pub fn next(&mut self, view: &ApiView) -> Option<Range> {
        match self {
            DocumentHitIterator::Singleshot { range, called } => {
                if *called {
                    return None;
                }
                *called = true;
                Some(*range)
            }
            DocumentHitIterator::TextMatcher {
                search_for,
                from,
                hits_left,
            } => {
                if *hits_left == 0 {
                    return None;
                }
                // As in the original, the count goes below zero for "no
                // limit" and never reaches zero again.
                *hits_left = hits_left.wrapping_sub(1);
                let search_in = view.api_contents_utf16();
                let start = usize::try_from(*from + 1).unwrap_or(0);
                let next = index_of(&search_in, search_for, start)?;
                *from = next as i64;
                Some(Range::new(next as i32, (next + search_for.len()) as i32))
            }
            DocumentHitIterator::ElementMatcher {
                element_type,
                restrictions,
                index,
                hits_left,
            } => {
                if *hits_left == 0 {
                    return None;
                }
                *hits_left = hits_left.wrapping_sub(1);
                for info in view.elements() {
                    if info.element.element_type != *element_type
                        || (info.api_position as i64) <= *index
                    {
                        continue;
                    }
                    let all_matched = restrictions
                        .iter()
                        .all(|(key, value)| info.element.property(key) == Some(value.as_str()));
                    if !all_matched {
                        continue;
                    }
                    *index = info.api_position as i64;
                    return Some(Range::new(
                        info.api_position as i32,
                        info.api_position as i32 + 1,
                    ));
                }
                None
            }
        }
    }

    /// Tells the iterator that `delta` characters were inserted (or, if
    /// negative, removed) at `where_` (`shift`). Only the text matcher
    /// cares.
    pub fn shift(&mut self, where_: i64, delta: i64) {
        if let DocumentHitIterator::TextMatcher { from, .. } = self
            && *from != -1
            && where_ - 1 <= *from
        {
            *from += delta;
        }
    }
}

impl DocumentHitIterator {
    /// Makes the text matcher continue its search at `position` or later.
    /// Not in the original, whose text matcher would find a match again
    /// inside text inserted after it.
    pub fn skip_to(&mut self, position: i64) {
        if let DocumentHitIterator::TextMatcher { from, .. } = self {
            *from = (*from).max(position - 1);
        }
    }
}

/// `String.indexOf(needle, from)` over UTF-16 code units.
fn index_of(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if from > haystack.len() {
        return None;
    }
    if needle.is_empty() {
        return Some(from);
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| position + from)
}
