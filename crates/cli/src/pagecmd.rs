//! `rsntr page ...`: publish, list, remove, and fetch HTML pages served
//! as holograms by the `pages` modulation. Local pages ride the owner
//! channel; `--peer` targets a peer's `pages` table (needs a write grant
//! there). Large HTML is published in chunks so each statement fits the
//! frame budget; a torn publish is visible as `updated_at='publishing'`.

use std::path::Path;

use anyhow::{Result, bail};

use resonator_protocol::{Done, RequestKind, Value};

use crate::channel::{self, OwnerChannel, Prefer};
use crate::client::{self, QueryOutcome};

/// A remote page write refused by the peer's policy (exit 2 territory;
/// `pending` marks the parked-for-the-owner case).
#[derive(Debug)]
pub struct PageDenied {
    pub reason: String,
    pub pending: bool,
}

/// HTML larger than this is published in chunks (well under the 256 KiB
/// frame budget once the statement and a text param are serialized).
const CHUNK_BYTES: usize = 120 * 1024;

/// Where a page write goes.
enum Sink<'a> {
    Local(OwnerChannel),
    Remote {
        dir: &'a Path,
        peer: &'a str,
        offline: bool,
    },
}

impl Sink<'_> {
    /// Runs one `sql-sqlite` Execute. A remote policy denial is a typed
    /// `Err(PageDenied)` so callers can exit 2 with the reason (and the
    /// pending marker) instead of a generic error. The `Done` on the ok
    /// side carries `affected_rows`, which is the only way `page rm`
    /// can tell a page it deleted from a name that was never there.
    async fn exec(&self, sql: &str, params: Vec<Value>) -> Result<Result<Done, PageDenied>> {
        match self {
            Sink::Local(ch) => Ok(Ok(channel::execute(ch, sql, params).await?)),
            Sink::Remote { dir, peer, offline } => {
                let report = client::run_statement(
                    dir,
                    peer,
                    RequestKind::Execute,
                    "sql-sqlite",
                    sql,
                    params,
                    *offline,
                    None,
                )
                .await?;
                match report.outcome {
                    QueryOutcome::Rows { done, .. } => Ok(Ok(done)),
                    QueryOutcome::Denied(d) => {
                        let reason = d.reason.unwrap_or_else(|| "(no reason given)".to_string());
                        let pending = reason == resonator_authenticator::PENDING_REASON;
                        Ok(Err(PageDenied { reason, pending }))
                    }
                    QueryOutcome::Failed(e) => {
                        bail!("[{}] {}", e.code, e.reason.unwrap_or_default())
                    }
                    other => bail!("unexpected publish response: {other:?}"),
                }
            }
        }
    }
}

/// What `page publish` did.
#[derive(Debug)]
pub enum PublishOutcome {
    Published {
        name: String,
        bytes: usize,
        chunks: usize,
    },
    /// The peer's policy refused (or parked) the write.
    Denied(PageDenied),
}

