//! Wave tools: hand-shaped actions on waves for language models (the
//! built-in agents' tools and the MCP endpoint). Every tool runs as Robots
//! API operations as one participant, so the executor's access checks
//! decide what can be read and changed: a caller can never touch a wave it
//! is not a participant of.
//!
//! Text is addressed by matching rather than by offset where possible;
//! where offsets appear they are in UTF-16 code units of the blip's text
//! as `read_wave` shows it (the first line break of a blip counts as one
//! unit at offset 0, so the visible text starts at 1).

use async_trait::async_trait;
use resonator_model::id::ParticipantId;
use resonator_robots::api::{JsonRpcResponse, OperationRequest};
use resonator_robots::events::Reactor;
use resonator_robots::ops::{OperationExecutor, Registry};
use serde_json::{Value, json};

use crate::outline::{RenderOptions, WaveView, render};

/// Runs operations as someone.
#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, operations: Vec<OperationRequest>) -> Vec<JsonRpcResponse>;
    fn actor(&self) -> &ParticipantId;
}

/// Operations through the executor as a participant.
pub struct ExecutorRunner<'a> {
    pub executor: &'a OperationExecutor,
    pub actor: ParticipantId,
    pub registry: Registry,
}

#[async_trait]
impl Runner for ExecutorRunner<'_> {
    async fn run(&self, operations: Vec<OperationRequest>) -> Vec<JsonRpcResponse> {
        self.executor
            .execute_detailed(&self.actor, self.registry, &operations, None)
            .await
            .responses_with_submit_errors()
    }

    fn actor(&self) -> &ParticipantId {
        &self.actor
    }
}

#[async_trait]
impl Runner for Reactor {
    async fn run(&self, operations: Vec<OperationRequest>) -> Vec<JsonRpcResponse> {
        self.execute(operations)
            .await
            .responses_with_submit_errors()
    }

    fn actor(&self) -> &ParticipantId {
        self.account_id()
    }
}

/// A tool failure, with text that tells the caller what to do.
pub type ToolResult<T = Value> = Result<T, String>;

/// Which wavelet: `wave_id` like `example.com/w+abc`, and the wavelet,
/// by default the conversation root `<domain>/conv+root`. A wave
/// reference `example.com/w+abc/~/conv+root` is accepted as `wave_id`.
pub fn wavelet_of(wave_id: &str, wavelet_id: Option<&str>) -> ToolResult<(String, String)> {
    let wave_id = wave_id.trim().trim_start_matches("wave://");
    let (wave, wavelet_from_ref) = match wave_id.split_once("/~/") {
        Some((wave, wavelet)) => (wave.to_owned(), Some(wavelet.to_owned())),
        None => (wave_id.to_owned(), None),
    };
    let Some((domain, id)) = wave.split_once('/') else {
        return Err(format!(
            "'{wave_id}' is not a wave id; wave ids look like example.com/w+abc123 (see search_waves)"
        ));
    };
    if domain.is_empty() || id.is_empty() || id.contains('/') {
        return Err(format!("'{wave_id}' is not a wave id"));
    }
    let wavelet = wavelet_id
        .map(str::to_owned)
        .or(wavelet_from_ref.map(|w| {
            if w.contains('/') {
                w
            } else {
                format!("{domain}/{w}")
            }
        }))
        .unwrap_or_else(|| format!("{domain}/conv+root"));
    Ok((wave, wavelet))
}

fn op(id: &str, method: &str, params: Value) -> ToolResult<OperationRequest> {
    OperationRequest::from_value(&json!({"id": id, "method": method, "params": params}))
        .map_err(|e| format!("invalid request: {e}"))
}

/// Runs one operation and returns its data, or its error as text.
pub async fn run_one(runner: &dyn Runner, method: &str, params: Value) -> ToolResult {
    let request = op("op", method, params)?;
    let responses = runner.run(vec![request]).await;
    let Some(response) = responses.into_iter().next() else {
        return Err("no response".to_owned());
    };
    if let Some(message) = response.error_message() {
        return Err(friendly(message));
    }
    Ok(serde_json::to_value(&response)
        .ok()
        .and_then(|v| v.get("data").cloned())
        .unwrap_or(Value::Null))
}

/// Server messages made a little more helpful.
fn friendly(message: &str) -> String {
    if message.contains("not a participant") || message.contains("access") {
        format!("{message} (you can only use waves you are a participant of)")
    } else {
        message.to_owned()
    }
}

