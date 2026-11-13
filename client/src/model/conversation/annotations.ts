// The annotation keys used in blip documents (`AnnotationConstants`).
//
// TypeScript port of the Rust crate `resonator-model` (module
// `conversation::annotations`), itself derived from
// `org.waveprotocol.wave.model.conversation.AnnotationConstants` of Apache
// Wave, licensed under the Apache License, Version 2.0.

/** Prefix for style annotations (follows CSS). */
export const STYLE_PREFIX = "style";
export const STYLE_BG_COLOR = "style/backgroundColor";
export const STYLE_COLOR = "style/color";
export const STYLE_FONT_FAMILY = "style/fontFamily";
export const STYLE_FONT_SIZE = "style/fontSize";
export const STYLE_FONT_STYLE = "style/fontStyle";
export const STYLE_FONT_WEIGHT = "style/fontWeight";
export const STYLE_TEXT_DECORATION = "style/textDecoration";
export const STYLE_VERTICAL_ALIGN = "style/verticalAlign";

/** All style keys. */
export const STYLE_KEYS: readonly string[] = [
  STYLE_BG_COLOR,
  STYLE_COLOR,
  STYLE_FONT_FAMILY,
  STYLE_FONT_SIZE,
  STYLE_FONT_STYLE,
  STYLE_FONT_WEIGHT,
  STYLE_TEXT_DECORATION,
  STYLE_VERTICAL_ALIGN,
];

/** Prefix for user annotations. */
export const USER_PREFIX = "user";
/** The range of text selected by the user; followed by a session id. */
export const USER_RANGE = "user/r/";
/**
 * The user's selection focus, always extends to the document end; followed
 * by a session id.
 */
export const USER_END = "user/e/";
/**
 * User activity annotation, always covers the whole document; followed by a
 * session id.
 */
export const USER_DATA = "user/d/";

/** Prefix for link annotations. */
export const LINK_PREFIX = "link";
/** An automatically created link. */
export const LINK_AUTO = "link/auto";
/** A user-created link. */
export const LINK_MANUAL = "link/manual";
/** A link to a wave. */
export const LINK_WAVE = "link/wave";

/** Prefix for mention annotations (not in the original). */
export const MENTION_PREFIX = "mention";
/** An `@name` mention of a participant: the value is their address. */
export const MENTION_USER = "mention/user";

/** Prefix for spelling annotations. */
export const SPELLY_PREFIX = "spell";
/** Prefix for language annotations. */
export const LANGUAGE_PREFIX = "lang";
/** Prefix for translation annotations. */
export const ROSY_PREFIX = "tr";
/** An automatically translated section. */
export const ROSY_AUTO = "tr/1";
