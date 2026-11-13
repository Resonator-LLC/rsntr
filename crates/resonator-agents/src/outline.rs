//! A wave as text for a model or a tool caller: a threaded outline with
//! blip ids, authors, times and text, cut to a budget.
//!
//! ```text
//! Title: Lunch
//! Wave: example.com/w+abc (wavelet example.com/conv+root)
//! Participants: alice@example.com, bob@example.com
//!
//! [b+1] alice@example.com, 2026-11-11 12:00 UTC
//!   Where shall we go?
//!   [b+3] bob@example.com, 2026-11-11 12:05 UTC (reply)
//!     The usual place.
//! [b+2] carol@example.com, 2026-11-11 12:07 UTC
//!   Count me in.
//! ```
//!
//! What is left out is marked `[… n blips not shown …]`, text cut short
//! ends with `[… cut …]`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::Value;

/// One blip as the outline needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlipInfo {
    pub id: String,
    pub creator: String,
    pub contributors: Vec<String>,
    /// The text, lines separated by `\n`, without the leading newline.
    pub text: String,
    pub modified: i64,
    pub reply_threads: Vec<String>,
    /// The addresses `@`-mentioned in the blip (`mention/user`).
    pub mentions: Vec<String>,
}

/// A wavelet's conversation, from a `robot.fetchWave` response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WaveView {
    pub wave_id: String,
    pub wavelet_id: String,
    pub title: String,
    pub participants: Vec<String>,
    pub creator: String,
    /// The blips of the root thread.
    pub root_thread: Vec<String>,
    pub blips: BTreeMap<String, BlipInfo>,
    /// Reply threads by id: their blips.
    pub threads: BTreeMap<String, Vec<String>>,
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The values of the `mention/user` annotations of a fetched blip.
fn mentions_of(annotations: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for annotation in annotations.as_array().into_iter().flatten() {
        if annotation["name"].as_str() == Some("mention/user")
            && let Some(value) = annotation["value"].as_str()
        {
            let value = value.trim().to_ascii_lowercase();
            if !value.is_empty() && !out.contains(&value) {
                out.push(value);
            }
        }
    }
    out
}

fn text_of(content: &str) -> String {
    content.strip_prefix('\n').unwrap_or(content).to_owned()
}

impl WaveView {
    /// Reads the `data` of a `robot.fetchWave` response.
    pub fn from_fetch(data: &Value) -> Option<WaveView> {
        let wavelet = data.get("waveletData")?;
        let mut view = WaveView {
            wave_id: wavelet["waveId"].as_str().unwrap_or("").to_owned(),
            wavelet_id: wavelet["waveletId"].as_str().unwrap_or("").to_owned(),
            title: wavelet["title"].as_str().unwrap_or("").to_owned(),
            participants: strings(&wavelet["participants"]),
            creator: wavelet["creator"].as_str().unwrap_or("").to_owned(),
            root_thread: strings(&wavelet["rootThread"]["blipIds"]),
            ..WaveView::default()
        };
        if view.root_thread.is_empty()
            && let Some(root) = wavelet["rootBlipId"].as_str()
        {
            view.root_thread.push(root.to_owned());
        }
        if let Some(blips) = data.get("blips").and_then(Value::as_object) {
            for (id, blip) in blips {
                view.blips.insert(
                    id.clone(),
                    BlipInfo {
                        id: id.clone(),
                        creator: blip["creator"].as_str().unwrap_or("").to_owned(),
                        contributors: strings(&blip["contributors"]),
                        text: text_of(blip["content"].as_str().unwrap_or("")),
                        modified: blip["lastModifiedTime"].as_i64().unwrap_or(0),
                        reply_threads: strings(&blip["replyThreadIds"]),
                        mentions: mentions_of(&blip["annotations"]),
                    },
                );
            }
        }
        if let Some(threads) = data.get("threads").and_then(Value::as_object) {
            for (id, thread) in threads {
                view.threads.insert(id.clone(), strings(&thread["blipIds"]));
            }
        }
        if view.title.trim().is_empty()
            && let Some(first) = view.root_thread.first().and_then(|id| view.blips.get(id))
        {
            // Wave titles are the first line of the root blip.
            view.title = first.text.lines().next().unwrap_or("").trim().to_owned();
        }
        Some(view)
    }

