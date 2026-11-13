//! The search index in SQLite: which wavelets each participant is on (the
//! per-user wave view of `PerUserWaveViewHandler` in the original), what a
//! wave is sorted by, each user's archive and follow state, and the text
//! of conversational wavelets in an FTS5 table.
//!
//! Everything here is derived from the wavelets and can be rebuilt from
//! them. Rows carry the wavelet version they reflect and are only ever
//! replaced by a row of the same or a later version, so that the live
//! indexer and the catch-up at start-up can write in any order.
//!
//! # Full text
//!
//! The FTS5 table uses the `unicode61` tokenizer with `remove_diacritics
//! 2`: words are runs of letters and digits in any script, compared
//! without case and without diacritics. That alone serves text written
//! with spaces between words. For the rest, text is rewritten before it is
//! indexed and before it is searched ([`fts_normalize`]): every character
//! of a script written without spaces (Han, kana, Hangul, Thai, Lao, Khmer,
//! Myanmar) becomes a word of its own, so that any run of such characters
//! is found as a phrase; and every emoji or pictograph, which `unicode61`
//! would drop as punctuation, becomes a word naming its code point.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use resonator_model::conversation::{Conversation, wavelet_has_conversation};
use resonator_model::data::{BlipData, WaveletData};
use resonator_model::digest::collate_text_for_documents;
use resonator_model::id::util::{
    is_blip_id, is_conversation_root_wavelet_id, is_conversational_id, is_user_data_wavelet,
};
use resonator_model::id::{ParticipantId, WaveId, WaveletId, WaveletName};
use resonator_model::supplement::{NO_VERSION, ReadablePrimitiveSupplement, UserDataSupplement};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use super::query::{SortKeys, TextTerm};
use crate::storage::{Database, StorageError};

/// Meta key: the version of the text normalisation the full-text table
/// was filled with.
const META_TEXT_SCHEME: &str = "search_text_scheme";

/// Changes whenever [`fts_normalize`] or the tokenizer changes what is
/// stored, which makes the next start refill the full-text table.
const TEXT_SCHEME: &str = "1";

pub(crate) const KIND_OTHER: i64 = 0;
pub(crate) const KIND_CONVERSATIONAL: i64 = 1;
pub(crate) const KIND_USER_DATA: i64 = 2;

fn is_unspaced_script(c: char) -> bool {
    matches!(
        c as u32,
        0x0E00..=0x0EFF      // Thai, Lao
        | 0x1000..=0x109F    // Myanmar
        | 0x1780..=0x17FF    // Khmer
        | 0x3040..=0x30FF    // Hiragana, Katakana
        | 0x31F0..=0x31FF    // Katakana extensions
        | 0x3400..=0x4DBF    // CJK extension A
        | 0x4E00..=0x9FFF    // CJK unified ideographs
        | 0xAC00..=0xD7AF    // Hangul syllables
        | 0xF900..=0xFAFF    // CJK compatibility ideographs
        | 0xFF66..=0xFF9F    // Half-width katakana
        | 0x20000..=0x3FFFF  // CJK extensions B and later
    )
}

fn is_pictograph(c: char) -> bool {
    matches!(
        c as u32,
        0x2190..=0x21FF      // Arrows
        | 0x2300..=0x23FF    // Miscellaneous technical
        | 0x25A0..=0x27BF    // Shapes, symbols, dingbats
        | 0x2B00..=0x2BFF    // More symbols and arrows
        | 0x1F000..=0x1FAFF  // Emoji and pictographs
    )
}

/// Rewrites text for the full-text index (see the module documentation).
/// The same rewriting is applied to the text indexed and to the terms
/// searched for.
pub fn fts_normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    for c in text.chars() {
        if is_unspaced_script(c) {
            out.push(' ');
            out.push(c);
            out.push(' ');
        } else if is_pictograph(c) {
            let _ = write!(out, " wvsym{:x} ", c as u32);
        } else if c == '\0' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// The FTS5 query for free-text terms: every term a phrase, all of them
/// required. `None` if there are no terms.
pub fn fts_query(terms: &[TextTerm]) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    let mut query = String::new();
    for term in terms {
        if !query.is_empty() {
            query.push(' ');
        }
        query.push('"');
        for c in fts_normalize(&term.text).chars() {
            if c == '"' {
                // A quote is punctuation to the tokenizer anyway.
                query.push(' ');
            } else {
                query.push(c);
            }
        }
        query.push('"');
        if term.prefix {
            query.push('*');
        }
    }
    Some(query)
}

