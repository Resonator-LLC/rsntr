//! The app wavelet of a gadget, `app+<gadget id>` in the gadget's wave:
//! what an app posts with `wave.post`, one data document per message
//! (`msg+<id>`, `<m kind="..." t="<ms>">body</m>`), with the participants
//! of the conversation. It is not conversational, so it shows nowhere in
//! the wave, search or unread counts; the client opens it for the app's Log.
//!
//! A message's sequence number is the wavelet version its document was
//! last changed at; messages are never changed, so that is the version
//! that created it, and versions only grow.

use base64::Engine;
use resonator_apps::WaveMessage;
use resonator_model::data::WaveletData;
use resonator_model::doc::DocView;
use resonator_model::docop::{Attributes, DocOpBuilder};
use resonator_model::id::{ParticipantId, WaveId, WaveletId, WaveletName};
use resonator_model::operation::WaveletDelta;
use resonator_model::version::HashedVersionFactory;
use resonator_model::wavelet::OpBasedWavelet;

/// The prefix of app wavelet ids.
pub const APP_WAVELET_PREFIX: &str = "app+";
/// The prefix of message documents.
pub const MESSAGE_PREFIX: &str = "msg+";

const MESSAGE_TAG: &str = "m";
const KIND_ATTR: &str = "kind";
const TIME_ATTR: &str = "t";

/// Whether a gadget id can name an app wavelet: a token of letters,
/// digits, `-` and `_`.
fn is_gadget_token(gadget: &str) -> bool {
    !gadget.is_empty()
        && gadget.len() <= 128
        && gadget
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The app wavelet of `gadget` in `wave`; `None` for a gadget id that
/// cannot name one.
pub fn app_wavelet(wave: &WaveId, gadget: &str) -> Option<WaveletName> {
    if !is_gadget_token(gadget) {
        return None;
    }
    let id = WaveletId::of(wave.domain(), &format!("{APP_WAVELET_PREFIX}{gadget}")).ok()?;
    Some(WaveletName::of(wave.clone(), id))
}

/// Whether a wavelet is an app wavelet.
pub fn is_app_wavelet(wavelet: &WaveletId) -> bool {
    wavelet.id().starts_with(APP_WAVELET_PREFIX)
}

/// The gadget of an app wavelet.
pub fn gadget_of(wavelet: &WaveletId) -> Option<&str> {
    wavelet
        .id()
        .strip_prefix(APP_WAVELET_PREFIX)
        .filter(|g| is_gadget_token(g))
}

/// The messages of an app wavelet after `since`, in order.
pub fn messages(data: &WaveletData, since: u64) -> Vec<WaveMessage> {
    let mut messages: Vec<WaveMessage> = data
        .documents()
        .filter(|doc| doc.id().starts_with(MESSAGE_PREFIX))
        .filter(|doc| doc.last_modified_version() > since)
        .filter_map(|doc| {
            let view = DocView::new(doc.content());
            let element = view.first_child_element(view.root())?;
            if !view.has_tag(element, MESSAGE_TAG) {
                return None;
            }
            let body: String = view
                .children(element)
                .iter()
                .filter_map(|&child| view.text(child))
                .collect();
            Some(WaveMessage {
                seq: doc.last_modified_version(),
                author: doc.author().address().to_owned(),
                kind: view.attribute(element, KIND_ATTR)?.to_owned(),
                body,
            })
        })
        .collect();
    messages.sort_by_key(|m| m.seq);
    messages
}

/// Characters a document may hold: no control characters but newline and
/// tab, no non-characters.
fn document_text(text: &str) -> String {
    text.chars()
        .filter(|&c| {
            (!c.is_control() || c == '\n' || c == '\t')
                && !matches!(c, '\u{fffe}' | '\u{ffff}' | '\u{fdd0}'..='\u{fdef}')
        })
        .collect()
}

fn message_id() -> String {
    let mut seed = [0u8; 12];
    // Without randomness the time and a counter still make ids unique here.
    if getrandom::fill(&mut seed).is_err() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        seed[..8].copy_from_slice(&n.to_le_bytes());
    }
    format!(
        "{MESSAGE_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(seed)
    )
}