/// Fetches a wavelet as the runner's participant.
pub async fn fetch(runner: &dyn Runner, wave: &str, wavelet: &str) -> ToolResult<WaveView> {
    let data = run_one(
        runner,
        "robot.fetchWave",
        json!({"waveId": wave, "waveletId": wavelet}),
    )
    .await
    .map_err(|e| format!("cannot read wave {wave}: {e}"))?;
    WaveView::from_fetch(&data).ok_or_else(|| format!("wave {wave} has no conversation"))
}

/// The blips of a thread (`t+…`) or the one blip.
fn selection(
    view: &WaveView,
    blip_id: Option<&str>,
    thread_id: Option<&str>,
) -> ToolResult<Option<Vec<String>>> {
    if let Some(thread) = thread_id {
        let blips = view
            .threads
            .get(thread)
            .ok_or_else(|| format!("there is no thread {thread} in this wave"))?;
        // The thread with the replies of its blips.
        let order = view.order();
        let mut selected = Vec::new();
        for blip in blips {
            if let Some(position) = order.iter().position(|(id, _)| id == blip) {
                let depth = order[position].1;
                selected.push(blip.clone());
                for (id, d) in &order[position + 1..] {
                    if *d <= depth {
                        break;
                    }
                    selected.push(id.clone());
                }
            }
        }
        return Ok(Some(selected));
    }
    if let Some(blip) = blip_id {
        if !view.blips.contains_key(blip) {
            return Err(format!("there is no blip {blip} in this wave"));
        }
        return Ok(Some(vec![blip.to_owned()]));
    }
    Ok(None)
}

/// `read_wave`: the outline of a wave, of one blip or of one thread.
pub async fn read_wave(
    runner: &dyn Runner,
    wave: &str,
    wavelet: &str,
    blip_id: Option<&str>,
    thread_id: Option<&str>,
    budget: usize,
) -> ToolResult<String> {
    let view = fetch(runner, wave, wavelet).await?;
    let only = selection(&view, blip_id, thread_id)?;
    let full = only.is_some();
    let text = render(
        &view,
        &RenderOptions {
            budget,
            keep: if full {
                only.clone().unwrap_or_default()
            } else {
                Vec::new()
            },
            mark: None,
            only,
        },
    );
    Ok(cap(&text, budget))
}

/// Cuts a text to `max` characters with a marker.
pub fn cap(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!(
            "{}\n[… truncated; read a single blip or thread to see more …]",
            &text[..end]
        ),
        None => text.to_owned(),
    }
}