/// The state a user keeps in a user-data wavelet, as far as searching
/// needs it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct UserState {
    /// The explicit follow state.
    pub followed: Option<bool>,
    /// The version at which each conversational wavelet was archived.
    pub archive: Vec<(WaveletId, i32)>,
}

/// What the index holds about a wavelet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaveletEntry {
    pub name: WaveletName,
    pub version: u64,
    pub creator: String,
    pub created: i64,
    pub last_modified: i64,
    pub participants: BTreeSet<String>,
    pub kind: i64,
    pub is_root: bool,
    pub has_conversation: bool,
    pub user_state: Option<UserState>,
    /// Title and blip text, normalised; `None` leaves the indexed text as
    /// it is.
    pub text: Option<(String, String)>,
}

pub(crate) fn kind_of(wavelet_id: &WaveletId) -> i64 {
    if is_conversational_id(wavelet_id).unwrap_or(false) {
        KIND_CONVERSATIONAL
    } else if is_user_data_wavelet(wavelet_id) {
        KIND_USER_DATA
    } else {
        KIND_OTHER
    }
}

impl WaveletEntry {
    /// Reads a wavelet. Runs under the wavelet's lock, so it copies and
    /// leaves the rest to the caller's thread.
    pub(crate) fn extract(wavelet: &WaveletData, with_text: bool) -> Self {
        let kind = kind_of(wavelet.wavelet_id());
        let conversational = kind == KIND_CONVERSATIONAL;
        let user_state = (kind == KIND_USER_DATA).then(|| {
            let supplement = UserDataSupplement::new(wavelet);
            UserState {
                followed: supplement.followed(),
                archive: supplement
                    .archive_wavelets()
                    .into_iter()
                    .map(|id| {
                        let version = supplement.archive_wavelet_version(&id);
                        (id, version)
                    })
                    .filter(|(_, version)| *version != NO_VERSION)
                    .collect(),
            }
        });
        let text = (with_text && conversational).then(|| {
            let title = Conversation::read(wavelet)
                .map(|conversation| conversation.title())
                .unwrap_or_default();
            let content = collate_text_for_documents(
                wavelet
                    .documents()
                    .filter(|blip| is_blip_id(blip.id()))
                    .map(BlipData::content),
            );
            (title, content)
        });
        WaveletEntry {
            name: wavelet.wavelet_name(),
            version: wavelet.version(),
            creator: wavelet.creator().address().to_owned(),
            created: wavelet.creation_time(),
            last_modified: wavelet.last_modified_time(),
            participants: wavelet
                .participants()
                .iter()
                .map(|participant| participant.address().to_owned())
                .collect(),
            kind,
            is_root: is_conversation_root_wavelet_id(wavelet.wavelet_id()),
            has_conversation: conversational && wavelet_has_conversation(wavelet),
            user_state,
            text,
        }
    }

    /// Normalises the text for the full-text table; kept out of
    /// [`Self::extract`] because that runs under the wavelet's lock.
    pub(crate) fn normalize_text(&mut self) {
        if let Some((title, content)) = &mut self.text {
            *title = fts_normalize(title);
            *content = fts_normalize(content);
        }
    }
}

/// The outcome of writing entries: who has to be told that their search
/// results may have changed.
#[derive(Debug, Default)]
pub(crate) struct WriteOutcome {
    /// Addresses of the participants, before or after, of the wavelets
    /// written.
    pub affected: BTreeSet<String>,
    /// Entries written (not skipped as older than the index).
    pub written: usize,
}

fn name_params(name: &WaveletName) -> [&str; 4] {
    [
        name.wave_id.domain(),
        name.wave_id.id(),
        name.wavelet_id.domain(),
        name.wavelet_id.id(),
    ]
}

