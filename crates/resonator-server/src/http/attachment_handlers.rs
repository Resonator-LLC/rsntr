//! The attachment endpoints.
//!
//! Follow `AttachmentServlet` and `AttachmentInfoServlet` of Apache Wave
//! (Apache License 2.0): `GET /attachment/<id>` and `GET /thumbnail/<id>`
//! serve an attachment and its thumbnail to whoever may read the wavelet
//! it belongs to, `POST /attachment/<id>` takes a multipart form with the
//! fields `attachmentId` and `waveRef` and one file, and
//! `GET /attachmentsInfo?attachmentIds=a,b` describes attachments.
//!
//! What is served comes from users, so it must not be able to act as a
//! page of this server: every response says `Content-Disposition:
//! attachment`, `X-Content-Type-Options: nosniff` and
//! `Content-Security-Policy: sandbox`, and only content types a browser
//! merely displays are passed on; everything else, HTML, SVG and scripts
//! included, is `application/octet-stream`.

use std::collections::HashMap;
use std::io::SeekFrom;

use axum::body::Body;
use axum::extract::multipart::{Multipart, MultipartError, MultipartRejection};
use axum::extract::{Path, Query, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use resonator_model::proto::attachment::AttachmentsResponse;
use resonator_model::proto::json::to_json;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use super::{AppState, Context, failure, header_str, internal_error};
use crate::attachment::mime::served_content_type;
use crate::attachment::thumbnail::{PATTERN_MIME_TYPE, THUMBNAIL_MIME_TYPE, pattern_for};
use crate::attachment::{
    AttachmentError, AttachmentId, AttachmentMetadata, FetchError, Upload, wave_ref_to_wavelet_name,
};
use crate::auth::Identity;

/// Most ids one `/attachmentsInfo` request may name.
const MAX_INFO_IDS: usize = 500;

/// Longest value of a form field of an upload, in bytes.
const MAX_FIELD_LENGTH: usize = 2048;

/// The policy for anything served from the attachment store: if a browser
/// does show it as a document, it is in a sandbox without scripts and
/// without this server's origin.
const SANDBOX_POLICY: &str = "sandbox; default-src 'none'; style-src 'unsafe-inline'";

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

fn forbidden() -> Response {
    StatusCode::FORBIDDEN.into_response()
}

/// The metadata of the attachment a request path names, if the caller may
/// read the wavelet it belongs to: 404 if there is no such attachment, 403
/// if the caller may not read it (`AttachmentServlet.doGet`).
///
/// With federation, an attachment uploaded to another server (the domain
/// of its id) that is not cached here is fetched from that server first,
/// for the wavelet `wave_ref` names (the query parameter `waveRef`), which
/// the caller must be able to read: 503 if that server cannot be reached.
async fn authorized_metadata(
    state: &AppState,
    identity: Option<&Identity>,
    id: &str,
    wave_ref: Option<&str>,
) -> Result<AttachmentMetadata, Response> {
    let Ok(id) = AttachmentId::deserialise(id) else {
        return Err(not_found());
    };
    // Nobody who is not signed in learns whether an id exists.
    let Some(identity) = identity.filter(|identity| identity.can_read()) else {
        return Err(forbidden());
    };
    let metadata = match state.attachments.metadata(&id).await {
        Ok(metadata) => metadata,
        Err(error) => return Err(internal_error(error)),
    };
    let remote = state.attachments.is_fetchable(&id);
    // The wavelet to check: the one the attachment was stored for, or for
    // one not here yet, the one the caller names.
    let wavelet_name = match &metadata {
        Some(metadata) => metadata.wavelet_name(),
        None if remote => wave_ref.and_then(wave_ref_to_wavelet_name),
        None => return Err(not_found()),
    };
    let Some(wavelet_name) = wavelet_name else {
        return Err(if metadata.is_some() {
            forbidden()
        } else {
            not_found()
        });
    };
    match state
        .frontend
        .check_access(&identity.participant, &wavelet_name)
        .await
    {
        Ok(true) => {}
        Ok(false) => return Err(forbidden()),
        Err(error) => return Err(internal_error(error)),
    }
    match metadata {
        Some(metadata) if !metadata.evicted => {
            if metadata.origin.is_some()
                && let Err(error) = state.attachments.touch(&metadata.id).await
            {
                tracing::debug!(%error, "cannot note the use of a cached attachment");
            }
            Ok(metadata)
        }
        _ if !remote => Err(not_found()),
        _ => match state.attachments.fetch_remote(&id, &wavelet_name).await {
            Ok(metadata) => Ok(metadata),
            Err(FetchError::NotFound) => Err(not_found()),
            Err(FetchError::Unavailable { domain, message }) => {
                tracing::debug!(attachment = %id, %message, "attachment unavailable");
                Err(failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("unavailable from {domain}"),
                ))
            }
            Err(error @ FetchError::Rejected(_)) => {
                tracing::warn!(attachment = %id, %error, "fetched attachment rejected");
                Err(failure(StatusCode::BAD_GATEWAY, error.to_string()))
            }
            Err(error @ FetchError::Local(_)) => Err(internal_error(error)),
        },
    }
}

