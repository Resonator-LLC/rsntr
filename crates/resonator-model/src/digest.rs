//! Wave digests for search results and the inbox: title, snippet,
//! participants, blip and unread counts.
//!
//! Derived from the pure parts of
//! `org.waveprotocol.box.server.waveserver.WaveDigester` and from
//! `org.waveprotocol.box.common.Snippets` of Apache Wave, licensed under
//! the Apache License, Version 2.0.
//!
//! [`build_digest_with_supplement`] is the whole of `WaveDigester.build`.
//! What is left to the server: fetching the wavelets of a wave that a user
//! may see, and serialising the wave id for the client (`ApiIdSerializer`
//! in the original).

use crate::conversation::{Conversation, ConversationBlip, title, wavelet_has_conversation};
use crate::data::{BlipData, WaveletData};
use crate::docop::{BootstrapDocument, DocOpComponent};
use crate::id::constants::{CONVERSATION_ROOT_WAVELET, MANIFEST_DOCUMENT_ID};
use crate::id::util::{is_conversational_id, is_user_data_wavelet};
use crate::id::{ParticipantId, WaveId, WaveletId};
use crate::supplement::{PrimitiveSupplementImpl, SupplementImpl, UserDataSupplement};

/// Maximum length of a digest snippet, in UTF-16 code units.
pub const DIGEST_SNIPPET_LENGTH: usize = 140;

/// Maximum number of participants listed in a digest.
pub const PARTICIPANTS_SNIPPET_LENGTH: usize = 5;

/// The summary of a wave shown in a list of waves (`SearchResult.Digest`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest {
    pub title: String,
    pub snippet: String,
    pub wave_id: WaveId,
    /// The first few participants, as addresses.
    pub participants: Vec<String>,
    /// Milliseconds since the epoch, or -1.
    pub last_modified: i64,
    /// Milliseconds since the epoch, or -1.
    pub created: i64,
    pub unread_count: usize,
    pub blip_count: usize,
    /// The participant's own labels on the wave (folder ids of their
    /// user-data supplement), ascending. An addition to the original, for
    /// ensembles.
    pub labels: Vec<i32>,
}

/// Java's `String.trim`: strips characters up to U+0020 from both ends.
fn java_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= ' ')
}

/// The first `max` UTF-16 code units of a string (`substring(0, max)`),
/// without splitting a surrogate pair: a pair that straddles the limit is
/// left out.
fn truncate_utf16(s: &str, max: usize) -> &str {
    let mut units = 0;
    for (index, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max {
            return &s[..index];
        }
    }
    s
}

fn collate_into(doc: &BootstrapDocument, out: &mut String) {
    for component in doc.as_operation().iter() {
        match component {
            DocOpComponent::Characters(chars) => out.push_str(chars),
            DocOpComponent::ElementStart { tag, .. } if tag == "line" => out.push(' '),
            _ => {}
        }
    }
}

/// The text of documents one after another, with a space for every `line`
/// element, trimmed (`Snippets.collateTextForDocuments`).
pub fn collate_text_for_documents<'a>(
    documents: impl IntoIterator<Item = &'a BootstrapDocument>,
) -> String {
    let mut text = String::new();
    for doc in documents {
        collate_into(doc, &mut text);
    }
    java_trim(&text).to_owned()
}

/// The text of all documents of a wavelet, in order of document id
/// (`Snippets.collateTextForWavelet`; the original's order is that of a
/// hash map).
pub fn collate_text_for_wavelet(wavelet: &WaveletData) -> String {
    collate_text_for_documents(wavelet.documents().map(BlipData::content))
}

/// A snippet of a wavelet of at most `max_snippet_length` UTF-16 code
/// units (`Snippets.renderSnippet`).
///
/// As in the original, the snippet is taken from the document that was
/// modified last: its text and, should that document be the conversation
/// manifest, the text of the blips it lists, in manifest order, until the
/// snippet is long enough. Among documents modified at the same time the
/// first by document id is taken (the original takes the first of a hash
/// map). A wavelet without documents has an empty snippet.
pub fn render_snippet(wavelet: &WaveletData, max_snippet_length: usize) -> String {
    let mut newest: Option<&BlipData> = None;
    let mut newest_lmt = -1;
    for blip in wavelet.documents() {
        if blip.last_modified_time() > newest_lmt {
            newest_lmt = blip.last_modified_time();
            newest = Some(blip);
        }
    }
    let mut snippet = String::new();
    match newest {
        None => snippet.push_str(&collate_text_for_wavelet(wavelet)),
        Some(newest) => {
            snippet.push_str(&collate_text_for_documents([newest.content()]));
            snippet.push(' ');
            for component in newest.content().as_operation().iter() {
                if snippet.encode_utf16().count() >= max_snippet_length {
                    break;
                }
                if let DocOpComponent::ElementStart { tag, attributes } = component
                    && tag == "blip"
                    && let Some(blip_id) = attributes.get("id")
                    && let Some(document) = wavelet.document(blip_id)
                {
                    snippet.push_str(&collate_text_for_documents([document.content()]));
                    snippet.push(' ');
                }
            }
        }
    }
    truncate_utf16(&snippet, max_snippet_length).to_owned()
}

