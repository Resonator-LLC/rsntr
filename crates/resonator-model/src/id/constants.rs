//! Conventional id prefixes and well-known ids (`IdConstants`).

/// Separates tokens in an id.
pub const TOKEN_SEPARATOR: char = '+';

pub const WAVE_URI_SCHEME: &str = "wave";
pub const WAVE_PREFIX: &str = "w";
pub const PROFILE_WAVE_PREFIX: &str = "prof";

pub const CONVERSATION_WAVELET_PREFIX: &str = "conv";
pub const CONVERSATION_ROOT_WAVELET: &str = "conv+root";
pub const USER_DATA_WAVELET_PREFIX: &str = "user";

pub const BLIP_PREFIX: &str = "b";
pub const MANIFEST_DOCUMENT_ID: &str = "conversation";
pub const TAGS_DOC_ID: &str = "tags";
pub const GHOST_BLIP_PREFIX: &str = "g";
pub const ATTACHMENT_METADATA_PREFIX: &str = "attach";
pub const OLD_METADATA_DOC_ID: &str = "m/metadata";
pub const ROBOT_PREFIX: &str = "r";
pub const ROLES_DATA_DOC_ID: &str = "roles";
pub const INDEXABILITY_DATA_DOC_ID: &str = "listing";
