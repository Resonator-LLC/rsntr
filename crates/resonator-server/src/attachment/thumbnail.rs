//! Thumbnails of image attachments, and the stand-in pictures for
//! everything else.
//!
//! Follows `AttachmentService.makeThumbnail` of Apache Wave (Apache
//! License 2.0): a JPEG of at most 200 by 200 pixels with the image's
//! proportions, on black. Only PNG, JPEG, GIF and WebP are decoded, and
//! only within limits, so that a small file claiming to be an enormous
//! picture costs nothing.

use std::io::Cursor;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::{ImageFormat, ImageReader, Limits, RgbImage};

/// The content type of stored thumbnails
/// (`AttachmentService.THUMBNAIL_MIME_TYPE`).
pub const THUMBNAIL_MIME_TYPE: &str = "image/jpeg";

/// Width and height of the stand-in pictures
/// (`AttachmentService.THUMBNAIL_PATTERN_WIDTH`, `_HEIGHT`).
pub const PATTERN_WIDTH: u32 = 95;
pub const PATTERN_HEIGHT: u32 = 60;

const MAX_THUMBNAIL_WIDTH: u32 = 200;
const MAX_THUMBNAIL_HEIGHT: u32 = 200;

/// Limits on the images that are decoded for a thumbnail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    /// Longest side, in pixels.
    pub max_dimension: u32,
    /// Most pixels.
    pub max_pixels: u64,
    /// Most memory the decoder may allocate, in bytes.
    pub max_alloc: u64,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        DecodeLimits {
            max_dimension: 16_384,
            max_pixels: 50_000_000,
            max_alloc: 256 * 1024 * 1024,
        }
    }
}

/// A thumbnail and the size of the image it was made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub image_width: u32,
    pub image_height: u32,
    pub width: u32,
    pub height: u32,
    pub jpeg: Vec<u8>,
}

/// The size of the thumbnail of an image (`makeThumbnail`): the image's
/// own size if it fits in 200 by 200, otherwise the largest size of its
/// proportions that does.
pub fn thumbnail_size(image_width: u32, image_height: u32) -> (u32, u32) {
    let (width, height) = (u64::from(image_width), u64::from(image_height));
    let mut thumbnail_width = width.min(u64::from(MAX_THUMBNAIL_WIDTH));
    let mut thumbnail_height = height.min(u64::from(MAX_THUMBNAIL_HEIGHT));
    if width * thumbnail_height < height * thumbnail_width {
        thumbnail_width = width * thumbnail_height / height.max(1);
    } else {
        thumbnail_height = height * thumbnail_width / width.max(1);
    }
    // The original fails on an extreme aspect ratio, which gives zero.
    (
        thumbnail_width.max(1) as u32,
        thumbnail_height.max(1) as u32,
    )
}

/// Makes the thumbnail of the image in a file; `None` if the file is not
/// an image of a supported format, is damaged, or exceeds the limits.
/// Decoding is CPU-bound: call it off the async runtime.
pub fn make_thumbnail(path: &Path, limits: DecodeLimits) -> Option<Thumbnail> {
    let reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let format = reader.format()?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return None;
    }
    // The header alone says how large the picture claims to be.
    let (image_width, image_height) = reader.into_dimensions().ok()?;
    if image_width == 0
        || image_height == 0
        || image_width > limits.max_dimension
        || image_height > limits.max_dimension
        || u64::from(image_width) * u64::from(image_height) > limits.max_pixels
    {
        return None;
    }
    let mut reader = ImageReader::open(path).ok()?;
    reader.set_format(format);
    let mut decoder_limits = Limits::default();
    decoder_limits.max_image_width = Some(limits.max_dimension);
    decoder_limits.max_image_height = Some(limits.max_dimension);
    decoder_limits.max_alloc = Some(limits.max_alloc);
    reader.limits(decoder_limits);
    let image = reader.decode().ok()?;
    let (width, height) = thumbnail_size(image_width, image_height);
    let scaled = image.thumbnail_exact(width, height).into_rgba8();
    // On black, as the original draws it.
    let mut flat = RgbImage::new(width, height);
    for (target, source) in flat.pixels_mut().zip(scaled.pixels()) {
        let alpha = u32::from(source[3]);
        for channel in 0..3 {
            target[channel] = ((u32::from(source[channel]) * alpha + 127) / 255) as u8;
        }
    }
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(Cursor::new(&mut jpeg), 85)
        .encode_image(&flat)
        .ok()?;
    Some(Thumbnail {
        image_width,
        image_height,
        width,
        height,
        jpeg,
    })
}

/// The content type of the stand-in pictures.
pub const PATTERN_MIME_TYPE: &str = "image/svg+xml";

/// The stand-in picture for an attachment that is not an image, by the
/// family of its content type. The original serves PNG files from a
/// "thumbnail patterns" directory, one per content type with a default;
/// these are built in.
pub fn pattern_for(mime_type: &str) -> String {
    let (label, colour) = match mime_type {
        "application/pdf" => ("PDF", "#b5403a"),
        "application/zip"
        | "application/gzip"
        | "application/x-tar"
        | "application/x-7z-compressed"
        | "application/vnd.rar"
        | "application/x-bzip2"
        | "application/x-xz" => ("ZIP", "#8a6d3b"),
        other => match other.split('/').next().unwrap_or("") {
            "text" => ("TXT", "#4a6fa5"),
            "audio" => ("AUDIO", "#6b4fa0"),
            "video" => ("VIDEO", "#a04f7d"),
            "image" => ("IMAGE", "#3f8f6b"),
            _ => ("FILE", "#6b7280"),
        },
    };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{PATTERN_WIDTH}\" \
         height=\"{PATTERN_HEIGHT}\" viewBox=\"0 0 {PATTERN_WIDTH} {PATTERN_HEIGHT}\">\
         <rect width=\"100%\" height=\"100%\" rx=\"6\" fill=\"#f3f4f6\"/>\
         <path d=\"M32 8h22l10 10v34H32z\" fill=\"#fff\" stroke=\"{colour}\" stroke-width=\"2\"/>\
         <path d=\"M54 8v10h10\" fill=\"none\" stroke=\"{colour}\" stroke-width=\"2\"/>\
         <text x=\"48\" y=\"42\" text-anchor=\"middle\" font-family=\"sans-serif\" \
         font-size=\"9\" font-weight=\"bold\" fill=\"{colour}\">{label}</text></svg>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_sizes_follow_the_original() {
        assert_eq!(thumbnail_size(100, 50), (100, 50));
        assert_eq!(thumbnail_size(200, 200), (200, 200));
        assert_eq!(thumbnail_size(400, 200), (200, 100));
        assert_eq!(thumbnail_size(200, 400), (100, 200));
        assert_eq!(thumbnail_size(1920, 1080), (200, 112));
        assert_eq!(thumbnail_size(1000, 100), (200, 20));
        assert_eq!(thumbnail_size(100, 1000), (20, 200));
        assert_eq!(thumbnail_size(10_000, 1), (200, 1));
    }

    #[test]
    fn patterns_are_plain_pictures() {
        for mime in ["application/pdf", "text/plain", "video/mp4", "x/y", ""] {
            let svg = pattern_for(mime);
            assert!(svg.starts_with("<svg "), "{mime}");
            assert!(!svg.contains("script"), "{mime}");
        }
        assert!(pattern_for("application/pdf").contains("PDF"));
        assert!(pattern_for("audio/mpeg").contains("AUDIO"));
        assert!(pattern_for("application/octet-stream").contains("FILE"));
    }
}