fn is_root(id: &WaveletId) -> bool {
    id.id() == CONVERSATION_ROOT_WAVELET
}

/// The wavelets a digest is made from.
#[derive(Debug, Clone, Copy)]
pub struct DigestWavelets<'a> {
    /// The conversation root wavelet if the wave has one, else the last
    /// other conversational wavelet.
    pub conversation: Option<&'a WaveletData>,
    /// The user-data wavelet created by the participant.
    pub user_data: Option<&'a WaveletData>,
}

/// Picks the wavelets a digest is made from, as `WaveDigester.build` does.
/// Note that the user-data wavelet is recognised by its creator, not by
/// the address in its id.
pub fn select_wavelets<'a>(
    participant: &ParticipantId,
    wavelets: &[&'a WaveletData],
) -> DigestWavelets<'a> {
    let mut root = None;
    let mut other = None;
    let mut user_data = None;
    for &wavelet in wavelets {
        let id = wavelet.wavelet_id();
        if is_root(id) {
            root = Some(wavelet);
        } else if is_conversational_id(id).unwrap_or(false) {
            other = Some(wavelet);
        } else if is_user_data_wavelet(id) && wavelet.creator() == participant {
            user_data = Some(wavelet);
        }
    }
    DigestWavelets {
        conversation: root.or(other),
        user_data,
    }
}

fn first_participants<'a>(
    participants: impl IntoIterator<Item = &'a ParticipantId>,
    into: &mut Vec<String>,
) {
    for participant in participants {
        if into.len() >= PARTICIPANTS_SNIPPET_LENGTH {
            break;
        }
        into.push(participant.address().to_owned());
    }
}

