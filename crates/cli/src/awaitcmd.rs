//! `rsntr await`: the agent wake primitive. Blocks until the next unseen
//! event — an incoming chat message or a new `_inbox` row — emits exactly
//! one JSON line, durably advances a cursor file, and exits (0 on an
//! event, 3 on timeout). One invocation per wake; the harness re-arms.
//!
//! - The cursor file (`{"version":1,"chat_rowid":N,"inbox_rowid":N}`) is
//!   written tmp+fsync+rename. A sibling `<cursor>.lock` holds an
//!   exclusive `flock` for the process lifetime, so two awaiters can
//!   never race one cursor (kernel-owned: released on any exit).
//! - Catch-up first, no daemon needed: unseen rows past the cursor are
//!   emitted oldest-first, one per run. Only when caught up does the
//!   command entrain over the owner control socket (which accepts
//!   `rsntr:Entrain` ungated: socket access is ownership) on the chat
//!   inbox point and the `_inbox` point, and block.
//! - At-least-once: the caller prints the event, then persists the
//!   cursor. A crash between the two re-emits the same event next run;
//!   the event `id` is the dedup key.
//! - A first run (no cursor file) starts chat at now but replays every
//!   `_inbox` row still awaiting a decision: open requests are never
//!   lost to a late start. `--from-start` replays chat history too.
//! - Blocked, the command also re-scans its cursor every
//!   [`RESCAN_INTERVAL`]: a lost vibration costs seconds, never the whole
//!   timeout.

#![cfg(unix)]

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::json;

use resonator_node::INBOX_POINT;
use resonator_node::chat::CHAT_INBOX_POINT;
use resonator_protocol::{EntrainmentEvent, EntrainmentReader};
use resonator_transport::RequestStream;

use crate::events;
use crate::store;

/// Cursor file name inside a node directory (the default).
pub const CURSOR_FILE: &str = "rsntr.await-cursor";

/// The durable position of one awaiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub chat_rowid: i64,
    pub inbox_rowid: i64,
}

impl Cursor {
    pub fn json(&self) -> serde_json::Value {
        json!({ "chat_rowid": self.chat_rowid, "inbox_rowid": self.inbox_rowid })
    }
}

/// The cursor path: `--cursor-file` or `<dir>/rsntr.await-cursor`.
pub fn cursor_path(dir: &Path, explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dir.join(CURSOR_FILE))
}

/// Loads a cursor file; `None` when absent. An unreadable or foreign
/// file is a hard error (never a silent reset: that would replay or
/// skip events).
pub fn load_cursor(path: &Path) -> Result<Option<Cursor>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => bail!("reading the cursor file {}: {e}", path.display()),
    };
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        anyhow::anyhow!(
            "cursor file {} is unreadable ({e}); delete it to restart from now",
            path.display()
        )
    })?;
    if v.get("version").and_then(|x| x.as_i64()) != Some(1) {
        bail!(
            "cursor file {} has an unknown version; delete it to restart from now",
            path.display()
        );
    }
    let get = |k: &str| v.get(k).and_then(|x| x.as_i64());
    match (get("chat_rowid"), get("inbox_rowid")) {
        (Some(c), Some(i)) => Ok(Some(Cursor {
            chat_rowid: c,
            inbox_rowid: i,
        })),
        _ => bail!(
            "cursor file {} is missing its rowids; delete it to restart from now",
            path.display()
        ),
    }
}

/// Persists a cursor durably: write a sibling temp file, fsync, rename.
pub fn save_cursor(path: &Path, cursor: &Cursor) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let text = json!({
        "version": 1,
        "chat_rowid": cursor.chat_rowid,
        "inbox_rowid": cursor.inbox_rowid,
    })
    .to_string();
    {
        use std::io::Write;
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(text.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} over {}", tmp.display(), path.display()))?;
    Ok(())
}

/// An exclusive advisory lock on `<cursor>.lock`, held for the process
/// lifetime. The lock file is a sibling (never renamed) because the
/// cursor file's inode changes on every atomic write.
pub struct CursorLock {
    _file: std::fs::File,
    pub path: PathBuf,
}