    /// Every blip in reading order, with its depth.
    pub fn order(&self) -> Vec<(String, usize)> {
        fn visit(view: &WaveView, blips: &[String], depth: usize, out: &mut Vec<(String, usize)>) {
            for id in blips {
                if out.iter().any(|(seen, _)| seen == id) || depth > 64 {
                    continue;
                }
                out.push((id.clone(), depth));
                if let Some(blip) = view.blips.get(id) {
                    for thread in &blip.reply_threads {
                        if let Some(replies) = view.threads.get(thread) {
                            visit(view, replies, depth + 1, out);
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        visit(self, &self.root_thread, 0, &mut out);
        out
    }

    /// The blip a blip answers (the one before it in its thread, or the
    /// one its thread hangs off) and so on up: the trigger's context.
    pub fn ancestry(&self, blip_id: &str) -> Vec<String> {
        let mut thread_of: HashMap<&str, (Vec<String>, Option<&str>)> = HashMap::new();
        thread_of.insert("", (self.root_thread.clone(), None));
        let mut parent_of_thread: HashMap<&str, &str> = HashMap::new();
        for blip in self.blips.values() {
            for thread in &blip.reply_threads {
                parent_of_thread.insert(thread.as_str(), blip.id.as_str());
            }
        }
        let mut out = Vec::new();
        let mut current = blip_id.to_owned();
        for _ in 0..64 {
            out.push(current.clone());
            let (siblings, parent) = if self.root_thread.contains(&current) {
                (&self.root_thread, None)
            } else if let Some((thread, blips)) = self
                .threads
                .iter()
                .find(|(_, blips)| blips.contains(&current))
            {
                (blips, parent_of_thread.get(thread.as_str()).copied())
            } else {
                break;
            };
            let position = siblings.iter().position(|b| *b == current).unwrap_or(0);
            if position > 0 {
                out.push(siblings[position - 1].clone());
            }
            match parent {
                Some(parent) => current = parent.to_owned(),
                None => break,
            }
        }
        let _ = thread_of;
        out.dedup();
        out
    }

    /// The blips of the thread holding `blip` (the root thread or a reply
    /// thread) and whether it is the root thread.
    fn thread_holding(&self, blip: &str) -> Option<(&[String], bool)> {
        if self.root_thread.iter().any(|b| b == blip) {
            return Some((&self.root_thread, true));
        }
        self.threads
            .values()
            .find(|blips| blips.iter().any(|b| b == blip))
            .map(|blips| (blips.as_slice(), false))
    }

    /// Where an answer to `blip` goes: a reply thread under it, unless it
    /// is the last blip of a reply thread. Then that thread goes on, so a
    /// side conversation stays together instead of indenting further each
    /// turn.
    pub fn answer_placement(&self, blip: &str) -> Placement {
        match self.thread_holding(blip) {
            Some((blips, false)) if blips.last().is_some_and(|last| last == blip) => {
                Placement::After(blip.to_owned())
            }
            _ => Placement::Under(blip.to_owned()),
        }
    }

    /// Where a contribution goes when `by` added the agent: an answer to
    /// the last message `by` wrote, or the end of the wave when they wrote
    /// nothing.
    pub fn added_placement(&self, by: &str) -> Placement {
        self.blips
            .values()
            .filter(|blip| {
                (blip.creator == by || blip.contributors.iter().any(|c| c == by))
                    && !blip.text.trim().is_empty()
                    && self.thread_holding(&blip.id).is_some()
            })
            .max_by_key(|blip| blip.modified)
            .map_or(Placement::End, |blip| self.answer_placement(&blip.id))
    }

    /// Whether anyone but `except` wrote something.
    pub fn has_text_by_others(&self, except: &str) -> bool {
        self.blips
            .values()
            .any(|blip| blip.creator != except && !blip.text.trim().is_empty())
    }
}

/// Where a new blip goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// In a new reply thread under this blip.
    Under(String),
    /// Next after this blip in its thread.
    After(String),
    /// At the end of the root thread.
    End,
}

/// What to render and how much.
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Most characters of the result (roughly; headers count).
    pub budget: usize,
    /// Blips that must be shown (the trigger and its ancestry).
    pub keep: Vec<String>,
    /// The blip to mark as the one being answered.
    pub mark: Option<String>,
    /// Only these blips (a single blip or a thread), in reading order.
    pub only: Option<Vec<String>>,
}

/// Milliseconds since the epoch as `YYYY-MM-DD HH:MM UTC`.
pub fn format_time(millis: i64) -> String {
    if millis <= 0 {
        return "unknown time".to_owned();
    }
    let seconds = millis.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rest / 3600,
        (rest % 3600) / 60
    )
}

fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{} [… cut …]", &text[..end]),
        None => text.to_owned(),
    }
}