/// `rsntr page publish <name> <file>`: upsert the page, chunking large
/// HTML. `peer` targets a remote node's `pages` table.
pub async fn page_publish(
    dir: &Path,
    prefer: Prefer,
    name: &str,
    html: &str,
    content_type: &str,
    peer: Option<&str>,
    offline: bool,
) -> Result<PublishOutcome> {
    let sink = match peer {
        None => Sink::Local(OwnerChannel::open(dir, prefer).await?),
        Some(peer) => Sink::Remote { dir, peer, offline },
    };
    let bytes = html.len();

    if bytes <= CHUNK_BYTES {
        if let Err(denied) = sink
            .exec(
                "INSERT INTO pages (name, html, content_type, updated_at) \
             VALUES (?1, ?2, ?3, datetime('now')) \
             ON CONFLICT(name) DO UPDATE SET html = ?2, content_type = ?3, \
                 updated_at = datetime('now')",
                vec![
                    Value::Text(name.to_string()),
                    Value::Text(html.to_string()),
                    Value::Text(content_type.to_string()),
                ],
            )
            .await?
        {
            return Ok(PublishOutcome::Denied(denied));
        }
        return Ok(PublishOutcome::Published {
            name: name.to_string(),
            bytes,
            chunks: 1,
        });
    }

    // Chunked: seed with the first slice (marker updated_at), append the
    // rest, then a commit marker. char-boundary-safe slicing.
    let mut slices: Vec<&str> = Vec::new();
    let mut start = 0;
    while start < html.len() {
        let mut end = (start + CHUNK_BYTES).min(html.len());
        while end < html.len() && !html.is_char_boundary(end) {
            end -= 1;
        }
        slices.push(&html[start..end]);
        start = end;
    }
    if let Err(denied) = sink
        .exec(
            "INSERT INTO pages (name, html, content_type, updated_at) \
             VALUES (?1, ?2, ?3, 'publishing') \
             ON CONFLICT(name) DO UPDATE SET html = ?2, content_type = ?3, \
                 updated_at = 'publishing'",
            vec![
                Value::Text(name.to_string()),
                Value::Text(slices[0].to_string()),
                Value::Text(content_type.to_string()),
            ],
        )
        .await?
    {
        return Ok(PublishOutcome::Denied(denied));
    }
    for slice in &slices[1..] {
        if let Err(denied) = sink
            .exec(
                "UPDATE pages SET html = html || ?2 WHERE name = ?1",
                vec![
                    Value::Text(name.to_string()),
                    Value::Text(slice.to_string()),
                ],
            )
            .await?
        {
            return Ok(PublishOutcome::Denied(denied));
        }
    }
    if let Err(denied) = sink
        .exec(
            "UPDATE pages SET updated_at = datetime('now') WHERE name = ?1",
            vec![Value::Text(name.to_string())],
        )
        .await?
    {
        return Ok(PublishOutcome::Denied(denied));
    }
    Ok(PublishOutcome::Published {
        name: name.to_string(),
        bytes,
        chunks: slices.len(),
    })
}

/// `rsntr page rm <name>`: delete a page (local or on `peer`); a
/// remote policy denial comes back typed.
///
/// The ok side is the number of rows the delete matched, so the caller
/// can tell a page it took down from a name that was never there. Zero
/// is the answer that matters; `None` means the far side reported no
/// count, which is not evidence either way.
pub async fn page_rm(
    dir: &Path,
    prefer: Prefer,
    name: &str,
    peer: Option<&str>,
    offline: bool,
) -> Result<Result<Option<i64>, PageDenied>> {
    let sink = match peer {
        None => Sink::Local(OwnerChannel::open(dir, prefer).await?),
        Some(peer) => Sink::Remote { dir, peer, offline },
    };
    Ok(sink
        .exec(
            "DELETE FROM pages WHERE name = ?1",
            vec![Value::Text(name.to_string())],
        )
        .await?
        .map(|done| done.affected_rows))
}

/// One `page list` row.
#[derive(Debug)]
pub struct PageRow {
    pub name: String,
    pub content_type: String,
    pub bytes: i64,
    pub updated_at: String,
}

/// `rsntr page list`: local pages over the owner channel.
pub async fn page_list(dir: &Path, prefer: Prefer) -> Result<Vec<PageRow>> {
    let ch = OwnerChannel::open(dir, prefer).await?;
    let (_cols, rows, _done) = channel::query_rows(
        &ch,
        "SELECT name, content_type, length(html) AS bytes, updated_at \
         FROM pages ORDER BY name",
        vec![],
    )
    .await?;
    Ok(rows
        .iter()
        .map(|row| PageRow {
            name: channel::cell_text(row, "name").unwrap_or_default(),
            content_type: channel::cell_text(row, "content_type").unwrap_or_default(),
            bytes: channel::cell_text(row, "bytes")
                .and_then(|b| b.parse().ok())
                .unwrap_or(0),
            updated_at: channel::cell_text(row, "updated_at").unwrap_or_default(),
        })
        .collect())
}