impl CursorLock {
    pub fn acquire(cursor: &Path) -> Result<Self> {
        let path = PathBuf::from(format!("{}.lock", cursor.display()));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                bail!("{}", refusal(&path));
            }
            bail!("locking {}: {err}", path.display());
        }
        // Who and since when, for whoever is refused next. Written after
        // the lock is won, so only the holder ever writes it.
        record_holder(&file);
        Ok(Self { _file: file, path })
    }
}

/// Stamps the lock file with this process and the time it took the lock.
fn record_holder(file: &std::fs::File) {
    use std::io::{Seek, Write};
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("{{\"pid\":{},\"since\":{since}}}\n", std::process::id());
    let mut f = file;
    let _ = f.set_len(0);
    let _ = f.rewind();
    let _ = f.write_all(line.as_bytes());
    let _ = f.flush();
}

/// What to say when the lock is already held.
///
/// The bare refusal reads the same whether a healthy wake loop is
/// running or something is wedged, and it is most likely to be run by
/// someone already worried about their node. Naming the holder and its
/// age turns the refusal into an answer: two people read the identical
/// message opposite ways within minutes of each other, one as proof of
/// health and one as a fault.
fn refusal(path: &Path) -> String {
    let tail = ". Run one awaiter per cursor, or give this one its own --cursor-file";
    let Some((pid, since)) = read_holder(path) else {
        return format!("another rsntr await already holds {}{tail}", path.display());
    };
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    let age = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_sub(since))
        .unwrap_or(0);
    if alive {
        format!(
            "another rsntr await already holds {} (pid {pid}, {}). \
             A running wake loop is expected to hold it: this is what a \
             healthy node looks like{tail}",
            path.display(),
            human_age(age),
        )
    } else {
        format!(
            "the lock file {} names pid {pid} ({}), which is no longer running, \
             but the lock is still held. Some other live process holds it{tail}",
            path.display(),
            human_age(age),
        )
    }
}

fn read_holder(path: &Path) -> Option<(i32, u64)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    Some((v["pid"].as_i64()? as i32, v["since"].as_u64()?))
}

fn human_age(secs: u64) -> String {
    match secs {
        0..=90 => format!("held for {secs}s"),
        91..=5400 => format!("held for {}m", secs / 60),
        _ => format!("held for {}h", secs / 3600),
    }
}

/// What one `rsntr await` run produced.
#[derive(Debug)]
pub enum AwaitOutcome {
    /// One event (JSON with the `cursor` echo) and the cursor to persist
    /// AFTER printing it.
    Event(serde_json::Value, Cursor),
    /// Nothing arrived in the window; the cursor may still have advanced
    /// over own sends.
    Timeout(Cursor),
}

/// The oldest unseen event past `cur`, with the cursor advanced: the
/// emitting stream to the event's rowid, the other stream only when it
/// held no events (skipped rows only).
fn catch_up(dir: &Path, self_id: &str, cur: Cursor) -> Result<(Option<serde_json::Value>, Cursor)> {
    let (mc, msgs) = events::new_message_events(dir, cur.chat_rowid, self_id)?;
    let (ic, inbox) = events::new_inbox_events(dir, cur.inbox_rowid)?;
    let (m, i) = (msgs.first(), inbox.first());
    // Oldest first across the two streams (second-precision stamps tie
    // often; the inbox — rarer, more urgent — goes first on a tie).
    let pick_inbox = match (m, i) {
        (None, None) => {
            return Ok((
                None,
                Cursor {
                    chat_rowid: mc,
                    inbox_rowid: ic,
                },
            ));
        }
        (Some(_), None) => false,
        (None, Some(_)) => true,
        (Some(m), Some(i)) => i.received_at <= m.received_at,
    };
    if pick_inbox {
        let ev = i.expect("picked inbox");
        Ok((
            Some(events::inbox_json(ev)),
            Cursor {
                chat_rowid: if msgs.is_empty() { mc } else { cur.chat_rowid },
                inbox_rowid: ev.rowid,
            },
        ))
    } else {
        let ev = m.expect("picked message");
        Ok((
            Some(events::message_json(ev)),
            Cursor {
                chat_rowid: ev.rowid,
                inbox_rowid: if inbox.is_empty() {
                    ic
                } else {
                    cur.inbox_rowid
                },
            },
        ))
    }
}

