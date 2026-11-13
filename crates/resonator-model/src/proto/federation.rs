//! Messages of `org/waveprotocol/wave/federation/federation.protodevel`
//! (package `federation`). Fields documented as required are `required` in
//! the schema.

use super::wave_message;

/// Operations by one author for a wavelet at a given version.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolWaveletDelta {
    /// Required. The version the operations apply at.
    #[prost(message, optional, tag = "1")]
    pub hashed_version: Option<ProtocolHashedVersion>,
    /// Required.
    #[prost(string, optional, tag = "2")]
    pub author: Option<String>,
    #[prost(message, repeated, tag = "3")]
    pub operation: Vec<ProtocolWaveletOperation>,
    /// Addresses on whose behalf the author acts; unused by the server.
    #[prost(string, repeated, tag = "4")]
    pub address_path: Vec<String>,
}
wave_message!(ProtocolWaveletDelta {
    required: [hashed_version, author],
    nested: [hashed_version],
    repeated: [operation],
});

/// A wavelet version and the history hash at that version.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolHashedVersion {
    /// Required.
    #[prost(int64, optional, tag = "1")]
    pub version: Option<i64>,
    /// Required.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub history_hash: Option<Vec<u8>>,
}
wave_message!(ProtocolHashedVersion {
    required: [version, history_hash],
});

/// An operation within a delta. Exactly one field should be set.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolWaveletOperation {
    #[prost(string, optional, tag = "1")]
    pub add_participant: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub remove_participant: Option<String>,
    #[prost(message, optional, tag = "3")]
    pub mutate_document: Option<protocol_wavelet_operation::MutateDocument>,
    #[prost(bool, optional, tag = "4")]
    pub no_op: Option<bool>,
}
wave_message!(ProtocolWaveletOperation {
    nested: [mutate_document],
});

pub mod protocol_wavelet_operation {
    use super::super::wave_message;

    #[derive(Clone, PartialEq, Eq, prost::Message)]
    pub struct MutateDocument {
        /// Required.
        #[prost(string, optional, tag = "1")]
        pub document_id: Option<String>,
        /// Required.
        #[prost(message, optional, tag = "2")]
        pub document_operation: Option<super::ProtocolDocumentOperation>,
    }
    wave_message!(MutateDocument {
        required: [document_id, document_operation],
        nested: [document_operation],
    });
}

/// A document operation: a list of components.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolDocumentOperation {
    #[prost(message, repeated, tag = "1")]
    pub component: Vec<protocol_document_operation::Component>,
}
wave_message!(ProtocolDocumentOperation {
    repeated: [component],
});

pub mod protocol_document_operation {
    use super::super::wave_message;

    /// A component of a document operation. Exactly one field should be
    /// set.
    #[derive(Clone, PartialEq, Eq, prost::Message)]
    pub struct Component {
        #[prost(message, optional, tag = "1")]
        pub annotation_boundary: Option<component::AnnotationBoundary>,
        #[prost(string, optional, tag = "2")]
        pub characters: Option<String>,
        #[prost(message, optional, tag = "3")]
        pub element_start: Option<component::ElementStart>,
        #[prost(bool, optional, tag = "4")]
        pub element_end: Option<bool>,
        #[prost(int32, optional, tag = "5")]
        pub retain_item_count: Option<i32>,
        #[prost(string, optional, tag = "6")]
        pub delete_characters: Option<String>,
        #[prost(message, optional, tag = "7")]
        pub delete_element_start: Option<component::ElementStart>,
        #[prost(bool, optional, tag = "8")]
        pub delete_element_end: Option<bool>,
        #[prost(message, optional, tag = "9")]
        pub replace_attributes: Option<component::ReplaceAttributes>,
        #[prost(message, optional, tag = "10")]
        pub update_attributes: Option<component::UpdateAttributes>,
    }
    wave_message!(Component {
        nested: [
            annotation_boundary,
            element_start,
            delete_element_start,
            replace_attributes,
            update_attributes
        ],
    });

    pub mod component {
        use super::super::super::wave_message;

        #[derive(Clone, PartialEq, Eq, prost::Message)]
        pub struct KeyValuePair {
            /// Required.
            #[prost(string, optional, tag = "1")]
            pub key: Option<String>,
            /// Required.
            #[prost(string, optional, tag = "2")]
            pub value: Option<String>,
        }
        wave_message!(KeyValuePair {
            required: [key, value],
        });

        #[derive(Clone, PartialEq, Eq, prost::Message)]
        pub struct KeyValueUpdate {
            /// Required.
            #[prost(string, optional, tag = "1")]
            pub key: Option<String>,
            /// Absent if the key had no value.
            #[prost(string, optional, tag = "2")]
            pub old_value: Option<String>,
            /// Absent if the key gets no value.
            #[prost(string, optional, tag = "3")]
            pub new_value: Option<String>,
        }
        wave_message!(KeyValueUpdate { required: [key] });

