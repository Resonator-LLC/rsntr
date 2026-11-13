//! Content types of attachments.

/// The content type for a file name, by its extension;
/// `application/octet-stream` if the extension says nothing
/// (`AttachmentService.getMimeType`, which asks Java's file name map).
/// What the uploading client claims is not used.
pub fn mime_type_for(file_name: &str) -> &'static str {
    let extension = match file_name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => extension.to_ascii_lowercase(),
        _ => return OCTET_STREAM,
    };
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "heic" => "image/heic",
        "txt" | "text" | "log" => "text/plain",
        "csv" => "text/csv",
        "md" => "text/markdown",
        "htm" | "html" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "xml" => "application/xml",
        "xhtml" => "application/xhtml+xml",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "bz2" => "application/x-bzip2",
        "xz" => "application/x-xz",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "rtf" => "application/rtf",
        "epub" => "application/epub+zip",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "ogv" => "video/ogg",
        "mpeg" | "mpg" => "video/mpeg",
        _ => OCTET_STREAM,
    }
}

pub const OCTET_STREAM: &str = "application/octet-stream";

/// The content type an attachment of type `mime_type` is served with: the
/// type itself if a browser can only display or play it, otherwise
/// `application/octet-stream`. Anything a browser might run script from
/// (HTML, SVG, XML, scripts, and whatever is not listed here) is served as
/// plain bytes.
pub fn served_content_type(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/bmp" => "image/bmp",
        "image/x-icon" => "image/x-icon",
        "image/tiff" => "image/tiff",
        "image/avif" => "image/avif",
        "image/heic" => "image/heic",
        "text/plain" | "text/csv" | "text/markdown" => "text/plain; charset=utf-8",
        "application/pdf" => "application/pdf",
        "audio/mpeg" => "audio/mpeg",
        "audio/wav" => "audio/wav",
        "audio/ogg" => "audio/ogg",
        "audio/opus" => "audio/opus",
        "audio/flac" => "audio/flac",
        "audio/mp4" => "audio/mp4",
        "audio/aac" => "audio/aac",
        "video/mp4" => "video/mp4",
        "video/webm" => "video/webm",
        "video/quicktime" => "video/quicktime",
        "video/x-matroska" => "video/x-matroska",
        "video/x-msvideo" => "video/x-msvideo",
        "video/ogg" => "video/ogg",
        "video/mpeg" => "video/mpeg",
        _ => OCTET_STREAM,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_come_from_the_extension() {
        assert_eq!(mime_type_for("photo.PNG"), "image/png");
        assert_eq!(mime_type_for("a.b.jpeg"), "image/jpeg");
        assert_eq!(mime_type_for("notes.txt"), "text/plain");
        assert_eq!(mime_type_for("page.html"), "text/html");
        assert_eq!(mime_type_for("noextension"), OCTET_STREAM);
        assert_eq!(mime_type_for(".png"), OCTET_STREAM);
        assert_eq!(mime_type_for("weird.xyz"), OCTET_STREAM);
        assert_eq!(mime_type_for(""), OCTET_STREAM);
    }

    #[test]
    fn active_content_is_served_as_bytes() {
        for active in [
            "text/html",
            "image/svg+xml",
            "text/javascript",
            "application/xml",
            "application/xhtml+xml",
            "text/css",
            "application/json",
            "application/x-shockwave-flash",
            "made/up",
        ] {
            assert_eq!(served_content_type(active), OCTET_STREAM, "{active}");
        }
        assert_eq!(served_content_type("image/png"), "image/png");
        assert_eq!(
            served_content_type("text/plain"),
            "text/plain; charset=utf-8"
        );
        // Every type the table can produce is either passed or neutralised.
        assert_eq!(served_content_type(mime_type_for("x.mp4")), "video/mp4");
    }
}
