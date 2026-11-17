//! SQL mode: a search that starts with `SELECT ` is run as SQL.
//!
//! The query never touches the server's database. It runs on a private
//! in-memory database that holds a copy of what the user can see and
//! nothing else, so a query can read only the user's own data whatever it
//! does: there is no other user's row, account, session or delta in reach.
//! The copy is made with the same visibility rule as every other search
//! ([`super::index::visible_sql`]) and is kept per user until their search
//! generation changes.
//!
//! The tables (the public schema of SQL mode; see `docs/search.md`):
//!
//! * `waves(wave_id, title, creator, created, last_modified, box)`: every
//!   wave with a conversation the user is on. `wave_id` is `domain/id`,
//!   times are milliseconds since the epoch, `box` is `inbox`, `archive`
//!   or `mute`.
//! * `participants(wave_id, address)`: the participants of the
//!   conversations the user is on.
//! * `labels(wave_id, label)`: the user's own labels (ensembles).
//! * `texts(wave_id, title, content)`: the text of each conversation the
//!   user is on, as indexed, in an FTS5 table: `texts MATCH 'relay'`.
//!
//! The first column of the result is the wave id; the order of the rows
//! is the order of the result. Limits: one read-only statement, no
//! `ATTACH` or `PRAGMA`, a time budget, a row cap, a cap on value sizes.
//! At most one query per user runs at a time, and at most
//! [`CONCURRENCY`] in all, on threads of their own, so a slow query does
//! not hold up wave loading for anyone.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use resonator_model::id::util::build_user_data_wavelet_id;
use resonator_model::id::{IdSerialiser, ModernIdSerialiser, ParticipantId, WaveId};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::limits::Limit;
use rusqlite::{Connection, params};
use tokio::sync::Semaphore;

use super::index::{box_state_sql, visible_sql};
use crate::storage::{Database, StorageError};

/// Longest a query may run.
pub const TIME_BUDGET: Duration = Duration::from_millis(250);
/// Most rows a query may return.
pub const MAX_ROWS: usize = 5000;
/// Queries (and copies being made) at a time, over all users.
pub const CONCURRENCY: usize = 2;
/// Users whose copy is kept.
const CACHED_USERS: usize = 16;
/// Longest query text, in bytes.
const MAX_SQL_BYTES: i32 = 64 * 1024;
/// Largest string or blob a query may make, in bytes.
const MAX_VALUE_BYTES: i32 = 1024 * 1024;

/// Why a SQL search failed.
#[derive(Debug)]
pub enum SqlError {
    /// The query is wrong or broke a limit; the message is for the user.
    Query(String),
    Storage(StorageError),
}

impl From<StorageError> for SqlError {
    fn from(error: StorageError) -> Self {
        SqlError::Storage(error)
    }
}

/// A user's copy, made at a search generation.
struct Copy {
    generation: String,
    conn: Connection,
}

#[derive(Default)]
struct Slot {
    copy: Option<Copy>,
}

/// The SQL searches of a server.
pub struct SqlSearch {
    permits: Semaphore,
    users: Mutex<UserSlots>,
}

#[derive(Default)]
struct UserSlots {
    tick: u64,
    slots: HashMap<String, (u64, Arc<tokio::sync::Mutex<Slot>>)>,
}

impl Default for SqlSearch {
    fn default() -> Self {
        SqlSearch {
            permits: Semaphore::new(CONCURRENCY),
            users: Mutex::new(UserSlots::default()),
        }
    }
}

impl SqlSearch {
    fn slot(&self, user: &str) -> Arc<tokio::sync::Mutex<Slot>> {
        let mut users = self.users.lock().unwrap_or_else(|e| e.into_inner());
        users.tick += 1;
        let tick = users.tick;
        let slot = users
            .slots
            .entry(user.to_owned())
            .or_insert_with(|| (tick, Arc::default()));
        slot.0 = tick;
        let slot = Arc::clone(&slot.1);
        if users.slots.len() > CACHED_USERS {
            // Forget the least recently used copy nobody is using.
            let oldest = users
                .slots
                .iter()
                .filter(|(_, (_, slot))| Arc::strong_count(slot) == 1)
                .min_by_key(|(_, (used, _))| *used)
                .map(|(user, _)| user.clone());
            if let Some(oldest) = oldest {
                users.slots.remove(&oldest);
            }
        }
        slot
    }