fn render_blip(view: &WaveView, id: &str, depth: usize, mark: bool, max_text: usize) -> String {
    let indent = "  ".repeat(depth);
    let Some(blip) = view.blips.get(id) else {
        return format!("{indent}[{id}] (not available)\n");
    };
    let mut out = format!(
        "{indent}[{id}] {}, {}{}{}\n",
        blip.creator,
        format_time(blip.modified),
        if depth > 0 { " (reply)" } else { "" },
        if mark {
            "  <== the message you are answering"
        } else {
            ""
        }
    );
    let text = cut(blip.text.trim_end(), max_text);
    if text.trim().is_empty() {
        out.push_str(&format!("{indent}  (empty)\n"));
    }
    for line in text.lines() {
        out.push_str(&format!("{indent}  {line}\n"));
    }
    out
}

/// The header lines: title, ids, participants.
pub fn header(view: &WaveView) -> String {
    format!(
        "Title: {}\nWave: {} (wavelet {})\nParticipants: {}\n\n",
        if view.title.trim().is_empty() {
            "(untitled)"
        } else {
            view.title.trim()
        },
        view.wave_id,
        view.wavelet_id,
        view.participants.join(", ")
    )
}

/// Renders the outline within the budget. Blips in `keep` (and the first
/// root blip) are always shown, the trigger in full and others cut to a
/// share of the budget; the rest are added newest first while they fit,
/// and shown in reading order with markers for what was left out.
pub fn render(view: &WaveView, options: &RenderOptions) -> String {
    let budget = options.budget.max(500);
    let mut order = view.order();
    if let Some(only) = &options.only {
        order.retain(|(id, _)| only.contains(id));
    }
    let mut out = header(view);
    let per_blip = (budget / 4).max(200);
    let mut chosen: BTreeSet<String> = BTreeSet::new();
    let mut used = out.len();
    let mut rendered: HashMap<String, String> = HashMap::new();
    let mut take = |id: &str, depth: usize, full: bool, used: &mut usize, force: bool| -> bool {
        let mark = options.mark.as_deref() == Some(id);
        let max = if full { budget } else { per_blip };
        let text = render_blip(view, id, depth, mark, max);
        if !force && *used + text.len() > budget {
            return false;
        }
        *used += text.len();
        rendered.insert(id.to_owned(), text);
        true
    };
    let depth_of = |id: &str| order.iter().find(|(b, _)| b == id).map(|(_, d)| *d);
    // Must-haves: the root blip and what to keep.
    let mut must: Vec<String> = Vec::new();
    if options.only.is_none()
        && let Some((first, _)) = order.first()
    {
        must.push(first.clone());
    }
    must.extend(options.keep.iter().cloned());
    for id in &must {
        if chosen.contains(id) {
            continue;
        }
        if let Some(depth) = depth_of(id) {
            let full = options.mark.as_deref() == Some(id.as_str());
            take(id, depth, full, &mut used, true);
            chosen.insert(id.clone());
        }
    }
    // Then the newest first.
    let mut rest: Vec<(String, usize)> = order
        .iter()
        .filter(|(id, _)| !chosen.contains(id))
        .cloned()
        .collect();
    rest.sort_by_key(|(id, _)| std::cmp::Reverse(view.blips.get(id).map_or(0, |b| b.modified)));
    for (id, depth) in rest {
        if take(&id, depth, false, &mut used, false) {
            chosen.insert(id);
        }
    }
    let mut skipped = 0usize;
    for (id, _) in &order {
        match rendered.get(id) {
            Some(text) => {
                if skipped > 0 {
                    out.push_str(&format!("[… {skipped} blips not shown …]\n"));
                    skipped = 0;
                }
                out.push_str(text);
            }
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        out.push_str(&format!("[… {skipped} blips not shown …]\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample() -> WaveView {
        let data = json!({
            "waveletData": {
                "waveId": "example.com/w+a", "waveletId": "example.com/conv+root",
                "title": "Lunch", "participants": ["alice@example.com", "bot@example.com"],
                "rootThread": {"id": "", "blipIds": ["b+1", "b+2", "b+4"]}, "rootBlipId": "b+1"
            },
            "blips": {
                "b+1": {"creator": "alice@example.com", "content": "\nLunch\nWhere?", "lastModifiedTime": 1_000,
                        "replyThreadIds": ["t+1"]},
                "b+3": {"creator": "bob@example.com", "content": "\nThe usual.", "lastModifiedTime": 3_000},
                "b+2": {"creator": "bot@example.com", "content": "\nI suggest pizza.", "lastModifiedTime": 2_000},
                "b+4": {"creator": "alice@example.com", "content": "\nWhy pizza?", "lastModifiedTime": 4_000}
            },
            "threads": {"t+1": {"id": "t+1", "blipIds": ["b+3"]}}
        });
        WaveView::from_fetch(&data).unwrap()
    }

    #[test]
    fn answers_nest_under_root_blips_and_continue_side_threads() {
        let view = sample();
        // A message in the root thread gets an indented reply.
        assert_eq!(view.answer_placement("b+4"), Placement::Under("b+4".into()));
        assert_eq!(view.answer_placement("b+1"), Placement::Under("b+1".into()));
        // The last blip of a reply thread: the side conversation goes on.
        assert_eq!(view.answer_placement("b+3"), Placement::After("b+3".into()));
        // Unknown blips still get a reply under them.
        assert_eq!(view.answer_placement("b+9"), Placement::Under("b+9".into()));
    }

    #[test]
    fn a_reply_thread_with_more_after_gets_a_new_reply() {
        let mut view = sample();
        view.threads
            .insert("t+1".into(), vec!["b+3".into(), "b+5".into()]);
        assert_eq!(view.answer_placement("b+3"), Placement::Under("b+3".into()));
        assert_eq!(view.answer_placement("b+5"), Placement::After("b+5".into()));
    }

    #[test]
    fn when_added_it_answers_the_last_message_of_whoever_added_it() {
        let view = sample();
        assert_eq!(
            view.added_placement("alice@example.com"),
            Placement::Under("b+4".into())
        );
        assert_eq!(
            view.added_placement("bob@example.com"),
            Placement::After("b+3".into())
        );
        assert_eq!(view.added_placement("carol@example.com"), Placement::End);
    }

    #[test]
    fn threaded_outline_in_reading_order() {
        let view = sample();
        assert_eq!(
            view.order(),
            vec![
                ("b+1".to_owned(), 0),
                ("b+3".to_owned(), 1),
                ("b+2".to_owned(), 0),
                ("b+4".to_owned(), 0)
            ]
        );
        let text = render(
            &view,
            &RenderOptions {
                budget: 10_000,
                mark: Some("b+4".to_owned()),
                ..RenderOptions::default()
            },
        );
        assert!(
            text.starts_with("Title: Lunch\nWave: example.com/w+a"),
            "{text}"
        );
        assert!(
            text.contains("[b+1] alice@example.com, 1970-01-01 00:00 UTC\n  Lunch\n  Where?\n")
        );
        assert!(
            text.contains(
                "  [b+3] bob@example.com, 1970-01-01 00:00 UTC (reply)\n    The usual.\n"
            )
        );
        assert!(text.contains("[b+4] alice@example.com, 1970-01-01 00:00 UTC  <== the message"));
        let b3 = text.find("[b+3]").unwrap();
        assert!(text.find("[b+1]").unwrap() < b3 && b3 < text.find("[b+2]").unwrap());
    }

    #[test]
    fn ancestry_follows_threads_up() {
        let view = sample();
        assert_eq!(view.ancestry("b+4"), vec!["b+4", "b+2"]);
        assert_eq!(view.ancestry("b+3"), vec!["b+3", "b+1"]);
    }

    #[test]
    fn truncation_keeps_root_trigger_and_newest() {
        let mut view = sample();
        for i in 0..50 {
            let id = format!("b+x{i}");
            view.root_thread.insert(1, id.clone());
            view.blips.insert(
                id.clone(),
                BlipInfo {
                    id,
                    creator: "carol@example.com".to_owned(),
                    contributors: Vec::new(),
                    text: format!("old filler {i} ").repeat(20),
                    modified: 100 + i,
                    reply_threads: Vec::new(),
                    mentions: Vec::new(),
                },
            );
        }
        let text = render(
            &view,
            &RenderOptions {
                budget: 1500,
                keep: view.ancestry("b+4"),
                mark: Some("b+4".to_owned()),
                only: None,
            },
        );
        assert!(text.contains("[b+1]"), "{text}");
        assert!(
            text.contains("[b+4]") && text.contains("Why pizza?"),
            "{text}"
        );
        assert!(text.contains("[b+2]"), "{text}");
        assert!(text.contains("blips not shown"), "{text}");
        assert!(text.len() < 2500, "{}", text.len());
        // The newest filler made it in before the oldest.
        assert!(!text.contains("old filler 0 "), "{text}");
    }

    #[test]
    fn times() {
        assert_eq!(format_time(1_762_862_400_000), "2025-11-11 12:00 UTC");
        assert_eq!(format_time(0), "unknown time");
    }
}