/// The `waveRef` query parameter of an attachment request.
#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct WaveRefQuery {
    #[serde(rename = "waveRef")]
    wave_ref: Option<String>,
}

/// `Content-Disposition: attachment` with the file name in both forms: a
/// quoted ASCII fallback and the UTF-8 form of RFC 5987.
fn content_disposition(file_name: &str) -> HeaderValue {
    let name = if file_name.is_empty() {
        "attachment"
    } else {
        file_name
    };
    let fallback: String = name
        .chars()
        .map(|c| match c {
            '"' | '\\' | '/' | ';' | '%' => '_',
            c if c.is_ascii_graphic() || c == ' ' => c,
            _ => '_',
        })
        .collect();
    let mut encoded = String::new();
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

/// Whether an `If-None-Match` or `If-Range` value names the entity tag.
fn matches_etag(value: &str, etag: &str) -> bool {
    value.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

/// The byte range a `Range` header asks for.
#[derive(Debug, PartialEq, Eq)]
enum ByteRange {
    /// No usable range: serve everything.
    Whole,
    /// First and last byte, inclusive.
    Part(u64, u64),
    /// The range lies outside the content.
    Unsatisfiable,
}

/// Parses `Range: bytes=first-last`, `bytes=first-` or `bytes=-suffix`
/// against content of `length` bytes. Several ranges at once are not
/// supported and are answered with the whole content, which the
/// specification allows.
fn parse_range(value: &str, length: u64) -> ByteRange {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return ByteRange::Whole;
    };
    if spec.contains(',') {
        return ByteRange::Whole;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return ByteRange::Whole;
    };
    let number = |text: &str| text.trim().parse::<u64>().ok();
    match (first.trim().is_empty(), last.trim().is_empty()) {
        // The last `suffix` bytes.
        (true, false) => match number(last) {
            Some(0) => ByteRange::Unsatisfiable,
            Some(suffix) if length > 0 => {
                ByteRange::Part(length.saturating_sub(suffix), length - 1)
            }
            Some(_) => ByteRange::Unsatisfiable,
            None => ByteRange::Whole,
        },
        (false, _) => {
            let Some(first) = number(first) else {
                return ByteRange::Whole;
            };
            let last = if last.trim().is_empty() {
                Some(u64::MAX)
            } else {
                number(last)
            };
            match last {
                None => ByteRange::Whole,
                Some(last) if last < first => ByteRange::Whole,
                Some(_) if first >= length => ByteRange::Unsatisfiable,
                Some(last) => ByteRange::Part(first, last.min(length - 1)),
            }
        }
        (true, true) => ByteRange::Whole,
    }
}

/// What a file response says about its content.
struct Served<'a> {
    content_type: &'a str,
    file_name: &'a str,
    etag: String,
}

