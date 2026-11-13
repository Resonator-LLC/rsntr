//! Filters over wavelet ids (`IdFilter`, `IdFilters`).

use std::collections::BTreeSet;
use std::fmt;

use super::constants::CONVERSATION_WAVELET_PREFIX;
use super::wave_id::WaveletId;

/// Accepts wavelet ids that are in a set of ids or whose local id starts
/// with one of a set of prefixes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdFilter {
    ids: BTreeSet<WaveletId>,
    prefixes: BTreeSet<String>,
}

impl IdFilter {
    pub fn of(
        ids: impl IntoIterator<Item = WaveletId>,
        prefixes: impl IntoIterator<Item = String>,
    ) -> Self {
        IdFilter {
            ids: ids.into_iter().collect(),
            prefixes: prefixes.into_iter().collect(),
        }
    }

    pub fn of_ids(ids: impl IntoIterator<Item = WaveletId>) -> Self {
        Self::of(ids, [])
    }

    pub fn of_prefixes<'a>(prefixes: impl IntoIterator<Item = &'a str>) -> Self {
        Self::of([], prefixes.into_iter().map(str::to_owned))
    }

    /// Accepts every id (a single empty prefix).
    pub fn all_ids() -> Self {
        Self::of_prefixes([""])
    }

    /// Accepts no id.
    pub fn no_ids() -> Self {
        Self::of([], [])
    }

    /// Accepts conversational wavelet ids.
    pub fn conversation_wavelet_ids() -> Self {
        Self::of_prefixes([CONVERSATION_WAVELET_PREFIX])
    }

    pub fn accepts(&self, id: &WaveletId) -> bool {
        self.ids.contains(id)
            || self
                .prefixes
                .iter()
                .any(|p| id.id().starts_with(p.as_str()))
    }

    pub fn ids(&self) -> &BTreeSet<WaveletId> {
        &self.ids
    }

    pub fn prefixes(&self) -> &BTreeSet<String> {
        &self.prefixes
    }
}

impl fmt::Display for IdFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdFilter[")?;
        // Distinguishes no prefixes from a single empty prefix.
        if !self.prefixes.is_empty() {
            write!(f, "prefixes={:?}", self.prefixes)?;
        }
        if !self.ids.is_empty() {
            if !self.prefixes.is_empty() {
                f.write_str(", ")?;
            }
            f.write_str("ids=[")?;
            for (i, id) in self.ids.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{id}")?;
            }
            f.write_str("]")?;
        }
        f.write_str("]")
    }
}