        #[derive(Clone, PartialEq, Eq, prost::Message)]
        pub struct ElementStart {
            /// Required. The tag name.
            #[prost(string, optional, tag = "1")]
            pub r#type: Option<String>,
            #[prost(message, repeated, tag = "2")]
            pub attribute: Vec<KeyValuePair>,
        }
        wave_message!(ElementStart {
            required: [r#type],
            repeated: [attribute],
        });

        #[derive(Clone, PartialEq, Eq, prost::Message)]
        pub struct ReplaceAttributes {
            /// Set when both attribute lists are empty.
            #[prost(bool, optional, tag = "1")]
            pub empty: Option<bool>,
            #[prost(message, repeated, tag = "2")]
            pub old_attribute: Vec<KeyValuePair>,
            #[prost(message, repeated, tag = "3")]
            pub new_attribute: Vec<KeyValuePair>,
        }
        wave_message!(ReplaceAttributes {
            repeated: [old_attribute, new_attribute],
        });

        #[derive(Clone, PartialEq, Eq, prost::Message)]
        pub struct UpdateAttributes {
            /// Set when there are no updates.
            #[prost(bool, optional, tag = "1")]
            pub empty: Option<bool>,
            #[prost(message, repeated, tag = "2")]
            pub attribute_update: Vec<KeyValueUpdate>,
        }
        wave_message!(UpdateAttributes {
            repeated: [attribute_update],
        });

        #[derive(Clone, PartialEq, Eq, prost::Message)]
        pub struct AnnotationBoundary {
            /// Set when there are no ends and no changes.
            #[prost(bool, optional, tag = "1")]
            pub empty: Option<bool>,
            #[prost(string, repeated, tag = "2")]
            pub end: Vec<String>,
            #[prost(message, repeated, tag = "3")]
            pub change: Vec<KeyValueUpdate>,
        }
        wave_message!(AnnotationBoundary { repeated: [change] });
    }
}

/// A delta as applied by the host server. History hashes are computed over
/// the serialised form of this message.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolAppliedWaveletDelta {
    /// Required.
    #[prost(message, optional, tag = "1")]
    pub signed_original_delta: Option<ProtocolSignedDelta>,
    /// The version the delta was applied at, if it was transformed.
    #[prost(message, optional, tag = "2")]
    pub hashed_version_applied_at: Option<ProtocolHashedVersion>,
    /// Required.
    #[prost(int32, optional, tag = "3")]
    pub operations_applied: Option<i32>,
    /// Required. Milliseconds since the epoch.
    #[prost(int64, optional, tag = "4")]
    pub application_timestamp: Option<i64>,
    /// Extension: the host epoch the delta was applied in (see
    /// `federation_rpc::EpochStart`). Absent for epoch 0, the wavelet
    /// domain's own server, so such deltas keep their original bytes.
    #[prost(uint64, optional, tag = "16")]
    pub epoch: Option<u64>,
    /// Extension: the operations actually applied, for an orphaned delta
    /// resubmitted after a rewind and rebased by the host onto a history
    /// its original target is not part of. Replicas take these instead of
    /// transforming the original. Absent otherwise.
    #[prost(message, optional, tag = "17")]
    pub rebased_operations: Option<ProtocolWaveletDelta>,
}
wave_message!(ProtocolAppliedWaveletDelta {
    required: [
        signed_original_delta,
        operations_applied,
        application_timestamp
    ],
    nested: [
        signed_original_delta,
        hashed_version_applied_at,
        rebased_operations
    ],
});

/// A serialised `ProtocolWaveletDelta` with signatures over its bytes.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolSignedDelta {
    /// Required. The serialised `ProtocolWaveletDelta`.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub delta: Option<Vec<u8>>,
    #[prost(message, repeated, tag = "2")]
    pub signature: Vec<ProtocolSignature>,
}
wave_message!(ProtocolSignedDelta {
    required: [delta],
    repeated: [signature],
});

/// A signature of a delta.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolSignature {
    /// Required.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub signature_bytes: Option<Vec<u8>>,
    /// Required.
    #[prost(bytes = "vec", optional, tag = "2")]
    pub signer_id: Option<Vec<u8>>,
    /// Required.
    #[prost(
        enumeration = "protocol_signature::SignatureAlgorithm",
        optional,
        tag = "3"
    )]
    pub signature_algorithm: Option<i32>,
}
wave_message!(ProtocolSignature {
    required: [signature_bytes, signer_id, signature_algorithm],
});

pub mod protocol_signature {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
    #[repr(i32)]
    pub enum SignatureAlgorithm {
        Sha1Rsa = 1,
        /// A resonator extension: an Ed25519 signature (64 bytes) by a
        /// server key, whose signer id is the SHA-256 of the 32-byte public
        /// key (see `ProtocolSignerInfo::ed25519_public_key`).
        Ed25519 = 2,
    }
}

/// A certificate chain that signatures refer to by its hash.
///
/// Resonator's Ed25519 signers carry no certificates: they set
/// `ed25519_public_key` instead, with `hash_algorithm` SHA-256, and may
/// carry the key rotation statements that chain the key to earlier keys of
/// the same domain. Their signer id is the SHA-256 of the raw key.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolSignerInfo {
    /// Required.
    #[prost(
        enumeration = "protocol_signer_info::HashAlgorithm",
        optional,
        tag = "1"
    )]
    pub hash_algorithm: Option<i32>,
    /// Required.
    #[prost(string, optional, tag = "2")]
    pub domain: Option<String>,
    #[prost(bytes = "vec", repeated, tag = "3")]
    pub certificate: Vec<Vec<u8>>,
    /// Resonator extension: the signer's raw 32-byte Ed25519 public key,
    /// in place of the certificate chain.
    #[prost(bytes = "vec", optional, tag = "16")]
    pub ed25519_public_key: Option<Vec<u8>>,
    /// Resonator extension: key rotation statements for this domain.
    #[prost(message, repeated, tag = "17")]
    pub key_rotation: Vec<super::federation_rpc::SignedKeyRotation>,
    /// Resonator extension (person mode): the certificate by which the
    /// person key of `domain` lets this device key sign for it.
    #[prost(message, optional, tag = "18")]
    pub device_cert: Option<super::federation_rpc::SignedDeviceCert>,
}
wave_message!(ProtocolSignerInfo {
    required: [hash_algorithm, domain],
    nested: [device_cert],
    repeated: [key_rotation],
});

pub mod protocol_signer_info {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
    #[repr(i32)]
    pub enum HashAlgorithm {
        Sha256 = 1,
        Sha512 = 2,
    }
}