/// `search_waves`: digests of the caller's waves.
pub async fn search(runner: &dyn Runner, query: &str, limit: i32) -> ToolResult {
    let data = run_one(
        runner,
        "robot.search",
        json!({"query": query, "index": 0, "numResults": limit.clamp(1, 50)}),
    )
    .await?;
    let digests = data["searchResults"]["digests"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Ok(Value::Array(
        digests
            .iter()
            .map(|d| {
                json!({
                    "wave_id": d["waveId"],
                    "title": d["title"],
                    "snippet": d["snippet"],
                    "participants": d["participants"],
                    "unread": d["unreadCount"],
                    "blips": d["blipCount"],
                    "last_modified": crate::outline::format_time(d["lastModified"].as_i64().unwrap_or(0)),
                })
            })
            .collect(),
    ))
}

/// Text as a blip's content: starting with a line break, `\r` removed.
pub fn blip_content(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    format!("\n{}", text.trim_end_matches('\n'))
}

fn blip_data(wave: &str, wavelet: &str, temp: &str, text: &str) -> Value {
    json!({"waveId": wave, "waveletId": wavelet, "blipId": temp, "content": blip_content(text)})
}

fn new_blip_id(data: &Value) -> ToolResult<String> {
    data["newBlipId"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "the blip was not created".to_owned())
}

/// `create_wave`: a new wave with a title line and text.
pub async fn create_wave(
    runner: &dyn Runner,
    domain: &str,
    title: &str,
    text: &str,
    participants: &[String],
) -> ToolResult {
    let mut all = vec![runner.actor().address().to_owned()];
    all.extend(participants.iter().cloned());
    let create = op(
        "create",
        "robot.createWavelet",
        json!({"waveletData": {
            "waveId": format!("{domain}/TBD_wave"),
            "waveletId": format!("{domain}/conv+root"),
            "rootBlipId": "TBD_blip",
            "participants": all,
        }}),
    )?;
    let mut content = title.trim().to_owned();
    if !text.trim().is_empty() {
        content.push('\n');
        content.push_str(text.trim_end());
    }
    let write = op(
        "write",
        "document.modify",
        json!({"waveId": format!("{domain}/TBD_wave"), "waveletId": format!("{domain}/conv+root"),
               "blipId": "TBD_blip",
               "modifyAction": {"modifyHow": "INSERT_AFTER", "values": [content]}}),
    )?;
    // The title is the first line, but the digest (and so search and the
    // inbox) reads it from the title annotation, which writing text does
    // not set.
    let mut ops = vec![create, write];
    if !title.trim().is_empty() {
        ops.push(op(
            "title",
            "wavelet.setTitle",
            json!({"waveId": format!("{domain}/TBD_wave"), "waveletId": format!("{domain}/conv+root"),
                   "waveletTitle": title.trim()}),
        )?);
    }
    let responses = runner.run(ops).await;
    for response in &responses {
        if let Some(message) = response.error_message() {
            return Err(friendly(message));
        }
    }
    let data = responses
        .first()
        .and_then(|r| serde_json::to_value(r).ok())
        .map(|v| v["data"].clone())
        .unwrap_or_default();
    Ok(json!({"wave_id": data["waveId"], "root_blip_id": data["blipId"]}))
}

/// `append_blip`: a new blip at the end of the wave.
pub async fn append_blip(runner: &dyn Runner, wave: &str, wavelet: &str, text: &str) -> ToolResult {
    let data = run_one(
        runner,
        "wavelet.appendBlip",
        json!({"waveId": wave, "waveletId": wavelet, "blipData": blip_data(wave, wavelet, "TBD_new", text)}),
    )
    .await?;
    Ok(json!({"blip_id": new_blip_id(&data)?}))
}

/// `reply_to_blip`: a reply under a blip (a new thread on it), or with
/// `continue_thread` the next blip after it in its thread.
pub async fn reply(
    runner: &dyn Runner,
    wave: &str,
    wavelet: &str,
    blip: &str,
    text: &str,
    continue_thread: bool,
) -> ToolResult {
    let method = if continue_thread {
        "blip.continueThread"
    } else {
        "blip.createChild"
    };
    let data = run_one(
        runner,
        method,
        json!({"waveId": wave, "waveletId": wavelet, "blipId": blip,
               "blipData": blip_data(wave, wavelet, "TBD_reply", text)}),
    )
    .await?;
    Ok(json!({"blip_id": new_blip_id(&data)?}))
}

/// The API text of a blip (with its leading line break).
async fn blip_text(
    runner: &dyn Runner,
    wave: &str,
    wavelet: &str,
    blip: &str,
) -> ToolResult<(WaveView, String)> {
    let view = fetch(runner, wave, wavelet).await?;
    let text = view
        .blips
        .get(blip)
        .map(|b| format!("\n{}", b.text))
        .ok_or_else(|| format!("there is no blip {blip} in this wave"))?;
    Ok((view, text))
}

fn utf16_offset(text: &str, byte: usize) -> usize {
    text[..byte].encode_utf16().count()
}

/// `inline_reply`: a reply anchored in a blip's text, right after the
/// first occurrence of `after_text`.
pub async fn inline_reply(
    runner: &dyn Runner,
    wave: &str,
    wavelet: &str,
    blip: &str,
    after_text: &str,
    text: &str,
) -> ToolResult {
    let (_, content) = blip_text(runner, wave, wavelet, blip).await?;
    let Some(found) = content.find(after_text).filter(|_| !after_text.is_empty()) else {
        return Err(format!(
            "the text '{after_text}' does not occur in blip {blip}; read the blip and quote it exactly"
        ));
    };
    let index = utf16_offset(&content, found + after_text.len());
    let data = run_one(
        runner,
        "document.insertInlineBlip",
        json!({"waveId": wave, "waveletId": wavelet, "blipId": blip, "index": index,
               "blipData": blip_data(wave, wavelet, "TBD_inline", text)}),
    )
    .await?;
    Ok(json!({"blip_id": new_blip_id(&data)?}))
}

/// How to edit a blip's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Replace the first occurrence (or all) of a text.
    Replace {
        find: String,
        with: String,
        all: bool,
    },
    /// Replace UTF-16 code units `start..end` of the blip text.
    Range {
        start: usize,
        end: usize,
        with: String,
    },
    /// Add text at the end; a leading `\n` starts a new line.
    Append { text: String },
}