fn common_headers(served: &Served<'_>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let mut set = |name: header::HeaderName, value: &str| {
        if let Ok(value) = HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    };
    set(header::CONTENT_TYPE, served.content_type);
    set(header::ETAG, &served.etag);
    // Private, and checked with the server every time: who may read an
    // attachment can change.
    set(header::CACHE_CONTROL, "private, no-cache");
    set(header::ACCEPT_RANGES, "bytes");
    set(header::CONTENT_SECURITY_POLICY, SANDBOX_POLICY);
    set(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    headers.insert(
        header::CONTENT_DISPOSITION,
        content_disposition(served.file_name),
    );
    headers
}

/// Answers a conditional request without a body if the client's copy is
/// current.
fn not_modified(request: &HeaderMap, served: &Served<'_>) -> Option<Response> {
    let current = header_str(request, header::IF_NONE_MATCH)
        .is_some_and(|value| matches_etag(value, &served.etag));
    current.then(|| (StatusCode::NOT_MODIFIED, common_headers(served)).into_response())
}

/// Streams a file, or the part of it the request's `Range` asks for.
async fn serve_file(
    request: &HeaderMap,
    method: &Method,
    served: Served<'_>,
    mut file: tokio::fs::File,
    length: u64,
) -> Response {
    if let Some(response) = not_modified(request, &served) {
        return response;
    }
    let mut headers = common_headers(&served);
    // A range counts only if the client's copy is of this content.
    let range_applies = header_str(request, header::IF_RANGE)
        .is_none_or(|value| matches_etag(value, &served.etag) && value.trim() != "*");
    let range = match header_str(request, header::RANGE) {
        Some(value) if range_applies => parse_range(value, length),
        _ => ByteRange::Whole,
    };
    let (status, first, count) = match range {
        ByteRange::Whole => (StatusCode::OK, 0, length),
        ByteRange::Part(first, last) => {
            if let Ok(value) = HeaderValue::from_str(&format!("bytes {first}-{last}/{length}")) {
                headers.insert(header::CONTENT_RANGE, value);
            }
            (StatusCode::PARTIAL_CONTENT, first, last - first + 1)
        }
        ByteRange::Unsatisfiable => {
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{length}")) {
                headers.insert(header::CONTENT_RANGE, value);
            }
            return (StatusCode::RANGE_NOT_SATISFIABLE, headers).into_response();
        }
    };
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(count));
    if method == Method::HEAD {
        return (status, headers).into_response();
    }
    if first > 0
        && let Err(error) = file.seek(SeekFrom::Start(first)).await
    {
        return internal_error(error);
    }
    let body = Body::from_stream(ReaderStream::with_capacity(file.take(count), 64 * 1024));
    (status, headers, body).into_response()
}

/// `GET /attachment/<id>`: the bytes of an attachment. Supports `Range`
/// and conditional requests.
pub(super) async fn download(
    State(state): State<AppState>,
    context: Context,
    method: Method,
    request: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<WaveRefQuery>,
) -> Response {
    let metadata = match authorized_metadata(
        &state,
        context.identity.as_ref(),
        &id,
        query.wave_ref.as_deref(),
    )
    .await
    {
        Ok(metadata) => metadata,
        Err(response) => return response,
    };
    let (file, length) = match state.attachments.open_file(&metadata.id).await {
        Ok(Some(opened)) => opened,
        Ok(None) => return not_found(),
        Err(error) => return internal_error(error),
    };
    let served = Served {
        content_type: served_content_type(&metadata.mime_type),
        file_name: &metadata.file_name,
        etag: metadata.etag(),
    };
    tracing::debug!(attachment = %metadata.id, "serving attachment");
    serve_file(&request, &method, served, file, length).await
}

