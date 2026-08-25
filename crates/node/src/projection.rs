//! Building the caller's projection (projection protocol sec 3).
//!
//! The `_projection` table holds owner-authored points; this module turns
//! the rows at one path into a policy-filtered [`Projection`] for one
//! caller. Filtering runs against the same `_policy` the authenticator
//! enforces: a point the caller cannot invoke is not emitted, which is
//! what makes "the projection never lies" true. The well-known
//! `urn:rsntr:projection-changed` Sympathetic point is appended to every
//! root projection.

use rusqlite::Connection;

use resonator_protocol::vocab::{PROJECTION_CHANGED, XSD_NS};
use resonator_protocol::{Point, PointField, PointKind, Projection, Value};

/// What the projection handler should answer.
pub enum ProjectionOutcome {
    /// The path names no projection this node has (or the caller was never
    /// offered): `rsntr:Error` `point-unknown`.
    UnknownPath,
    /// The caller's filtered projection.
    Built(Projection),
}

/// One `_projection` row, undecoded.
struct PointRow {
    point_iri: String,
    kind: String,
    label: Option<String>,
    icon: Option<String>,
    role: Option<String>,
    projects_path: Option<String>,
    modulation: Option<String>,
    signal: Option<String>,
    params_order: Option<String>,
    fields: Option<String>,
    signal_template: Option<String>,
    fires: Option<String>,
    resource: Option<String>,
}