/// The wavelet to edit as `author`: the current one, or a new one (which
/// `author` joins first).
fn editable(
    name: &WaveletName,
    current: Option<WaveletData>,
    author: &ParticipantId,
    now: i64,
) -> Result<OpBasedWavelet, String> {
    let fresh = current.is_none();
    let data = current.unwrap_or_else(|| {
        WaveletData::empty(
            name,
            author.clone(),
            HashedVersionFactory::create_version_zero(name),
            now,
        )
    });
    let mut wavelet = OpBasedWavelet::new(data, author.clone(), now);
    if fresh {
        wavelet.add_participant(author).map_err(|e| e.to_string())?;
    }
    Ok(wavelet)
}

/// Makes the participants those of the conversation: adds the missing,
/// removes those gone (never `author`, who edits).
fn mirror(
    wavelet: &mut OpBasedWavelet,
    root: &[ParticipantId],
    author: &ParticipantId,
) -> Result<(), String> {
    for participant in root {
        wavelet
            .add_participant(participant)
            .map_err(|e| e.to_string())?;
    }
    let gone: Vec<ParticipantId> = wavelet
        .participants()
        .iter()
        .filter(|p| !root.contains(p) && *p != author)
        .cloned()
        .collect();
    for participant in gone {
        wavelet
            .remove_participant(&participant)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The delta posting one message as `author` (creating the wavelet if
/// `current` is `None`), with the participants brought up to date.
pub fn post_delta(
    name: &WaveletName,
    current: Option<WaveletData>,
    root: &[ParticipantId],
    author: &ParticipantId,
    kind: &str,
    body: &str,
    now: i64,
) -> Result<WaveletDelta, String> {
    if current
        .as_ref()
        .is_some_and(|data| !data.participants().contains(author))
    {
        return Err("not a participant of the app wavelet yet".to_owned());
    }
    let mut wavelet = editable(name, current, author, now)?;
    mirror(&mut wavelet, root, author)?;
    let attributes = Attributes::from_pairs([
        (KIND_ATTR.to_owned(), document_text(kind)),
        (TIME_ATTR.to_owned(), now.to_string()),
    ])
    .map_err(|e| e.to_string())?;
    let body = document_text(body);
    let mut builder = DocOpBuilder::new().element_start(MESSAGE_TAG, attributes);
    if !body.is_empty() {
        builder = builder.characters(&body);
    }
    let op = builder.element_end().build().map_err(|e| e.to_string())?;
    wavelet
        .apply_doc_op(&message_id(), op)
        .map_err(|e| e.to_string())?;
    wavelet
        .take_delta()
        .ok_or_else(|| "nothing to post".to_owned())
}

/// The delta bringing the participants of an existing app wavelet up to
/// date as `author`, a participant of it; `None` if they are.
pub fn mirror_delta(
    current: WaveletData,
    root: &[ParticipantId],
    author: &ParticipantId,
    now: i64,
) -> Option<WaveletDelta> {
    if !current.participants().contains(author) {
        return None;
    }
    let same = current.participants().len() == root.len()
        && root.iter().all(|p| current.participants().contains(p));
    if same {
        return None;
    }
    let mut wavelet = OpBasedWavelet::new(current, author.clone(), now);
    mirror(&mut wavelet, root, author).ok()?;
    wavelet.take_delta()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wave() -> WaveId {
        WaveId::of("example.com", "w+1").unwrap()
    }

    #[test]
    fn app_wavelets_are_named_after_the_gadget() {
        let name = app_wavelet(&wave(), "app-Ab_9").unwrap();
        assert_eq!(name.wavelet_id.id(), "app+app-Ab_9");
        assert!(is_app_wavelet(&name.wavelet_id));
        assert_eq!(gadget_of(&name.wavelet_id), Some("app-Ab_9"));
        assert!(app_wavelet(&wave(), "").is_none());
        assert!(app_wavelet(&wave(), "a+b").is_none());
        assert!(app_wavelet(&wave(), "a/b").is_none());
    }

    #[test]
    fn controls_are_dropped_from_document_text() {
        assert_eq!(document_text("a\u{0}b\nc\u{ffff}"), "ab\nc");
    }
}