fn with_cursor(mut v: serde_json::Value, cur: &Cursor) -> serde_json::Value {
    v["cursor"] = cur.json();
    v
}

/// Entrains `point` over one fresh owner-channel connection.
async fn entrain_socket(
    dir: &Path,
    point: &str,
) -> Result<(crate::owner::OwnerStream, EntrainmentReader)> {
    let (stream, reader, _id) = crate::owner::entrain(dir, point).await?;
    Ok((stream, reader))
}

/// Runs one await: catch up, else entrain over the owner socket and block
/// until an event or the timeout (`None` = indefinitely).
pub async fn await_event(
    dir: &Path,
    cursor: Cursor,
    timeout: Option<Duration>,
) -> Result<AwaitOutcome> {
    let self_id = store::node_id(dir)?.to_string();

    // Catch-up needs no daemon.
    let (ev, mut cur) = catch_up(dir, &self_id, cursor)?;
    if let Some(ev) = ev {
        return Ok(AwaitOutcome::Event(with_cursor(ev, &cur), cur));
    }

    // Caught up: block on the daemon's vibrations.
    let chat = if events::table_exists(dir, "chat_messages") {
        Some(entrain_socket(dir, CHAT_INBOX_POINT).await?)
    } else {
        None
    };
    let mut inbox = entrain_socket(dir, INBOX_POINT).await?;
    let mut chat_stream = chat;
    let deadline = timeout.map(|t| tokio::time::Instant::now() + t);
    // Belt and braces: the vibration gives sub-second latency; a periodic
    // re-scan of the cursor bounds the damage when a notification is ever
    // lost (a missed wake once cost an agent its whole --timeout).
    let rescan = rescan_interval();
    let mut next_rescan = tokio::time::Instant::now() + rescan;

    loop {
        let frame = tokio::select! {
            f = async {
                match chat_stream.as_mut() {
                    Some((s, _)) => s.recv().await,
                    None => std::future::pending().await,
                }
            } => ("chat", f),
            f = inbox.0.recv() => ("inbox", f),
            _ = tokio::time::sleep_until(next_rescan) => {
                next_rescan = tokio::time::Instant::now() + rescan;
                let (ev, next) = catch_up(dir, &self_id, cur)?;
                cur = next;
                if let Some(ev) = ev {
                    return Ok(AwaitOutcome::Event(with_cursor(ev, &cur), cur));
                }
                continue;
            }
            _ = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => {
                return Ok(AwaitOutcome::Timeout(cur));
            }
        };
        let (which, frame) = frame;
        let frame =
            frame.map_err(|e| anyhow::anyhow!("receiving from the {which} entrainment: {e}"))?;
        let Some(frame) = frame else {
            bail!("the serving node closed the {which} entrainment stream");
        };
        let reader = match which {
            "chat" => &mut chat_stream.as_mut().expect("chat stream present").1,
            _ => &mut inbox.1,
        };
        match reader.accept(frame).context("entrainment choreography")? {
            EntrainmentEvent::Entrained | EntrainmentEvent::Vibration(_) => {
                let (ev, next) = catch_up(dir, &self_id, cur)?;
                cur = next;
                if let Some(ev) = ev {
                    return Ok(AwaitOutcome::Event(with_cursor(ev, &cur), cur));
                }
            }
            EntrainmentEvent::Damped => bail!("the serving node damped the {which} entrainment"),
            EntrainmentEvent::Denied(d) => bail!(
                "{which} entrainment denied: {}",
                d.reason.unwrap_or_else(|| "(no reason given)".into())
            ),
            EntrainmentEvent::Error(e) => {
                bail!(
                    "{which} entrainment error {}: {}",
                    e.code,
                    e.reason.unwrap_or_default()
                )
            }
        }
    }
}