/// `edit_blip`.
pub async fn edit_blip(
    runner: &dyn Runner,
    wave: &str,
    wavelet: &str,
    blip: &str,
    edit: &Edit,
) -> ToolResult {
    let base = json!({"waveId": wave, "waveletId": wavelet, "blipId": blip});
    let mut params = base.as_object().cloned().unwrap_or_default();
    match edit {
        Edit::Replace { find, with, all } => {
            if find.is_empty() {
                return Err("'find' must not be empty".to_owned());
            }
            let (_, content) = blip_text(runner, wave, wavelet, blip).await?;
            let count = content.matches(find.as_str()).count();
            if count == 0 {
                return Err(format!(
                    "the text '{find}' does not occur in blip {blip}; read the blip and quote it exactly"
                ));
            }
            params.insert(
                "modifyQuery".to_owned(),
                json!({"textMatch": find, "maxRes": if *all { -1 } else { 1 }}),
            );
            params.insert(
                "modifyAction".to_owned(),
                json!({"modifyHow": "REPLACE", "values": [with]}),
            );
            run_one(runner, "document.modify", Value::Object(params)).await?;
            Ok(json!({"replaced": if *all { count } else { 1 }}))
        }
        Edit::Range { start, end, with } => {
            if start >= end || *start == 0 {
                return Err("the range must have start >= 1 and end > start (UTF-16 code units; offset 0 is the blip's first line break)".to_owned());
            }
            params.insert("range".to_owned(), json!({"start": start, "end": end}));
            params.insert(
                "modifyAction".to_owned(),
                json!({"modifyHow": "REPLACE", "values": [with]}),
            );
            run_one(runner, "document.modify", Value::Object(params)).await?;
            Ok(json!({"ok": true}))
        }
        Edit::Append { text } => {
            params.insert(
                "modifyAction".to_owned(),
                json!({"modifyHow": "INSERT_AFTER", "values": [text]}),
            );
            run_one(runner, "document.modify", Value::Object(params)).await?;
            Ok(json!({"ok": true}))
        }
    }
}

/// `delete_blip`.
pub async fn delete_blip(runner: &dyn Runner, wave: &str, wavelet: &str, blip: &str) -> ToolResult {
    run_one(
        runner,
        "blip.delete",
        json!({"waveId": wave, "waveletId": wavelet, "blipId": blip}),
    )
    .await?;
    Ok(json!({"deleted": blip}))
}

/// `set_title`.
pub async fn set_title(runner: &dyn Runner, wave: &str, wavelet: &str, title: &str) -> ToolResult {
    run_one(
        runner,
        "wavelet.setTitle",
        json!({"waveId": wave, "waveletId": wavelet, "waveletTitle": title}),
    )
    .await?;
    Ok(json!({"title": title}))
}

/// `add_participant` / `remove_participant`.
pub async fn participant(
    runner: &dyn Runner,
    wave: &str,
    wavelet: &str,
    address: &str,
    add: bool,
) -> ToolResult {
    if ParticipantId::of(address).is_err() {
        return Err(format!(
            "'{address}' is not an address like name@example.com"
        ));
    }
    let method = if add {
        "wavelet.addParticipant"
    } else {
        "wavelet.removeParticipant"
    };
    run_one(
        runner,
        method,
        json!({"waveId": wave, "waveletId": wavelet, "participantId": address}),
    )
    .await?;
    Ok(json!({"participant": address, "added": add}))
}

/// `mark_read`: the wave, or one blip of it.
pub async fn mark_read(
    runner: &dyn Runner,
    wave: &str,
    wavelet: &str,
    blip: Option<&str>,
) -> ToolResult {
    let mut params = json!({"waveId": wave, "waveletId": wavelet, "modifyHow": "markAsRead"});
    if let Some(blip) = blip {
        params["blipId"] = json!(blip);
    }
    run_one(runner, "robot.folderAction", params).await?;
    Ok(json!({"ok": true}))
}

/// `list_unread`: inbox waves with unread blips.
pub async fn list_unread(runner: &dyn Runner, limit: i32) -> ToolResult {
    let found = search(runner, "in:inbox", 50).await?;
    let unread: Vec<Value> = found
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d["unread"].as_i64().unwrap_or(0) > 0)
        .take(limit.clamp(1, 50) as usize)
        .collect();
    Ok(Value::Array(unread))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wavelet_names() {
        assert_eq!(
            wavelet_of("example.com/w+a", None).unwrap(),
            (
                "example.com/w+a".to_owned(),
                "example.com/conv+root".to_owned()
            )
        );
        assert_eq!(
            wavelet_of("example.com/w+a/~/conv+root", None).unwrap().1,
            "example.com/conv+root"
        );
        assert!(
            wavelet_of("w+a", None)
                .unwrap_err()
                .contains("not a wave id")
        );
        assert!(wavelet_of("example.com/", None).is_err());
    }

    #[test]
    fn contents_and_offsets() {
        assert_eq!(blip_content("a\r\nb\n"), "\na\nb");
        assert_eq!(utf16_offset("\naé😀b", "\naé😀".len()), 5);
        assert!(cap(&"x".repeat(20), 5).contains("truncated"));
    }
}