/// The digest of a conversational wave (`WaveDigester.generateDigest`), or
/// `None` if the wavelet has no conversation manifest.
///
/// `is_unread` says whether a blip is unread for the user the digest is
/// for. Title, counts and last-modified time come from the conversation of
/// the root wavelet; for another conversational wavelet they are empty
/// (the original fails with a null pointer in that case).
pub fn generate_digest(
    conversation_wavelet: &WaveletData,
    mut is_unread: impl FnMut(&Conversation<'_>, &ConversationBlip) -> bool,
) -> Option<Digest> {
    if !wavelet_has_conversation(conversation_wavelet) {
        return None;
    }
    let root_conversation = Conversation::read(conversation_wavelet)
        .filter(|_| is_root(conversation_wavelet.wavelet_id()));
    let title = root_conversation
        .as_ref()
        .and_then(|c| c.root_thread().first_blip())
        .and_then(|blip| conversation_wavelet.document(blip.id()))
        .map(|blip| title::extract_title(blip.content()))
        .unwrap_or_default();
    let title = java_trim(&title).to_owned();
    let rendered = render_snippet(conversation_wavelet, DIGEST_SNIPPET_LENGTH);
    let mut snippet = java_trim(&rendered);
    if !title.is_empty()
        && let Some(rest) = snippet.strip_prefix(title.as_str())
    {
        // Strip the title from the snippet if the snippet starts with it.
        snippet = rest;
    }
    let mut participants = Vec::new();
    first_participants(conversation_wavelet.participants(), &mut participants);
    let mut unread_count = 0;
    let mut blip_count = 0;
    let mut last_modified = -1;
    if let Some(conversation) = &root_conversation {
        for blip in conversation.blips_breadth_first() {
            if is_unread(conversation, blip) {
                unread_count += 1;
            }
            if let Some(data) = conversation.blip_data(blip.id()) {
                last_modified = last_modified.max(data.last_modified_time());
            }
            blip_count += 1;
        }
    }
    Some(Digest {
        title,
        snippet: snippet.to_owned(),
        wave_id: conversation_wavelet.wave_id().clone(),
        participants,
        last_modified,
        created: conversation_wavelet.creation_time(),
        unread_count,
        blip_count,
        labels: Vec::new(),
    })
}

fn wave_id_title(wave_id: &WaveId) -> String {
    wave_id
        .serialise()
        .unwrap_or_else(|_| format!("{}/{}", wave_id.domain(), wave_id.id()))
}

/// The digest of a wave that has no wavelets or no conversation
/// (`WaveDigester.generateEmptyorUnknownDigest`).
///
/// Quirks kept from the original: the participants are the first five
/// over all wavelets in the order given; `created` is the larger of the
/// last-modified time so far and the creation time of the last wavelet;
/// the blip count is the number of documents.
pub fn generate_empty_or_unknown_digest(wave_id: &WaveId, wavelets: &[&WaveletData]) -> Digest {
    let title = wave_id_title(wave_id);
    if wavelets.is_empty() {
        return Digest {
            title,
            snippet: "(empty)".to_owned(),
            wave_id: wave_id.clone(),
            participants: Vec::new(),
            last_modified: -1,
            created: -1,
            unread_count: 0,
            blip_count: 0,
            labels: Vec::new(),
        };
    }
    let mut last_modified = -1;
    let mut created = -1;
    let mut documents = 0;
    let mut participants = Vec::new();
    for wavelet in wavelets {
        last_modified = last_modified.max(wavelet.last_modified_time());
        created = last_modified.max(wavelet.creation_time());
        documents += wavelet.documents().count();
        first_participants(wavelet.participants(), &mut participants);
    }
    Digest {
        title,
        snippet: "(unknown)".to_owned(),
        wave_id: wave_id.clone(),
        participants,
        last_modified,
        created,
        unread_count: 0,
        blip_count: documents,
        labels: Vec::new(),
    }
}

/// The digest of a wave for a participant (`WaveDigester.build`).
///
/// `is_unread` gets the user-data wavelet picked for the participant, if
/// any, along with the conversation and blip in question.
///
/// Quirk kept from the original: when the wave has a conversational
/// wavelet without a manifest document, the document count of the
/// "unknown" digest is one too high (looking for the manifest creates the
/// document).
pub fn build_digest(
    participant: &ParticipantId,
    wave_id: &WaveId,
    wavelets: &[&WaveletData],
    mut is_unread: impl FnMut(Option<&WaveletData>, &Conversation<'_>, &ConversationBlip) -> bool,
) -> Digest {
    let selected = select_wavelets(participant, wavelets);
    selected
        .conversation
        .and_then(|wavelet| {
            generate_digest(wavelet, |conversation, blip| {
                is_unread(selected.user_data, conversation, blip)
            })
        })
        .unwrap_or_else(|| {
            let mut digest = generate_empty_or_unknown_digest(wave_id, wavelets);
            if selected
                .conversation
                .is_some_and(|wavelet| wavelet.document(MANIFEST_DOCUMENT_ID).is_none())
            {
                digest.blip_count += 1;
            }
            digest
        })
}

/// The digest of a wave for a participant, with unread counts from the
/// participant's user-data wavelet among `wavelets`, as
/// `WaveDigester.build` computes them: without such a wavelet every blip
/// is unread.
pub fn build_digest_with_supplement(
    participant: &ParticipantId,
    wave_id: &WaveId,
    wavelets: &[&WaveletData],
) -> Digest {
    let selected = select_wavelets(participant, wavelets);
    // A snapshot: the documents are interpreted once, not per blip.
    let state = match selected.user_data {
        Some(user_data) => PrimitiveSupplementImpl::copy_of(&UserDataSupplement::new(user_data)),
        None => PrimitiveSupplementImpl::new(),
    };
    let supplement = SupplementImpl::new(state);
    let mut labels = supplement.folders();
    labels.sort_unstable();
    labels.dedup();
    let mut digest = build_digest(participant, wave_id, wavelets, |_, conversation, blip| {
        conversation.blip_data(blip.id()).is_some_and(|data| {
            // Versions are narrowed as the original's `(int)` cast does.
            supplement.is_blip_unread(
                conversation.wavelet_id(),
                blip.id(),
                data.last_modified_version() as i32,
            )
        })
    });
    digest.labels = labels;
    digest
}
