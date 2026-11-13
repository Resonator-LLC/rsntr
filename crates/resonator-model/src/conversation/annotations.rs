//! The annotation keys used in blip documents (`AnnotationConstants`).

/// Prefix for style annotations (follows CSS).
pub const STYLE_PREFIX: &str = "style";
pub const STYLE_BG_COLOR: &str = "style/backgroundColor";
pub const STYLE_COLOR: &str = "style/color";
pub const STYLE_FONT_FAMILY: &str = "style/fontFamily";
pub const STYLE_FONT_SIZE: &str = "style/fontSize";
pub const STYLE_FONT_STYLE: &str = "style/fontStyle";
pub const STYLE_FONT_WEIGHT: &str = "style/fontWeight";
pub const STYLE_TEXT_DECORATION: &str = "style/textDecoration";
pub const STYLE_VERTICAL_ALIGN: &str = "style/verticalAlign";

/// All style keys.
pub const STYLE_KEYS: [&str; 8] = [
    STYLE_BG_COLOR,
    STYLE_COLOR,
    STYLE_FONT_FAMILY,
    STYLE_FONT_SIZE,
    STYLE_FONT_STYLE,
    STYLE_FONT_WEIGHT,
    STYLE_TEXT_DECORATION,
    STYLE_VERTICAL_ALIGN,
];

/// Prefix for user annotations.
pub const USER_PREFIX: &str = "user";
/// The range of text selected by the user; followed by a session id.
pub const USER_RANGE: &str = "user/r/";
/// The user's selection focus, always extends to the document end;
/// followed by a session id.
pub const USER_END: &str = "user/e/";
/// User activity annotation, always covers the whole document; followed by
/// a session id.
pub const USER_DATA: &str = "user/d/";

/// Prefix for link annotations.
pub const LINK_PREFIX: &str = "link";
/// An automatically created link.
pub const LINK_AUTO: &str = "link/auto";
/// A user-created link.
pub const LINK_MANUAL: &str = "link/manual";
/// A link to a wave.
pub const LINK_WAVE: &str = "link/wave";

/// Prefix for mention annotations. Not in the original, which had no
/// mentions; Google Wave's `@name` mentions were plain text.
pub const MENTION_PREFIX: &str = "mention";
/// An `@name` mention of a participant: the value is their address.
pub const MENTION_USER: &str = "mention/user";

/// Prefix for spelling annotations.
pub const SPELLY_PREFIX: &str = "spell";
/// Prefix for language annotations.
pub const LANGUAGE_PREFIX: &str = "lang";
/// Prefix for translation annotations.
pub const ROSY_PREFIX: &str = "tr";
/// An automatically translated section.
pub const ROSY_AUTO: &str = "tr/1";