/// How often a blocked await re-scans its cursor regardless of
/// vibrations: 5 s by default, `RSNTR_AWAIT_RESCAN_MS` overrides
/// (tests use a short one).
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(5);

fn rescan_interval() -> Duration {
    std::env::var("RSNTR_AWAIT_RESCAN_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|ms| Duration::from_millis(ms.max(50)))
        .unwrap_or(RESCAN_INTERVAL)
}

/// The starting cursor for a first run. Chat starts at now (old
/// conversation is history, not a wake); the inbox starts just before
/// the oldest row still awaiting a decision, so an agent that boots
/// after a knock or a parked request landed is woken for it rather than
/// silently past it. `from_start` replays everything.
pub fn initial_cursor(dir: &Path, from_start: bool) -> Result<Cursor> {
    if from_start {
        return Ok(Cursor {
            chat_rowid: 0,
            inbox_rowid: 0,
        });
    }
    let inbox_rowid = match events::oldest_pending_inbox_rowid(dir)? {
        Some(rowid) => rowid - 1,
        None => events::max_rowid(dir, "_inbox")?,
    };
    Ok(Cursor {
        chat_rowid: events::max_rowid(dir, "chat_messages")?,
        inbox_rowid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips_and_rejects_garbage() {
        let tmp = crate::testutil::TempDir::new("await-cursor");
        let path = tmp.path().join("c");
        assert!(load_cursor(&path).unwrap().is_none());
        let c = Cursor {
            chat_rowid: 7,
            inbox_rowid: 3,
        };
        save_cursor(&path, &c).unwrap();
        assert_eq!(load_cursor(&path).unwrap(), Some(c));
        std::fs::write(&path, "not json").unwrap();
        assert!(load_cursor(&path).is_err());
        std::fs::write(&path, r#"{"version":9,"chat_rowid":1,"inbox_rowid":1}"#).unwrap();
        assert!(load_cursor(&path).is_err());
    }

    #[test]
    fn lock_excludes_a_second_holder() {
        let tmp = crate::testutil::TempDir::new("await-lock");
        let path = tmp.path().join("c");
        let first = CursorLock::acquire(&path).unwrap();
        let second = CursorLock::acquire(&path);
        assert!(second.is_err(), "second acquire must fail while held");
        drop(first);
        assert!(CursorLock::acquire(&path).is_ok(), "released on drop");
    }

    /// The refusal is read by someone worried about their node, so it
    /// has to say whether what it found is a healthy wake loop or a
    /// problem. Naming the live holder answers that; the bare message
    /// was read both ways by two people within minutes.
    #[test]
    fn refusal_names_the_live_holder_and_calls_it_healthy() {
        let tmp = crate::testutil::TempDir::new("await-lock-msg");
        let path = tmp.path().join("c");
        let _held = CursorLock::acquire(&path).unwrap();
        let err = match CursorLock::acquire(&path) {
            Ok(_) => panic!("the lock must still be held"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains(&format!("pid {}", std::process::id())),
            "names the holder: {err}"
        );
        assert!(err.contains("held for"), "says how long: {err}");
        assert!(
            err.contains("healthy"),
            "says an expected holder is not a fault: {err}"
        );
        assert!(
            err.contains("--cursor-file"),
            "still says the way out: {err}"
        );
    }

    /// An older build left the lock file empty. The refusal must still
    /// be the plain one rather than a claim about a pid it cannot read.
    #[test]
    fn refusal_without_a_stamp_stays_plain() {
        let tmp = crate::testutil::TempDir::new("await-lock-bare");
        let path = tmp.path().join("c");
        std::fs::write(format!("{}.lock", path.display()), b"").unwrap();
        let _held = CursorLock::acquire(&path).unwrap();
        // Blank the stamp the way an older holder would have left it.
        std::fs::write(format!("{}.lock", path.display()), b"").unwrap();
        let err = match CursorLock::acquire(&path) {
            Ok(_) => panic!("the lock must still be held"),
            Err(e) => e.to_string(),
        };
        assert!(!err.contains("pid "), "invents no holder: {err}");
        assert!(
            err.contains("--cursor-file"),
            "still says the way out: {err}"
        );
    }
}