/// `GET /thumbnail/<id>`: the stored thumbnail of an image attachment, or
/// a stand-in picture for the attachment's kind of content.
pub(super) async fn thumbnail(
    State(state): State<AppState>,
    context: Context,
    method: Method,
    request: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<WaveRefQuery>,
) -> Response {
    let metadata = match authorized_metadata(
        &state,
        context.identity.as_ref(),
        &id,
        query.wave_ref.as_deref(),
    )
    .await
    {
        Ok(metadata) => metadata,
        Err(response) => return response,
    };
    let etag = metadata.etag();
    if metadata.image.is_some() {
        let (file, length) = match state.attachments.open_thumbnail(&metadata.id).await {
            Ok(Some(opened)) => opened,
            Ok(None) => return not_found(),
            Err(error) => return internal_error(error),
        };
        let served = Served {
            content_type: THUMBNAIL_MIME_TYPE,
            file_name: &metadata.file_name,
            etag: format!("\"{}-t\"", etag.trim_matches('"')),
        };
        return serve_file(&request, &method, served, file, length).await;
    }
    let served = Served {
        content_type: PATTERN_MIME_TYPE,
        file_name: &metadata.file_name,
        etag: format!("\"{}-p\"", etag.trim_matches('"')),
    };
    if let Some(response) = not_modified(&request, &served) {
        return response;
    }
    let mut headers = common_headers(&served);
    headers.remove(header::ACCEPT_RANGES);
    (headers, pattern_for(&metadata.mime_type)).into_response()
}

/// `GET /attachmentsInfo?attachmentIds=a,b`: the metadata of the named
/// attachments that exist and that the caller may read, as an
/// `AttachmentsResponse` in the protocol's JSON form
/// (`AttachmentInfoServlet`). 404 without the parameter; ids that are not
/// valid, not known or not the caller's to read are left out.
pub(super) async fn info(
    State(state): State<AppState>,
    context: Context,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(ids) = params.get("attachmentIds") else {
        return not_found();
    };
    let mut response = AttachmentsResponse::default();
    let identity = context.identity.as_ref().filter(|i| i.can_read());
    // With federation, attachments of other servers named here are
    // fetched for the wavelet `waveRef` names.
    let wave_ref = params.get("waveRef");
    let remote_wavelet = wave_ref
        .filter(|_| state.attachments.fetches_remote())
        .and_then(|wave_ref| wave_ref_to_wavelet_name(wave_ref));
    if let Some(identity) = identity {
        let mut seen = std::collections::HashSet::new();
        for id in ids.split(',').take(MAX_INFO_IDS) {
            let Ok(id) = AttachmentId::deserialise(id) else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            let metadata = match state.attachments.metadata(&id).await {
                Ok(Some(metadata)) => metadata,
                Ok(None) if remote_wavelet.is_some() && state.attachments.is_fetchable(&id) => {
                    // Fetched from its server, through the access check.
                    if let Ok(metadata) = authorized_metadata(
                        &state,
                        Some(identity),
                        &id.serialise(),
                        wave_ref.map(String::as_str),
                    )
                    .await
                    {
                        response.attachment.push(metadata.to_proto());
                    }
                    continue;
                }
                Ok(None) => continue,
                Err(error) => return internal_error(error),
            };
            let Some(wavelet_name) = metadata.wavelet_name() else {
                continue;
            };
            match state
                .frontend
                .check_access(&identity.participant, &wavelet_name)
                .await
            {
                Ok(true) => response.attachment.push(metadata.to_proto()),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, wavelet = %wavelet_name, "cannot check access for attachment info");
                }
            }
        }
    }
    match to_json(&response) {
        Ok(text) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            text,
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

/// Room for the parts of an upload form that are not the file.
const FORM_OVERHEAD: u64 = 64 * 1024;

fn upload_error(error: AttachmentError) -> Response {
    match error {
        AttachmentError::TooLarge { max } => failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("The file is larger than the {max} bytes allowed."),
        ),
        AttachmentError::QuotaExceeded { quota } => failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("Your attachment quota of {quota} bytes is used up."),
        ),
        AttachmentError::AlreadyExists(id) => failure(
            StatusCode::CONFLICT,
            format!("An attachment with id {id} already exists."),
        ),
        other => internal_error(other),
    }
}

