//! Messages of `box/attachment/attachment.proto` (package `attachment`).

use super::wave_message;

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct AttachmentsResponse {
    #[prost(message, repeated, tag = "1")]
    pub attachment: Vec<AttachmentMetadata>,
}
wave_message!(AttachmentsResponse {
    repeated: [attachment],
});

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct AttachmentMetadata {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub attachment_id: Option<String>,
    /// Required. The wavelet the attachment belongs to, as a wave
    /// reference path.
    #[prost(string, optional, tag = "2")]
    pub wave_ref: Option<String>,
    /// Required.
    #[prost(string, optional, tag = "3")]
    pub file_name: Option<String>,
    /// Required.
    #[prost(string, optional, tag = "4")]
    pub mime_type: Option<String>,
    /// Required. Bytes.
    #[prost(int64, optional, tag = "5")]
    pub size: Option<i64>,
    /// Required. The address of the uploader, or empty.
    #[prost(string, optional, tag = "6")]
    pub creator: Option<String>,
    /// Required.
    #[prost(string, optional, tag = "7")]
    pub attachment_url: Option<String>,
    /// Required.
    #[prost(string, optional, tag = "8")]
    pub thumbnail_url: Option<String>,
    /// Set if the attachment is an image.
    #[prost(message, optional, tag = "9")]
    pub image_metadata: Option<ImageMetadata>,
    #[prost(message, optional, tag = "10")]
    pub thumbnail_metadata: Option<ImageMetadata>,
    #[prost(bool, optional, tag = "11")]
    pub malware: Option<bool>,
}
wave_message!(AttachmentMetadata {
    required: [
        attachment_id,
        wave_ref,
        file_name,
        mime_type,
        size,
        creator,
        attachment_url,
        thumbnail_url
    ],
    nested: [image_metadata, thumbnail_metadata],
});

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ImageMetadata {
    /// Required.
    #[prost(int32, optional, tag = "1")]
    pub width: Option<i32>,
    /// Required.
    #[prost(int32, optional, tag = "2")]
    pub height: Option<i32>,
}
wave_message!(ImageMetadata {
    required: [width, height],
});