/// Builds the caller's projection at `path` (empty = root). Runs on the db
/// thread. `id` rides into the response when non-empty. `unfiltered` skips
/// the policy filter (the owner channel: the owner sees every point,
/// docs/owner-channel.md sec 4).
pub fn build_projection(
    conn: &Connection,
    peer: &str,
    id: &str,
    path: &str,
    unfiltered: bool,
) -> Result<ProjectionOutcome, rusqlite::Error> {
    let path = path.trim();
    let mut stmt = conn.prepare(
        "SELECT point_iri, kind, label, icon, role, projects_path, modulation, \
                signal, params_order, fields, signal_template, fires, resource \
         FROM _projection WHERE path = ?1 ORDER BY ord, point_iri",
    )?;
    let rows: Vec<PointRow> = stmt
        .query_map([path], |r| {
            Ok(PointRow {
                point_iri: r.get(0)?,
                kind: r.get(1)?,
                label: r.get(2)?,
                icon: r.get(3)?,
                role: r.get(4)?,
                projects_path: r.get(5)?,
                modulation: r.get(6)?,
                signal: r.get(7)?,
                params_order: r.get(8)?,
                fields: r.get(9)?,
                signal_template: r.get(10)?,
                fires: r.get(11)?,
                resource: r.get(12)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    // The root always exists (it may be empty); any other path must have
    // at least one point row to be a projection at all.
    if rows.is_empty() && !path.is_empty() {
        return Ok(ProjectionOutcome::UnknownPath);
    }

    let mut offers: Vec<Point> = Vec::new();
    for row in rows {
        if unfiltered
            || visible(
                conn,
                peer,
                &row.kind,
                row.modulation.as_deref(),
                row.resource.as_deref(),
            )?
        {
            offers.push(row_to_point(row));
        }
    }
    if path.is_empty() {
        offers.push(projection_changed_point());
    }

    let id = if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    };
    Ok(ProjectionOutcome::Built(Projection { id, offers }))
}

/// The well-known Sympathetic point appended to every root projection.
pub fn projection_changed_point() -> Point {
    Point {
        iri: PROJECTION_CHANGED.into(),
        kind: PointKind::Sympathetic,
        label: Some("projection changed".into()),
        comment: None,
        icon: None,
        role: None,
        projects: None,
        coupling: Vec::new(),
        modulation: None,
        signal: None,
        params_order: Vec::new(),
        signal_template: None,
        fires: None,
    }
}

/// The `_policy` actions that make a point of `kind` visible/invocable.
pub fn actions_for_kind(kind: &str) -> &'static [&'static str] {
    match kind {
        "excitable" => &["write"],
        "sympathetic" => &["entrain", "read"],
        // radiant, bare, and unknown kinds gate on read.
        _ => &["read"],
    }
}

/// Whether an allow row exists for one of `actions` on `resource` for this
/// peer (or the `*` wildcards). A point with no resource is public. This is
/// deliberately the same approximation the help modulation uses
/// (`readable_tables`): allow-row-exists, most-specific deny handling left
/// to the authenticator at invoke time.
///
/// A point in the `chat` modulation is additionally emitted when the
/// caller holds an allow row for action `chat` on the point's resource
/// (chat protocol sec 7.2): the invoke-time gate for chat is the `chat`
/// action, so this keeps "the projection never lies" exact without dummy
/// policy rows.
pub fn visible(
    conn: &Connection,
    peer: &str,
    kind: &str,
    modulation: Option<&str>,
    resource: Option<&str>,
) -> Result<bool, rusqlite::Error> {
    let Some(resource) = resource else {
        return Ok(true);
    };
    let mut actions: Vec<&str> = actions_for_kind(kind).to_vec();
    if modulation.is_some_and(|m| resonator_protocol::mod_matches("chat", m)) {
        actions.push("chat");
    }
    // Same rule for media points: their invoke-time gate is the `media`
    // action on the source name, so that allow row makes them visible.
    if modulation.is_some_and(|m| resonator_protocol::mod_matches("media", m)) {
        actions.push("media");
    }
    if modulation.is_some_and(|m| resonator_protocol::mod_matches("audio-duplex", m)) {
        actions.push("audio-duplex");
    }
    for action in actions {
        let n: i64 = conn.query_row(
            "SELECT count(*) FROM _policy \
             WHERE effect = 'allow' AND action = ?1 \
               AND (table_name = ?2 OR table_name = '*') \
               AND (peer_or_group = ?3 OR peer_or_group = '*')",
            (action, resource, peer),
            |r| r.get(0),
        )?;
        if n > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn row_to_point(row: PointRow) -> Point {
    Point {
        kind: parse_kind(&row.kind),
        iri: row.point_iri,
        label: row.label,
        comment: None,
        icon: row.icon,
        role: row.role,
        projects: row.projects_path,
        coupling: parse_fields(row.fields.as_deref()),
        modulation: row.modulation,
        signal: row.signal,
        params_order: parse_string_array(row.params_order.as_deref()),
        signal_template: row.signal_template,
        fires: row.fires,
    }
}

fn parse_kind(kind: &str) -> PointKind {
    match kind {
        "excitable" => PointKind::Excitable,
        "radiant" => PointKind::Radiant,
        "sympathetic" => PointKind::Sympathetic,
        "bare" => PointKind::Bare,
        // A full class IRI names a custom kind; anything else degrades to
        // a bare navigation point rather than failing.
        iri if iri.contains(':') => PointKind::Other(iri.to_string()),
        _ => PointKind::Bare,
    }
}

fn parse_string_array(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

/// Decodes the `fields` JSON column: an array of
/// `{name, datatype, required, default, one_of, hint}` objects. Malformed
/// entries are skipped rather than failing the whole projection.
fn parse_fields(raw: Option<&str>) -> Vec<PointField> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let name = obj.get("name")?.as_str()?.to_string();
            Some(PointField {
                name,
                datatype: obj
                    .get("datatype")
                    .and_then(|v| v.as_str())
                    .map(expand_datatype),
                required: obj
                    .get("required")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                default: obj.get("default").and_then(json_to_value),
                one_of: obj
                    .get("one_of")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(json_to_value).collect())
                    .unwrap_or_default(),
                hint: obj.get("hint").and_then(|v| v.as_str()).map(str::to_string),
            })
        })
        .collect()
}

/// `xsd:local` and bare `local` shorthands expand to the full xsd IRI;
/// full IRIs pass through.
fn expand_datatype(s: &str) -> String {
    if let Some(local) = s.strip_prefix("xsd:") {
        format!("{XSD_NS}{local}")
    } else if s.contains(':') {
        s.to_string()
    } else {
        format!("{XSD_NS}{s}")
    }
}

fn json_to_value(v: &serde_json::Value) -> Option<Value> {
    match v {
        serde_json::Value::String(s) => Some(Value::Text(s.clone())),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Value::Integer(i))
            } else {
                n.as_f64().map(Value::Real)
            }
        }
        serde_json::Value::Bool(b) => Some(Value::Integer(i64::from(*b))),
        _ => None,
    }
}