    /// Runs `query` for `user` and returns the wave ids of the result, in
    /// its order, without duplicates.
    pub async fn run(
        &self,
        db: &Database,
        user: &ParticipantId,
        shared: Option<&ParticipantId>,
        full_text: bool,
        generation: &str,
        query: &str,
    ) -> Result<Vec<WaveId>, SqlError> {
        let slot = self.slot(user.address());
        // One query per user at a time.
        let mut slot = slot.lock().await;
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| SqlError::Query("The server is shutting down.".to_owned()))?;
        let copy = match slot.copy.take() {
            Some(copy) if copy.generation == generation => copy,
            _ => {
                let rows = read_user_rows(db, user, shared, full_text).await?;
                let generation = generation.to_owned();
                run_blocking(move || {
                    Ok(Copy {
                        conn: build_copy(&rows)?,
                        generation,
                    })
                })
                .await?
            }
        };
        let query = query.to_owned();
        let (copy, result) = tokio::task::spawn_blocking(move || {
            let mut copy = copy;
            let result = execute(&mut copy.conn, &query, TIME_BUDGET);
            (copy, result)
        })
        .await
        .map_err(|error| {
            SqlError::Storage(StorageError::Io(std::io::Error::other(error.to_string())))
        })?;
        slot.copy = Some(copy);
        let ids = result.map_err(SqlError::Query)?;
        let mut seen = BTreeSet::new();
        let mut waves = Vec::with_capacity(ids.len());
        for id in ids {
            let wave_id = ModernIdSerialiser.deserialise_wave_id(&id).map_err(|_| {
                SqlError::Query(format!(
                    "The first column must be a wave id like example.com/w+abc; got '{id}'."
                ))
            })?;
            if seen.insert(id) {
                waves.push(wave_id);
            }
        }
        Ok(waves)
    }
}

async fn run_blocking<T, F>(work: F) -> Result<T, SqlError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StorageError> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| {
            SqlError::Storage(StorageError::Io(std::io::Error::other(error.to_string())))
        })?
        .map_err(SqlError::Storage)
}

/// What the user can see, as copied into their database.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct UserRows {
    /// Wave id to (creator, created, last modified, box).
    pub waves: BTreeMap<String, (Option<String>, i64, i64, &'static str)>,
    pub participants: BTreeSet<(String, String)>,
    pub labels: BTreeSet<(String, i64)>,
    /// (wave id, is root, title, content), one per conversation.
    pub texts: Vec<(String, bool, String, String)>,
}

fn wave_text(domain: &str, id: &str) -> String {
    format!("{domain}/{id}")
}