fn multipart_error(error: MultipartError) -> Response {
    let status = error.status();
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        failure(status, "The upload is too large.")
    } else {
        failure(StatusCode::BAD_REQUEST, "The upload is not a valid form.")
    }
}

/// The text of a small form field.
async fn field_text(
    mut field: axum::extract::multipart::Field<'_>,
) -> Result<Option<String>, MultipartError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await? {
        if bytes.len() + chunk.len() > MAX_FIELD_LENGTH {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8(bytes).ok())
}

/// `POST /attachment/<anything>`: stores the file of a multipart form as
/// the attachment named by the form field `attachmentId`, belonging to the
/// wavelet named by the form field `waveRef`, which the caller must be
/// able to read (`AttachmentServlet.doPost`). Answers 201 with `OK`.
///
/// The file is written to disk as it arrives. 400 if a field or the file
/// is missing or invalid, 403 if the caller may not use the wavelet, 409 if
/// the id is taken, 413 if the file is too large, 415 if the request is
/// not a multipart form.
pub(super) async fn upload(
    State(state): State<AppState>,
    context: Context,
    headers: HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
) -> Response {
    // Before reading anything: uploads are for signed-in writers.
    let Some(identity) = context.identity.as_ref().filter(|i| i.can_write()) else {
        return forbidden();
    };
    // And for those with quota left. Where the request declares its
    // length, an upload that cannot fit is refused before its body is
    // read; the form's other parts are allowed for. An upload of unknown
    // length is stopped while it streams, see `begin_upload_for`.
    match state.attachments.quota_left(&identity.participant).await {
        Ok(None) => {}
        Ok(Some((quota, left))) => {
            let declared = headers
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            if left == 0
                || declared.is_some_and(|length| length > left.saturating_add(FORM_OVERHEAD))
            {
                return upload_error(AttachmentError::QuotaExceeded { quota });
            }
        }
        Err(error) => return upload_error(error),
    }
    let Ok(mut multipart) = multipart else {
        return failure(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Request contents type is not supported by the servlet.",
        );
    };
    let bad_request = |message: &str| failure(StatusCode::BAD_REQUEST, message);
    let mut attachment_id = None;
    let mut wave_ref = None;
    let mut file: Option<(Upload, String)> = None;
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(error) => return multipart_error(error),
        };
        let name = field.name().unwrap_or("").to_owned();
        if let Some(file_name) = field.file_name().map(str::to_owned) {
            if file.is_some() {
                return bad_request("Only one file per request.");
            }
            let mut upload = match state
                .attachments
                .begin_upload_for(&identity.participant)
                .await
            {
                Ok(upload) => upload,
                Err(error) => return upload_error(error),
            };
            loop {
                match field.chunk().await {
                    Ok(Some(chunk)) => {
                        if let Err(error) = upload.write(&chunk).await {
                            return upload_error(error);
                        }
                    }
                    Ok(None) => break,
                    Err(error) => return multipart_error(error),
                }
            }
            file = Some((upload, file_name));
            continue;
        }
        let text = match field_text(field).await {
            Ok(text) => text,
            Err(error) => return multipart_error(error),
        };
        match name.as_str() {
            "attachmentId" => attachment_id = text,
            "waveRef" => wave_ref = text,
            _ => {}
        }
    }
    let Some(attachment_id) = attachment_id else {
        return bad_request("No attachment Id in the request.");
    };
    let Ok(attachment_id) = AttachmentId::deserialise(&attachment_id) else {
        return bad_request("Invalid attachment Id in the request.");
    };
    // With federation, the domain of an id says which server holds the
    // attachment; nobody may upload in another server's name.
    if state.attachments.fetches_remote() && state.attachments.is_remote(&attachment_id) {
        return bad_request("The attachment id names another server.");
    }
    let Some(wave_ref) = wave_ref else {
        return bad_request("No wave reference in request.");
    };
    let Some(wavelet_name) = wave_ref_to_wavelet_name(&wave_ref) else {
        return bad_request("Invalid wave reference in request.");
    };
    match state
        .frontend
        .check_access(&identity.participant, &wavelet_name)
        .await
    {
        Ok(true) => {}
        Ok(false) => return forbidden(),
        Err(error) => return internal_error(error),
    }
    let Some((upload, file_name)) = file else {
        return bad_request("No file in the request.");
    };
    match state
        .attachments
        .finish_upload(
            upload,
            &attachment_id,
            &wavelet_name,
            &file_name,
            Some(&identity.participant),
        )
        .await
    {
        Ok(metadata) => {
            tracing::debug!(
                attachment = %metadata.id,
                file_name = %metadata.file_name,
                participant = %identity.participant,
                "attachment created"
            );
            (StatusCode::CREATED, "OK").into_response()
        }
        Err(error) => upload_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        use ByteRange::{Part, Unsatisfiable, Whole};
        assert_eq!(parse_range("bytes=0-9", 100), Part(0, 9));
        assert_eq!(parse_range("bytes=10-", 100), Part(10, 99));
        assert_eq!(parse_range("bytes=-10", 100), Part(90, 99));
        assert_eq!(parse_range("bytes=-1000", 100), Part(0, 99));
        assert_eq!(parse_range("bytes=50-1000", 100), Part(50, 99));
        assert_eq!(parse_range("bytes=99-99", 100), Part(99, 99));
        assert_eq!(parse_range("bytes=100-", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=0-", 0), Unsatisfiable);
        assert_eq!(parse_range("bytes=-5", 0), Unsatisfiable);
        for ignored in [
            "bytes=0-1,5-6",
            "bytes=9-0",
            "items=0-9",
            "bytes=a-b",
            "bytes=-",
            "bytes=5",
            "",
            "bytes=99999999999999999999-",
        ] {
            assert_eq!(parse_range(ignored, 100), Whole, "{ignored}");
        }
    }

    #[test]
    fn file_names_are_encoded_safely() {
        let value = |name: &str| content_disposition(name).to_str().unwrap().to_owned();
        assert_eq!(
            value("photo.png"),
            "attachment; filename=\"photo.png\"; filename*=UTF-8''photo.png"
        );
        assert_eq!(
            value("a \"b\"; x=\\y.txt"),
            "attachment; filename=\"a _b__ x=_y.txt\"; \
             filename*=UTF-8''a%20%22b%22%3B%20x%3D%5Cy.txt"
        );
        assert_eq!(
            value("caf\u{e9}.txt"),
            "attachment; filename=\"caf_.txt\"; filename*=UTF-8''caf%C3%A9.txt"
        );
        assert_eq!(
            value(""),
            "attachment; filename=\"attachment\"; filename*=UTF-8''attachment"
        );
        // Nothing can break out of the header.
        assert!(!value("a\r\nSet-Cookie: x=y").contains('\n'));
    }

    #[test]
    fn entity_tags_are_compared() {
        assert!(matches_etag("\"abc\"", "\"abc\""));
        assert!(matches_etag("W/\"abc\"", "\"abc\""));
        assert!(matches_etag("\"x\", \"abc\"", "\"abc\""));
        assert!(matches_etag("*", "\"abc\""));
        assert!(!matches_etag("\"abd\"", "\"abc\""));
        assert!(!matches_etag("", "\"abc\""));
    }
}
