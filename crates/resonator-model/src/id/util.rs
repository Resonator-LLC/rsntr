//! Helpers for classifying and building ids (`IdUtil`).

use super::constants::*;
use super::escaper::{DEFAULT_ESCAPER, EscapeError};
use super::participant::ParticipantId;
use super::wave_id::{InvalidIdError, WaveletId};

pub fn is_conversation_root_wavelet_id(wavelet_id: &WaveletId) -> bool {
    wavelet_id.id() == CONVERSATION_ROOT_WAVELET || wavelet_id.id() == "conversation/root"
}

/// Whether the wavelet id is that of a conversational wavelet. Legacy ids
/// without a prefix token count as conversational. Fails if the id is not
/// properly escaped (the original throws `IllegalArgumentException`).
pub fn is_conversational_id(wavelet_id: &WaveletId) -> Result<bool, EscapeError> {
    let token = get_initial_token(wavelet_id.id())?;
    Ok(token == CONVERSATION_WAVELET_PREFIX || token.is_empty())
}

pub fn is_blip_id(id: &str) -> bool {
    has_initial_token_unescaped(id, BLIP_PREFIX) || is_ghost_blip_id(id) || is_legacy_blip_id(id)
}

fn is_legacy_blip_id(id: &str) -> bool {
    // Legacy blips have random ids containing "*"; data documents from that
    // time have a few well-known prefixes.
    id.contains('*')
        && !id.starts_with("m/")
        && !id.starts_with("attach+")
        && !id.starts_with("spell+")
}

pub fn is_ghost_blip_id(id: &str) -> bool {
    has_initial_token_unescaped(id, GHOST_BLIP_PREFIX)
}

pub fn is_attachment_data_document(id: &str) -> bool {
    has_initial_token_unescaped(id, ATTACHMENT_METADATA_PREFIX) || id.starts_with("m/attachment/")
}

pub fn is_manifest_document(id: &str) -> bool {
    id == MANIFEST_DOCUMENT_ID
}

pub fn is_tags_data_document(id: &str) -> bool {
    id == TAGS_DOC_ID
}

pub fn is_robot_doc_id(id: &str) -> bool {
    has_initial_token_unescaped(id, ROBOT_PREFIX)
}

/// The id of the user-data wavelet of `participant`, in the participant's
/// domain.
pub fn build_user_data_wavelet_id(
    participant: &ParticipantId,
) -> Result<WaveletId, InvalidIdError> {
    let id = join(&[USER_DATA_WAVELET_PREFIX, participant.address()]);
    WaveletId::of(participant.domain(), &id)
}

/// The address a user-data wavelet id belongs to, if it is one.
pub fn get_user_data_wavelet_address(wavelet_id: &WaveletId) -> Option<String> {
    match split(wavelet_id.id())?.as_slice() {
        [prefix, address] if prefix == USER_DATA_WAVELET_PREFIX => Some(address.clone()),
        _ => None,
    }
}

pub fn is_user_data_wavelet(wavelet_id: &WaveletId) -> bool {
    has_initial_token_unescaped(wavelet_id.id(), USER_DATA_WAVELET_PREFIX)
}

pub fn is_user_data_wavelet_of(address: &str, wavelet_id: &WaveletId) -> bool {
    get_user_data_wavelet_address(wavelet_id).as_deref() == Some(address)
}

/// The first token of an id with more than one token, otherwise "".
pub fn get_initial_token(id: &str) -> Result<String, EscapeError> {
    let mut tokens = DEFAULT_ESCAPER.split(TOKEN_SEPARATOR, id)?;
    if tokens.len() > 1 {
        Ok(tokens.swap_remove(0))
    } else {
        Ok(String::new())
    }
}

/// Whether `id` starts with `token` followed by the token separator.
/// `token` must not contain the separator.
pub fn has_initial_token_unescaped(id: &str, token: &str) -> bool {
    debug_assert!(!token.contains(TOKEN_SEPARATOR));
    id.strip_prefix(token)
        .is_some_and(|rest| rest.starts_with(TOKEN_SEPARATOR))
}

/// Splits an id into its unescaped tokens; `None` if it is not properly
/// escaped.
pub fn split(id: &str) -> Option<Vec<String>> {
    DEFAULT_ESCAPER.split(TOKEN_SEPARATOR, id).ok()
}

/// Escapes and joins tokens into an id. An empty token list gives "".
pub fn join(tokens: &[&str]) -> String {
    // The only failure of the escaper's join with this separator is an empty
    // token list.
    DEFAULT_ESCAPER
        .join(TOKEN_SEPARATOR, tokens)
        .unwrap_or_default()
}
