//! Schemas of the documents of a user-data wavelet (`UserDataSchemas`).
//!
//! The document shapes are described at
//! [`WaveletBasedSupplement`].

use std::sync::LazyLock;

use super::{
    BOOLEAN_VALUES, SchemaProvider, XmlSchemaConstraints, is_double, is_long,
    is_non_negative_integer, is_valid_integer, is_wavelet_id,
};
use crate::docop::DocumentSchema;
use crate::id::WaveletId;
use crate::id::util::is_user_data_wavelet;
use crate::supplement::wavelet_based::*;

/// Schema of the folder document (`UDW_FOLDER`). Any `i` attribute must be
/// an integer.
pub static UDW_FOLDER: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &[FOLDER_TAG]);
    s.add_attrs(FOLDER_TAG, &[ID_ATTR]);
    s.set_value_check(|_, attr, value| {
        (attr == ID_ATTR).then(|| is_valid_integer(value, i32::MIN))
    });
    s
});

fn version_is_non_negative(_tag: &str, attr: &str, value: &str) -> Option<bool> {
    (attr == VERSION_ATTR).then(|| is_non_negative_integer(value))
}

/// Schema of the archiving document (`UDW_ARCHIVE`). Any `v` attribute must
/// be a non-negative integer.
pub static UDW_ARCHIVE: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &[ARCHIVE_TAG]);
    s.add_attrs(ARCHIVE_TAG, &[ID_ATTR, VERSION_ATTR]);
    s.set_value_check(version_is_non_negative);
    s
});

/// Schema of the muted document (`UDW_MUTE`).
pub static UDW_MUTE: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &[MUTED_TAG]);
    s.add_attr_with_values(MUTED_TAG, MUTED_ATTR, &BOOLEAN_VALUES);
    s
});

/// Schema of the cleared document (`UDW_CLEARED`).
pub static UDW_CLEARED: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &[CLEARED_TAG]);
    s.add_attr_with_values(CLEARED_TAG, CLEARED_ATTR, &BOOLEAN_VALUES);
    s
});

/// Schema of the presentation document (`UDW_PRESENTATION`).
pub static UDW_PRESENTATION: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &[CONVERSATION_TAG]);
    s.add_children(Some(CONVERSATION_TAG), &[THREAD_TAG]);
    s.add_attrs(CONVERSATION_TAG, &[ID_ATTR]);
    s.add_attrs(THREAD_TAG, &[ID_ATTR]);
    s.add_attrs(THREAD_TAG, &[STATE_ATTR]);
    s
});

/// Schema of the read-state document (`UDW_READ`). Any `v` attribute must
/// be a non-negative integer.
pub static UDW_READ: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &[WAVELET_TAG]);
    s.add_children(Some(WAVELET_TAG), &[BLIP_READ_TAG]);
    s.add_children(Some(WAVELET_TAG), &[PARTICIPANTS_READ_TAG]);
    s.add_children(Some(WAVELET_TAG), &[WAVELET_READ_TAG]);
    s.add_children(Some(WAVELET_TAG), &[TAGS_READ_TAG]);
    s.add_attrs(WAVELET_TAG, &[ID_ATTR]);
    s.add_attrs(BLIP_READ_TAG, &[ID_ATTR]);
    s.add_attrs(BLIP_READ_TAG, &[VERSION_ATTR]);
    s.add_attrs(PARTICIPANTS_READ_TAG, &[VERSION_ATTR]);
    s.add_attrs(WAVELET_READ_TAG, &[VERSION_ATTR]);
    s.add_attrs(TAGS_READ_TAG, &[VERSION_ATTR]);
    s.set_value_check(version_is_non_negative);
    s
});

/// Schema of the seen document (`UDW_SEEN`).
pub static UDW_SEEN: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &[SEEN_VERSION_TAG]);
    s.add_attrs(SEEN_VERSION_TAG, &[ID_ATTR, SIGNATURE_ATTR]);
    s.add_children(None, &[NOTIFIED_VERSION_TAG]);
    s.add_attrs(NOTIFIED_VERSION_TAG, &[ID_ATTR, VERSION_ATTR]);
    s.add_children(None, &[NOTIFICATION_TAG]);
    s.add_attrs(NOTIFICATION_TAG, &[PENDING_NOTIFICATION_ATTR]);
    s
});

/// Schema of the gadgets document (`UDW_GADGET`).
pub static UDW_GADGET: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &[GADGET_TAG]);
    s.add_attrs(GADGET_TAG, &[ID_ATTR, PERMISSIONS_ATTR]);
    s.add_children(Some(GADGET_TAG), &[STATE_TAG]);
    s.add_attrs(STATE_TAG, &[NAME_ATTR, VALUE_ATTR]);
    s
});

/// Schema of the abuse document (`UDW_ABUSE`). Any `certainty` attribute
/// must be a floating-point number and any `timestamp` an integer.
pub static UDW_ABUSE: LazyLock<XmlSchemaConstraints> = LazyLock::new(|| {
    let mut s = XmlSchemaConstraints::new();
    s.add_children(None, &[WANTED_EVAL_TAG]);
    s.add_attrs(WANTED_EVAL_TAG, &[WAVELET_ID_ATTR]);
    s.add_attrs(WANTED_EVAL_TAG, &[CERTAINTY_ATTR]);
    s.add_attrs(WANTED_EVAL_TAG, &[TIMESTAMP_ATTR]);
    s.add_attrs(WANTED_EVAL_TAG, &[AGENT_ATTR]);
    s.add_attrs(WANTED_EVAL_TAG, &[COMMENT_ATTR]);
    s.add_attrs(WANTED_EVAL_TAG, &[IGNORED_ATTR]);
    s.add_attrs(WANTED_EVAL_TAG, &[ADDER_ATTR]);
    s.add_attr_with_values(WANTED_EVAL_TAG, WANTED_ATTR, &BOOLEAN_VALUES);
    s.set_value_check(|_, attr, value| match attr {
        CERTAINTY_ATTR => Some(is_double(value)),
        TIMESTAMP_ATTR => Some(is_long(value)),
        WAVELET_ID_ATTR => Some(is_wavelet_id(value)),
        _ => None,
    });
    s
});

/// The schemas of the supplement documents of user-data wavelets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UserDataSchemas;

impl SchemaProvider for UserDataSchemas {
    fn schema_for_id(
        &self,
        wavelet_id: &WaveletId,
        document_id: &str,
    ) -> Option<&dyn DocumentSchema> {
        if !is_user_data_wavelet(wavelet_id) {
            return None;
        }
        let schema: &XmlSchemaConstraints = match document_id {
            READSTATE_DOCUMENT => &UDW_READ,
            ARCHIVING_DOCUMENT => &UDW_ARCHIVE,
            FOLDERS_DOCUMENT => &UDW_FOLDER,
            MUTED_DOCUMENT => &UDW_MUTE,
            PRESENTATION_DOCUMENT => &UDW_PRESENTATION,
            CLEARED_DOCUMENT => &UDW_CLEARED,
            SEEN_DOCUMENT => &UDW_SEEN,
            GADGETS_DOCUMENT => &UDW_GADGET,
            ABUSE_DOCUMENT => &UDW_ABUSE,
            _ => return None,
        };
        Some(schema)
    }
}
