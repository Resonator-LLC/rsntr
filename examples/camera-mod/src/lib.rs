//! The `cameras` mod: a camera wall served as a hologram.
//!
//! A pure-config hologram mod: it needs no database
//! capabilities at all. The `hologram` verb streams the embedded viewer
//! app; the `sources` verb lists the cameras named in the mod's
//! `_modulations.config` (key `cameras`, a JSON array of
//! `{"name": "...", "label": "..."}` objects, written by seed.sql). The
//! video bytes themselves never pass through this mod: the app opens
//! each source over the hologram broker's media lane, which rides the
//! node's builtin `media` modulation and is policy-gated per source
//! (docs/hologram-protocol.md sections 5 and 6).

use extism_pdk::{FnResult, Json, plugin_fn};
use resonator_mod_pdk::{Descriptor, FrameOut, HandleResult, StatementIn, Value, host};

/// The viewer app, embedded at build time and streamed by `hologram`.
const APP_HTML: &str = include_str!("../app/index.html");

/// Raw bytes per Chunk frame; base64 plus Turtle overhead keeps the
/// serialized frame well under the 256 KiB frame cap.
const RAW_CHUNK: usize = 128 * 1024;

#[plugin_fn]
pub fn describe() -> FnResult<Json<Descriptor>> {
    Ok(Json(Descriptor {
        abi: 1,
        name: "cameras".to_string(),
        version: "0.1.0".to_string(),
        help_text: "a camera wall served as a hologram (docs/hologram-protocol.md).\n\
            verbs: hologram (the viewer app), sources (the cameras from the mod\n\
            config, with their talk sources), calls (the latest door call, needs\n\
            a granted db_read and a door_calls table). streams ride the media\n\
            modulation, talk rides audio-duplex, both gated per source."
            .to_string(),
        topics: vec![],
        // db_read is deliberately NOT a need: a wall without a door bell
        // loads with zero caps, and `calls` simply errors there. Granting
        // db_read (beyond the empty needs) enables it.
        needs: vec![],
    }))
}

#[plugin_fn]
pub fn handle(Json(st): Json<StatementIn>) -> FnResult<Json<HandleResult>> {
    let text = st.text.trim();
    let (verb, rest) = match text.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim()),
        None => (text, ""),
    };
    let result = match verb {
        "hologram" => hologram(rest),
        "sources" => sources(),
        "calls" => calls(),
        _ => Ok(HandleResult::error(
            "protocol-error",
            format!("unknown cameras signal {verb:?}; see the cameras help text"),
        )),
    };
    Ok(Json(result?))
}

/// Streams the embedded app: Hologram header, Chunk frames, Done trailer.
fn hologram(path: &str) -> FnResult<HandleResult> {
    if !path.is_empty() {
        return Ok(HandleResult::error(
            "point-unknown",
            format!("the camera wall serves no hologram asset {path:?}"),
        ));
    }
    let bytes = APP_HTML.as_bytes();
    host::emit(
        &FrameOut::new("Hologram")
            .prop(
                "contentType",
                Value::Text("text/html; charset=utf-8".to_string()),
            )
            .prop("size", Value::Integer(bytes.len() as i64)),
    )?;
    let mut chunks = 0i64;
    for (i, slice) in bytes.chunks(RAW_CHUNK).enumerate() {
        host::emit(
            &FrameOut::new("Chunk")
                .prop("seq", Value::Integer(i as i64))
                .prop("data", Value::blob_from_bytes(slice)),
        )?;
        chunks += 1;
    }
    host::emit_done(chunks)?;
    Ok(HandleResult::done())
}

/// One row per configured camera: (name, label). The list is deployment
/// config, not data; opening a stream is still gated per source by the
/// node's `_policy` media rows, so a listed camera a caller may not view
/// simply answers Denied when opened.
fn sources() -> FnResult<HandleResult> {
    let raw = match host::config_get("cameras")?.filter(|c| !c.is_empty()) {
        Some(raw) => raw,
        None => {
            return Ok(HandleResult::error(
                "engine-error",
                "no cameras configured; apply seed.sql",
            ));
        }
    };
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_str(&raw) else {
        return Ok(HandleResult::error(
            "engine-error",
            "the cameras config key is not a JSON array",
        ));
    };
    host::emit_result(&["name", "label", "talk"])?;
    let mut n = 0i64;
    for entry in &entries {
        let Some(name) = entry.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let label = entry
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or(name);
        let talk = entry
            .get("talk")
            .and_then(|v| v.as_str())
            .map(|t| Value::Text(t.to_string()))
            .unwrap_or(Value::Null);
        n += 1;
        host::emit_row(
            n,
            vec![
                ("name".to_string(), Value::Text(name.to_string())),
                ("label".to_string(), Value::Text(label.to_string())),
                ("talk".to_string(), talk),
            ],
        )?;
    }
    host::emit_done(n)?;
    Ok(HandleResult::done())
}

/// The latest door call, for the ring banner: one row (call_id, state,
/// at) or none. Runs as the requesting peer, so viewers need read on
/// `door_calls`; on a wall without the bell wiring this simply errors
/// and the app shows no call UI.
fn calls() -> FnResult<HandleResult> {
    let out = host::db_query(
        "SELECT call_id, state, at FROM door_calls ORDER BY at DESC, call_id DESC LIMIT 1",
        &[],
    )?;
    host::emit_result(&["call_id", "state", "at"])?;
    for (i, row) in out.rows.iter().enumerate() {
        let cells = out.columns.iter().cloned().zip(row.iter().cloned()).collect();
        host::emit_row(i as i64 + 1, cells)?;
    }
    host::emit_done(out.rows.len() as i64)?;
    Ok(HandleResult::done())
}
