//! `rsntr apps`: the app registry of a shared node (docs/shared-node.md).

use std::path::Path;

use anyhow::Result;

use resonator_node::{AppInfo, AppRow, TableDecl};
use resonator_protocol::Value;

use crate::channel::{self, OwnerChannel, Prefer};
use crate::store;

/// `rsntr apps list`: every app registered in the node.
pub fn apps_list(dir: &Path) -> Result<Vec<AppRow>> {
    let conn = store::open_db(dir)?;
    resonator_node::list_apps(&conn).map_err(|e| anyhow::anyhow!("{e}"))
}

/// `rsntr apps declare <app_id> <table> "<comment>"`: registers the app
/// (name/version optional) and declares one table, over the owner
/// channel so a serving process sees the writes as its own.
pub async fn apps_declare(
    dir: &Path,
    prefer: Prefer,
    app: &AppInfo,
    table: &str,
    comment: &str,
) -> Result<AppRow> {
    let statements = {
        let conn = store::open_db(dir)?;
        resonator_node::declare_statements(
            &conn,
            &app.id,
            &[TableDecl {
                name: table.to_string(),
                comment: comment.to_string(),
            }],
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?
    };
    let ch = OwnerChannel::open(dir, prefer).await?;
    declare_over(&ch, app).await?;
    for (sql, params) in statements {
        channel::execute(&ch, &sql, params.into_iter().map(Value::Text).collect()).await?;
    }
    let conn = store::open_db(dir)?;
    resonator_node::list_apps(&conn)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .into_iter()
        .find(|r| r.app_id == app.id)
        .ok_or_else(|| anyhow::anyhow!("app {} vanished after registering", app.id))
}

/// The registration `rsntr apps declare` makes, which is on someone
/// else's behalf as often as its own.
///
/// [`register_over`] restates the whole row from the calling process.
/// That is right when an app registers itself and wrong here: the
/// person running `apps declare` is often describing an app that runs
/// on a phone. It wrote this process's `platform` and `lib_version`
/// over the row, and reset `name` to the raw id and `version` to NULL
/// whenever the flags were omitted, so declaring an app's second
/// table, which is the ordinary way to use this command, turned
/// "Kontrolator 3.1, ios" into "am.resonator.kontrolator, linux" in
/// `apps list` and in the system graph, with nothing to say it had
/// happened. Every field was true about the process that wrote it, and
/// the column headings say they are about the app.
///
/// So: insert when there is no row, and on one that exists touch only
/// `last_seen` and what the caller actually supplied. A row this
/// command creates does still carry the CLI's platform, as a
/// placeholder the app's own registration replaces on its next open
/// (docs/shared-node.md): there is nothing truer to put there, and
/// unlike the overwrite it destroys no answer.
async fn declare_over(ch: &OwnerChannel, app: &AppInfo) -> Result<()> {
    resonator_node::apps::validate_app_id(&app.id).map_err(|e| anyhow::anyhow!("{e}"))?;
    let now = resonator_node::clock::now_rfc3339();
    let given = |v: &Option<String>| v.clone().map(Value::Text).unwrap_or(Value::Null);
    channel::execute(
        ch,
        "INSERT INTO _apps (app_id, name, version, lib_version, platform, first_seen, last_seen) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
         ON CONFLICT(app_id) DO UPDATE SET name = COALESCE(?7, name), \
           version = COALESCE(?3, version), last_seen = ?6",
        vec![
            Value::Text(app.id.clone()),
            // The insert needs a name (the column is NOT NULL); the
            // update must not invent one, hence the second binding.
            Value::Text(app.name.clone().unwrap_or_else(|| app.id.clone())),
            given(&app.version),
            Value::Text(resonator_node::apps::lib_version().to_string()),
            Value::Text(resonator_node::apps::platform().to_string()),
            Value::Text(now),
            given(&app.name),
        ],
    )
    .await?;
    Ok(())
}

/// The registration upsert over an owner channel (the same statement
/// [`resonator_node::register_app`] runs directly). For an app
/// registering ITSELF: every field describes the calling process, so
/// restating them all is the point. [`declare_over`] is the third-party
/// form.
pub async fn register_over(ch: &OwnerChannel, app: &AppInfo) -> Result<()> {
    resonator_node::apps::validate_app_id(&app.id).map_err(|e| anyhow::anyhow!("{e}"))?;
    let now = resonator_node::clock::now_rfc3339();
    channel::execute(
        ch,
        "INSERT INTO _apps (app_id, name, version, lib_version, platform, first_seen, last_seen) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
         ON CONFLICT(app_id) DO UPDATE SET name = excluded.name, version = excluded.version, \
           lib_version = excluded.lib_version, platform = excluded.platform, \
           last_seen = excluded.last_seen",
        vec![
            Value::Text(app.id.clone()),
            Value::Text(app.name.clone().unwrap_or_else(|| app.id.clone())),
            app.version.clone().map(Value::Text).unwrap_or(Value::Null),
            Value::Text(resonator_node::apps::lib_version().to_string()),
            Value::Text(resonator_node::apps::platform().to_string()),
            Value::Text(now),
        ],
    )
    .await?;
    Ok(())
}

/// The JSON shape of one app row.
pub fn app_json(r: &AppRow) -> serde_json::Value {
    serde_json::json!({
        "app_id": r.app_id,
        "name": r.name,
        "version": r.version,
        "lib_version": r.lib_version,
        "platform": r.platform,
        "tables": r.tables,
        "first_seen": r.first_seen,
        "last_seen": r.last_seen,
        "note": r.note,
    })
}