/// Reads what `user` can see from the index. Every row comes from a
/// wavelet the user (or the shared domain participant) is on, or from the
/// user's own user-data wavelet.
pub(crate) async fn read_user_rows(
    db: &Database,
    user: &ParticipantId,
    shared: Option<&ParticipantId>,
    full_text: bool,
) -> Result<UserRows, StorageError> {
    let address = user.address().to_owned();
    let shared = shared.map(|shared| shared.address().to_owned());
    let udw = build_user_data_wavelet_id(user).ok();
    let (udw_domain, udw_id) = udw.as_ref().map_or((String::new(), String::new()), |udw| {
        (udw.domain().to_owned(), udw.id().to_owned())
    });
    db.read(move |conn| {
        let visible = visible_sql("w.id", Some(2));
        let bind = params![address, shared, udw_domain, udw_id];
        // The statements over visible wavelets only use `?1` and `?2`.
        let visible_bind = &bind[..2];
        let mut rows = UserRows::default();
        let mut statement = conn.prepare(&format!(
            "SELECT w.wave_domain, w.wave_id, \
             MAX(CASE WHEN w.is_root = 1 THEN w.creator END), MAX(w.created), \
             MAX(CASE WHEN w.kind = 1 THEN w.last_modified ELSE -1 END), {} \
             FROM search_wavelets w WHERE {visible} \
             GROUP BY w.wave_domain, w.wave_id HAVING MAX(w.kind = 1) = 1",
            box_state_sql(3, 4)
        ))?;
        let found = statement.query_map(bind, |row| {
            Ok((
                wave_text(&row.get::<_, String>(0)?, &row.get::<_, String>(1)?),
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        for row in found {
            let (wave, creator, created, last_modified, state) = row?;
            let state = match state {
                1 => "archive",
                2 => "mute",
                _ => "inbox",
            };
            rows.waves
                .insert(wave, (creator, created, last_modified, state));
        }
        let mut statement = conn.prepare(&format!(
            "SELECT DISTINCT w.wave_domain, w.wave_id, p.participant \
             FROM search_wavelets w JOIN search_participants p ON p.wavelet = w.id \
             WHERE w.kind = 1 AND {visible}"
        ))?;
        let found = statement.query_map(visible_bind, |row| {
            Ok((
                wave_text(&row.get::<_, String>(0)?, &row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in found {
            let (wave, participant) = row?;
            if rows.waves.contains_key(&wave) {
                rows.participants.insert((wave, participant));
            }
        }
        // Only the user's own user-data wavelet (`?2` is not used here).
        let mut statement = conn.prepare(
            "SELECT w.wave_domain, w.wave_id, l.label \
             FROM search_wavelets w JOIN search_labels l ON l.udw = w.id \
             WHERE w.wavelet_domain = ?3 AND w.wavelet_id = ?4 AND w.creator = ?1",
        )?;
        let found = statement.query_map(bind, |row| {
            Ok((
                wave_text(&row.get::<_, String>(0)?, &row.get::<_, String>(1)?),
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in found {
            let (wave, label) = row?;
            if rows.waves.contains_key(&wave) {
                rows.labels.insert((wave, label));
            }
        }
        if full_text {
            let mut statement = conn.prepare(&format!(
                "SELECT w.wave_domain, w.wave_id, w.is_root, t.title, t.content \
                 FROM search_wavelets w JOIN search_text t ON t.rowid = w.id \
                 WHERE w.kind = 1 AND {visible}"
            ))?;
            let found = statement.query_map(visible_bind, |row| {
                Ok((
                    wave_text(&row.get::<_, String>(0)?, &row.get::<_, String>(1)?),
                    row.get::<_, bool>(2)?,
                    row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                ))
            })?;
            for row in found {
                let row = row?;
                if rows.waves.contains_key(&row.0) {
                    rows.texts.push(row);
                }
            }
        }
        Ok(rows)
    })
    .await
}

const SCHEMA: &str = "
CREATE TABLE waves (
    wave_id       TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    creator       TEXT,
    created       INTEGER NOT NULL,
    last_modified INTEGER NOT NULL,
    box           TEXT NOT NULL
);
CREATE TABLE participants (
    wave_id TEXT NOT NULL,
    address TEXT NOT NULL,
    PRIMARY KEY (wave_id, address)
) WITHOUT ROWID;
CREATE INDEX participants_address ON participants(address);
CREATE TABLE labels (
    wave_id TEXT NOT NULL,
    label   INTEGER NOT NULL,
    PRIMARY KEY (wave_id, label)
) WITHOUT ROWID;
CREATE INDEX labels_label ON labels(label);
";

/// Makes the user's private database from their rows and locks it down.
pub(crate) fn build_copy(rows: &UserRows) -> Result<Connection, StorageError> {
    let mut conn = Connection::open_in_memory()?;
    conn.execute_batch(SCHEMA)?;
    let fts = conn
        .execute_batch(
            "CREATE VIRTUAL TABLE texts USING fts5(wave_id UNINDEXED, title, content, \
             tokenize = \"unicode61 remove_diacritics 2\")",
        )
        .is_ok();
    if !fts {
        conn.execute_batch("CREATE TABLE texts (wave_id TEXT, title TEXT, content TEXT)")?;
    }
    // The title of a wave: of its root conversation, else of any.
    let mut titles: HashMap<&str, (bool, &str)> = HashMap::new();
    for (wave, is_root, title, _) in &rows.texts {
        let entry = titles.entry(wave).or_insert((*is_root, title));
        if *is_root && !entry.0 {
            *entry = (true, title);
        }
    }
    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO waves (wave_id, title, creator, created, last_modified, box) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (wave, (creator, created, last_modified, state)) in &rows.waves {
            let title = titles
                .get(wave.as_str())
                .map_or("", |(_, title)| title.trim());
            insert.execute(params![wave, title, creator, created, last_modified, state])?;
        }
        let mut insert =
            tx.prepare("INSERT INTO participants (wave_id, address) VALUES (?1, ?2)")?;
        for (wave, address) in &rows.participants {
            insert.execute(params![wave, address])?;
        }
        let mut insert = tx.prepare("INSERT INTO labels (wave_id, label) VALUES (?1, ?2)")?;
        for (wave, label) in &rows.labels {
            insert.execute(params![wave, label])?;
        }
        let mut insert =
            tx.prepare("INSERT INTO texts (wave_id, title, content) VALUES (?1, ?2, ?3)")?;
        for (wave, _, title, content) in &rows.texts {
            insert.execute(params![wave, title, content])?;
        }
    }
    tx.commit()?;
    lock_down(&mut conn)?;
    Ok(conn)
}

/// From here on the connection only answers single read-only queries.
fn lock_down(conn: &mut Connection) -> Result<(), StorageError> {
    conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    conn.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES)?;
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_VALUE_BYTES)?;
    conn.pragma_update(None, "query_only", true)?;
    conn.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::Select
        | AuthAction::Read { .. }
        | AuthAction::Function { .. }
        | AuthAction::Recursive => Authorization::Allow,
        // FTS5 reads it internally for `texts MATCH`; it is read-only.
        AuthAction::Pragma {
            pragma_name: "data_version",
            pragma_value: None,
        } => Authorization::Allow,
        _ => Authorization::Deny,
    }))?;
    Ok(())
}

/// Runs one query on a user's database; the first column of each row, as
/// text. Errors are messages for the user.
pub(crate) fn execute(
    conn: &mut Connection,
    query: &str,
    budget: Duration,
) -> Result<Vec<String>, String> {
    let deadline = Instant::now() + budget;
    conn.progress_handler(1000, Some(move || Instant::now() > deadline))
        .map_err(|error| error.to_string())?;
    let result = (|| {
        let mut statement = conn.prepare(query.trim()).map_err(describe)?;
        if !statement.readonly() {
            return Err("Only SELECT queries can be run.".to_owned());
        }
        if statement.column_count() == 0 {
            return Err("The query returns no columns.".to_owned());
        }
        let mut rows = statement.query([]).map_err(describe)?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().map_err(describe)? {
            let id = match row.get_ref(0).map_err(describe)? {
                rusqlite::types::ValueRef::Text(text) => String::from_utf8_lossy(text).into_owned(),
                _ => {
                    return Err(
                        "The first column must be the wave id (text like example.com/w+abc)."
                            .to_owned(),
                    );
                }
            };
            ids.push(id);
            if ids.len() >= MAX_ROWS {
                break;
            }
        }
        Ok(ids)
    })();
    let _ = conn.progress_handler(0, None::<fn() -> bool>);
    result
}

fn describe(error: rusqlite::Error) -> String {
    match &error {
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::OperationInterrupted =>
        {
            format!(
                "The query took longer than {} ms and was stopped.",
                TIME_BUDGET.as_millis()
            )
        }
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::AuthorizationForStatementDenied =>
        {
            "Only SELECT queries over waves, participants, labels and texts can be run.".to_owned()
        }
        rusqlite::Error::MultipleStatement => "Only one statement can be run.".to_owned(),
        rusqlite::Error::SqliteFailure(_, Some(message)) => message.clone(),
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Connection {
        let mut rows = UserRows::default();
        for (wave, state, modified) in [
            ("example.com/w+a", "inbox", 30),
            ("example.com/w+b", "archive", 20),
            ("example.com/w+c", "inbox", 10),
        ] {
            rows.waves.insert(
                wave.to_owned(),
                (Some("alice@example.com".to_owned()), 1, modified, state),
            );
            rows.participants
                .insert((wave.to_owned(), "alice@example.com".to_owned()));
        }
        rows.participants
            .insert(("example.com/w+b".to_owned(), "bob@example.com".to_owned()));
        rows.labels.insert(("example.com/w+c".to_owned(), 3));
        rows.texts.push((
            "example.com/w+a".to_owned(),
            true,
            "Relay load".to_owned(),
            "the relay is busy".to_owned(),
        ));
        rows.texts.push((
            "example.com/w+b".to_owned(),
            true,
            "Lease".to_owned(),
            "office lease".to_owned(),
        ));
        build_copy(&rows).unwrap()
    }

    fn run(conn: &mut Connection, query: &str) -> Result<Vec<String>, String> {
        execute(conn, query, TIME_BUDGET)
    }

    #[test]
    fn queries_over_the_tables() {
        let mut conn = sample();
        assert_eq!(
            run(
                &mut conn,
                "SELECT wave_id FROM waves ORDER BY last_modified DESC"
            )
            .unwrap(),
            ["example.com/w+a", "example.com/w+b", "example.com/w+c"]
        );
        assert_eq!(
            run(&mut conn, "SELECT wave_id FROM waves WHERE box = 'archive'").unwrap(),
            ["example.com/w+b"]
        );
        assert_eq!(
            run(
                &mut conn,
                "SELECT wave_id FROM participants WHERE address LIKE 'bob@%'"
            )
            .unwrap(),
            ["example.com/w+b"]
        );
        assert_eq!(
            run(
                &mut conn,
                "SELECT wave_id FROM texts WHERE texts MATCH 'relay'"
            )
            .unwrap(),
            ["example.com/w+a"]
        );
        assert_eq!(
            run(
                &mut conn,
                "SELECT wave_id FROM labels WHERE label = 3 UNION \
                 SELECT wave_id FROM texts WHERE texts MATCH 'title:lease'"
            )
            .unwrap(),
            ["example.com/w+b", "example.com/w+c"]
        );
        assert_eq!(
            run(
                &mut conn,
                "SELECT title FROM waves WHERE wave_id = 'example.com/w+a'"
            )
            .unwrap(),
            ["Relay load"]
        );
        // A query that is SELECT in lower case with a recursive CTE inside.
        assert_eq!(
            run(
                &mut conn,
                "select wave_id from waves where created in \
                 (with recursive n(x) as (select 1 union all select x + 1 from n where x < 3) \
                  select x from n) order by wave_id limit 1"
            )
            .unwrap(),
            ["example.com/w+a"]
        );
    }

    #[test]
    fn nothing_but_one_read_only_select() {
        let mut conn = sample();
        for bad in [
            "SELECT 1; DELETE FROM waves",
            "DELETE FROM waves",
            "INSERT INTO labels VALUES ('x', 1)",
            "UPDATE waves SET box = 'mute'",
            "CREATE TABLE x (y)",
            "DROP TABLE waves",
            "PRAGMA table_info(waves)",
            "SELECT * FROM pragma_table_info('waves')",
            "ATTACH DATABASE '/tmp/x.db' AS other",
            "SELECT load_extension('x')",
            "VACUUM INTO '/tmp/copy.db'",
        ] {
            assert!(run(&mut conn, bad).is_err(), "{bad}");
        }
        // Still intact.
        assert_eq!(
            run(&mut conn, "SELECT wave_id FROM waves").unwrap().len(),
            3
        );
        assert_eq!(
            run(&mut conn, "SELECT 1; SELECT 2").unwrap_err(),
            "Only one statement can be run."
        );
        assert!(
            run(&mut conn, "SELECT nope FROM waves")
                .unwrap_err()
                .contains("no such column")
        );
        assert!(
            run(&mut conn, "SELECT created FROM waves")
                .unwrap_err()
                .contains("first column")
        );
    }

    #[test]
    fn limits() {
        let mut conn = sample();
        let started = Instant::now();
        let error = execute(
            &mut conn,
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n) \
             SELECT 'example.com/w+a' FROM n WHERE x < 0",
            Duration::from_millis(50),
        )
        .unwrap_err();
        assert!(error.contains("was stopped"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
        // The connection works after an interrupted query.
        assert_eq!(
            run(&mut conn, "SELECT wave_id FROM waves").unwrap().len(),
            3
        );
        // Values are capped.
        assert!(run(&mut conn, "SELECT hex(zeroblob(10000000))").is_err());
        // Rows are capped.
        let rows = run(
            &mut conn,
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 100000) \
             SELECT 'example.com/w+' || x FROM n",
        )
        .unwrap();
        assert_eq!(rows.len(), MAX_ROWS);
    }
}