fn write_entry(
    conn: &Connection,
    entry: &WaveletEntry,
    fts: bool,
    outcome: &mut WriteOutcome,
) -> Result<(), StorageError> {
    let [wave_domain, wave_id, wavelet_domain, wavelet_id] = name_params(&entry.name);
    let version = i64::try_from(entry.version).unwrap_or(i64::MAX);
    let followed = entry
        .user_state
        .as_ref()
        .and_then(|state| state.followed)
        .map(i64::from);
    // Never go back to an older version: the live indexer and the
    // catch-up may write the same wavelet in either order.
    let id: Option<i64> = conn
        .prepare_cached(
            "INSERT INTO search_wavelets (wave_domain, wave_id, wavelet_domain, wavelet_id, \
             version, creator, created, last_modified, kind, is_root, has_conversation, followed) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
             ON CONFLICT (wave_domain, wave_id, wavelet_domain, wavelet_id) DO UPDATE SET \
             version = excluded.version, creator = excluded.creator, created = excluded.created, \
             last_modified = excluded.last_modified, kind = excluded.kind, \
             is_root = excluded.is_root, has_conversation = excluded.has_conversation, \
             followed = excluded.followed \
             WHERE excluded.version >= search_wavelets.version \
             RETURNING id",
        )?
        .query_row(
            params![
                wave_domain,
                wave_id,
                wavelet_domain,
                wavelet_id,
                version,
                entry.creator,
                entry.created,
                entry.last_modified,
                entry.kind,
                entry.is_root,
                entry.has_conversation,
                followed
            ],
            |row| row.get(0),
        )
        .optional()?;
    let Some(id) = id else {
        return Ok(());
    };
    outcome.written += 1;

    let existing: BTreeSet<String> = conn
        .prepare_cached("SELECT participant FROM search_participants WHERE wavelet = ?1")?
        .query_map([id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for removed in existing.difference(&entry.participants) {
        conn.prepare_cached(
            "DELETE FROM search_participants WHERE participant = ?1 AND wavelet = ?2",
        )?
        .execute(params![removed, id])?;
    }
    for added in entry.participants.difference(&existing) {
        conn.prepare_cached(
            "INSERT INTO search_participants (participant, wavelet) VALUES (?1, ?2)",
        )?
        .execute(params![added, id])?;
    }
    outcome.affected.extend(existing);
    outcome.affected.extend(entry.participants.iter().cloned());

    if let Some(state) = &entry.user_state {
        conn.prepare_cached("DELETE FROM search_archive WHERE udw = ?1")?
            .execute([id])?;
        for (wavelet, archived_at) in &state.archive {
            conn.prepare_cached(
                "INSERT OR REPLACE INTO search_archive (udw, wavelet_domain, wavelet_id, version) \
                 VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![id, wavelet.domain(), wavelet.id(), archived_at])?;
        }
    }

    if fts && let Some((title, content)) = &entry.text {
        conn.prepare_cached(
            "INSERT OR REPLACE INTO search_text (rowid, title, content) VALUES (?1, ?2, ?3)",
        )?
        .execute(params![id, title, content])?;
        conn.prepare_cached("UPDATE search_wavelets SET text_version = version WHERE id = ?1")?
            .execute([id])?;
    }
    Ok(())
}

/// The search index tables.
#[derive(Debug, Clone)]
pub(crate) struct IndexStore {
    db: Database,
}

/// The filters of a search, as the index query takes them.
#[derive(Debug, Clone, Default)]
pub(crate) struct IndexQuery {
    pub user: String,
    /// The shared domain participant, if waves shared with the domain are
    /// to be found too (a query without `in:`).
    pub shared: Option<String>,
    /// The id of the user's user-data wavelets.
    pub user_data: Option<WaveletId>,
    /// The folders the wave must be in: all of them.
    pub folders: Vec<Folder>,
    /// Every one of these must be a participant.
    pub with: Vec<String>,
    /// The creator must be every one of these.
    pub creators: Vec<String>,
    /// If not empty, the wave must be one of these.
    pub wave_ids: Vec<WaveId>,
    /// The FTS5 query, if there are free-text terms.
    pub text: Option<String>,
}

/// A folder an `in:` token names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Folder {
    Inbox,
    Archive,
    Mute,
}

impl IndexStore {
    pub(crate) fn new(db: Database) -> Self {
        IndexStore { db }
    }

    /// Creates the full-text table if the SQLite build has FTS5; returns
    /// whether full-text search is available. A table filled under another
    /// text scheme is emptied, to be refilled by the catch-up.
    pub(crate) async fn prepare_full_text(&self) -> Result<bool, StorageError> {
        self.db
            .write(|conn| {
                let has_fts5: bool = conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM pragma_compile_options \
                     WHERE compile_options = 'ENABLE_FTS5')",
                    [],
                    |row| row.get(0),
                )?;
                if !has_fts5 {
                    return Ok(false);
                }
                let scheme: Option<String> = conn
                    .query_row(
                        "SELECT value FROM meta WHERE key = ?1",
                        [META_TEXT_SCHEME],
                        |row| row.get(0),
                    )
                    .optional()?;
                if scheme.as_deref() != Some(TEXT_SCHEME) {
                    conn.execute_batch(
                        "DROP TABLE IF EXISTS search_text; \
                         UPDATE search_wavelets SET text_version = -1;",
                    )?;
                    conn.execute(
                        "INSERT INTO meta (key, value) VALUES (?1, ?2) \
                         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                        [META_TEXT_SCHEME, TEXT_SCHEME],
                    )?;
                }
                conn.execute_batch(
                    "CREATE VIRTUAL TABLE IF NOT EXISTS search_text USING fts5(\
                     title, content, tokenize = \"unicode61 remove_diacritics 2\")",
                )?;
                Ok(true)
            })
            .await
    }

    /// Writes entries in one transaction.
    pub(crate) async fn write(
        &self,
        entries: Vec<WaveletEntry>,
        fts: bool,
    ) -> Result<WriteOutcome, StorageError> {
        if entries.is_empty() {
            return Ok(WriteOutcome::default());
        }
        self.db
            .write(move |conn| {
                let mut outcome = WriteOutcome::default();
                for entry in &entries {
                    write_entry(conn, entry, fts, &mut outcome)?;
                }
                Ok(outcome)
            })
            .await
    }

    /// The stored wavelets whose index row is missing or not at the
    /// wavelet's last stored version, or (with `fts`) whose text is behind.
    pub(crate) async fn stale_wavelets(&self, fts: bool) -> Result<Vec<WaveletName>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare(
                    "SELECT w.wave_domain, w.wave_id, w.wavelet_domain, w.wavelet_id \
                     FROM wavelets w LEFT JOIN search_wavelets s \
                       ON s.wave_domain = w.wave_domain AND s.wave_id = w.wave_id \
                      AND s.wavelet_domain = w.wavelet_domain AND s.wavelet_id = w.wavelet_id \
                     WHERE s.id IS NULL \
                        OR s.version != (SELECT d.resulting_version FROM deltas d \
                                         WHERE d.wavelet = w.id \
                                         ORDER BY d.applied_at_version DESC LIMIT 1) \
                        OR (?1 AND s.kind = 1 AND s.text_version != s.version)",
                )?;
                let rows = statement.query_map([fts], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?;
                let mut names = Vec::new();
                for row in rows {
                    let (wave_domain, wave_id, wavelet_domain, wavelet_id) = row?;
                    // Ids that no longer parse are left alone.
                    if let Ok(name) =
                        WaveletName::of_parts(&wave_domain, &wave_id, &wavelet_domain, &wavelet_id)
                    {
                        names.push(name);
                    }
                }
                Ok(names)
            })
            .await
    }

    /// Removes the rows of wavelets that are no longer stored, and rows
    /// claiming a later version than the wavelet has (which no delta
    /// would ever replace). Returns how many were removed.
    pub(crate) async fn remove_orphans(&self, fts: bool) -> Result<usize, StorageError> {
        self.db
            .write(move |conn| {
                let same = "w.wave_domain = search_wavelets.wave_domain \
                            AND w.wave_id = search_wavelets.wave_id \
                            AND w.wavelet_domain = search_wavelets.wavelet_domain \
                            AND w.wavelet_id = search_wavelets.wavelet_id";
                let removed = conn.execute(
                    &format!(
                        "DELETE FROM search_wavelets WHERE NOT EXISTS \
                         (SELECT 1 FROM wavelets w WHERE {same}) \
                         OR version > COALESCE((SELECT d.resulting_version FROM wavelets w \
                         JOIN deltas d ON d.wavelet = w.id WHERE {same} \
                         ORDER BY d.applied_at_version DESC LIMIT 1), 0)"
                    ),
                    [],
                )?;
                if fts {
                    conn.execute(
                        "DELETE FROM search_text WHERE rowid NOT IN \
                         (SELECT id FROM search_wavelets)",
                        [],
                    )?;
                }
                Ok(removed)
            })
            .await
    }

    /// The waves matching a query with what they are sorted by, in no
    /// particular order.
    pub(crate) async fn find_waves(
        &self,
        query: IndexQuery,
    ) -> Result<Vec<SortKeys>, StorageError> {
        self.db
            .read(move |conn| {
                let mut sql = String::from(
                    "SELECT w.wave_domain, w.wave_id, \
                     MAX(CASE WHEN w.kind = 1 THEN w.last_modified ELSE -1 END), \
                     MAX(w.created), \
                     MAX(CASE WHEN w.is_root = 1 THEN w.creator END) \
                     FROM search_wavelets w WHERE w.id IN (SELECT wavelet FROM search_participants \
                     WHERE participant = ?1",
                );
                let mut values: Vec<Value> = vec![query.user.clone().into()];
                let mut bind = |value: Value| {
                    values.push(value);
                    values.len()
                };
                if let Some(shared) = &query.shared {
                    let n = bind(shared.clone().into());
                    let _ = write!(sql, " OR participant = ?{n}");
                }
                sql.push(')');
                for creator in &query.creators {
                    let n = bind(creator.clone().into());
                    let _ = write!(sql, " AND w.creator = ?{n}");
                }
                for with in &query.with {
                    let n = bind(with.clone().into());
                    let _ = write!(
                        sql,
                        " AND EXISTS (SELECT 1 FROM search_participants p \
                         WHERE p.participant = ?{n} AND p.wavelet = w.id)"
                    );
                }
                if !query.wave_ids.is_empty() {
                    sql.push_str(" AND (");
                    for (index, wave_id) in query.wave_ids.iter().enumerate() {
                        if index > 0 {
                            sql.push_str(" OR ");
                        }
                        let domain = bind(wave_id.domain().to_owned().into());
                        let id = bind(wave_id.id().to_owned().into());
                        let _ = write!(sql, "(w.wave_domain = ?{domain} AND w.wave_id = ?{id})");
                    }
                    sql.push(')');
                }
                if let Some(text) = &query.text {
                    let n = bind(text.clone().into());
                    let _ = write!(
                        sql,
                        " AND w.id IN (SELECT rowid FROM search_text WHERE search_text MATCH ?{n})"
                    );
                }
                sql.push_str(" GROUP BY w.wave_domain, w.wave_id HAVING MAX(w.kind = 1) = 1");
                if !query.folders.is_empty() {
                    // The user's own user-data wavelet of the wave.
                    let (domain, id) = query
                        .user_data
                        .as_ref()
                        .map_or(("", ""), |udw| (udw.domain(), udw.id()));
                    let udw_domain = bind(domain.to_owned().into());
                    let udw_id = bind(id.to_owned().into());
                    let udw = format!(
                        "u.wave_domain = w.wave_domain AND u.wave_id = w.wave_id \
                         AND u.wavelet_domain = ?{udw_domain} AND u.wavelet_id = ?{udw_id} \
                         AND u.creator = ?1"
                    );
                    // The conversations the user is on.
                    let conversations = "FROM search_wavelets c \
                         WHERE c.wave_domain = w.wave_domain AND c.wave_id = w.wave_id \
                         AND c.has_conversation = 1 AND EXISTS (SELECT 1 FROM search_participants p \
                         WHERE p.participant = ?1 AND p.wavelet = c.id)";
                    // Where the wave is for the user, from their
                    // user-data wavelet: muted (2) if they unfollowed it;
                    // else archived (1) if there are conversations and
                    // each was archived at its current version or later;
                    // else in the inbox (0), as is every wave they have
                    // no user-data wavelet of.
                    let state = format!(
                        "COALESCE((SELECT CASE WHEN u.followed = 0 THEN 2 \
                         WHEN EXISTS (SELECT 1 {conversations}) AND NOT EXISTS (SELECT 1 \
                         {conversations} AND NOT EXISTS (SELECT 1 FROM search_archive a \
                         WHERE a.udw = u.id AND a.wavelet_domain = c.wavelet_domain \
                         AND a.wavelet_id = c.wavelet_id AND a.version >= c.version)) THEN 1 \
                         ELSE 0 END FROM search_wavelets u WHERE {udw}), 0)"
                    );
                    for folder in &query.folders {
                        let code = match folder {
                            Folder::Inbox => 0,
                            Folder::Archive => 1,
                            Folder::Mute => 2,
                        };
                        let _ = write!(sql, " AND {state} = {code}");
                    }
                }
                let mut statement = conn.prepare_cached(&sql)?;
                let rows = statement.query_map(params_from_iter(values), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })?;
                let mut waves = Vec::new();
                for row in rows {
                    let (domain, id, last_modified, created, creator) = row?;
                    let Ok(wave_id) = WaveId::of(&domain, &id).or_else(|_| WaveId::of_legacy(&domain, &id))
                    else {
                        continue;
                    };
                    waves.push(SortKeys {
                        wave_id,
                        last_modified,
                        created,
                        creator,
                    });
                }
                Ok(waves)
            })
            .await
    }

    /// For each wave, the wavelets the index has `user` (or `shared`) on.
    pub(crate) async fn wavelets_in_view(
        &self,
        waves: Vec<WaveId>,
        user: &ParticipantId,
        shared: Option<String>,
    ) -> Result<Vec<Vec<WaveletId>>, StorageError> {
        let user = user.address().to_owned();
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT w.wavelet_domain, w.wavelet_id FROM search_wavelets w \
                     WHERE w.wave_domain = ?1 AND w.wave_id = ?2 AND EXISTS (SELECT 1 \
                     FROM search_participants p WHERE p.wavelet = w.id \
                     AND (p.participant = ?3 OR p.participant = ?4))",
                )?;
                let mut result = Vec::with_capacity(waves.len());
                for wave in &waves {
                    let rows = statement
                        .query_map(params![wave.domain(), wave.id(), user, shared], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })?;
                    let mut wavelets = Vec::new();
                    for row in rows {
                        let (domain, id) = row?;
                        if let Ok(wavelet_id) = WaveletId::of(&domain, &id)
                            .or_else(|_| WaveletId::of_legacy(&domain, &id))
                        {
                            wavelets.push(wavelet_id);
                        }
                    }
                    result.push(wavelets);
                }
                Ok(result)
            })
            .await
    }

    /// Number of wavelets in the index.
    pub(crate) async fn wavelet_count(&self) -> Result<u64, StorageError> {
        self.db
            .read(|conn| {
                let count: i64 =
                    conn.query_row("SELECT COUNT(*) FROM search_wavelets", [], |row| row.get(0))?;
                Ok(u64::try_from(count).unwrap_or(0))
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalisation_splits_unspaced_scripts_and_names_pictographs() {
        assert_eq!(fts_normalize("Hello, w\u{f6}rld!"), "Hello, w\u{f6}rld!");
        assert_eq!(
            fts_normalize("\u{4e2d}\u{6587}ab"),
            " \u{4e2d}  \u{6587} ab"
        );
        assert_eq!(fts_normalize("ok\u{1F600}"), "ok wvsym1f600 ");
        assert_eq!(fts_normalize("a\0b"), "a b");
    }

    #[test]
    fn queries_are_quoted_phrases() {
        let term = |text: &str, prefix: bool| TextTerm {
            text: text.to_owned(),
            prefix,
        };
        assert_eq!(fts_query(&[]), None);
        assert_eq!(
            fts_query(&[term("hello", false), term("wor", true)]).unwrap(),
            "\"hello\" \"wor\"*"
        );
        // Nothing a user types can leave the phrase.
        assert_eq!(
            fts_query(&[term("a\" OR \"b", false), term("NEAR(x", false)]).unwrap(),
            "\"a  OR  b\" \"NEAR(x\""
        );
        assert_eq!(
            fts_query(&[term("\u{4e2d}\u{6587}", false)]).unwrap(),
            "\" \u{4e2d}  \u{6587} \""
        );
    }
}
