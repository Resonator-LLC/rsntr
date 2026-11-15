// Schemas of conversation documents (`ConversationSchemas`).
//
// TypeScript port of the Rust crate `resonator-model` (module `schema`), itself
// derived from `org.waveprotocol.wave.model.schema.conversation`
// (`ConversationSchemas`) and `DefaultDocumentSchema` of Apache Wave,
// licensed under the Apache License, Version 2.0.

import { WAVELET_ID } from "../adt";
import type { DocumentSchema } from "../docop";
import { MANIFEST_DOCUMENT_ID, util as idUtil, type WaveletId } from "../id";
import {
  BOOLEAN_VALUES,
  XmlSchemaConstraints,
  isConversational,
  isNonNegativeInteger,
  isPositiveInteger,
  isValidInteger,
  type SchemaProvider,
} from "./constraints";

function lineContainer(s: XmlSchemaConstraints, element: string): void {
  s.addChildren(element, [
    "line",
    "image",
    "gadget",
    "eqn",
    "experimental",
    "mediasearch",
    "img",
    "reply",
    "profile",
  ]);
  s.containsBlipText([element]);
  s.addRequiredInitial(element, ["line"]);
}

function oneLiner(s: XmlSchemaConstraints, element: string): void {
  s.containsBlipText([element]);
}

function containsFormElements(s: XmlSchemaConstraints, element: string): void {
  s.addChildren(element, [
    "button",
    "check",
    "input",
    "label",
    "password",
    "radiogroup",
    "radio",
    "textarea",
  ]);
}

function blipValueCheck(tag: string, attr: string, value: string): boolean | null {
  if (tag === "line" && attr === "i") {
    return isPositiveInteger(value);
  }
  if (tag === "img" && (attr === "width" || attr === "height")) {
    return isValidInteger(value, 0);
  }
  if ((tag === "lmt" && attr === "t") || (tag === "invitation" && attr === "remaining")) {
    return isNonNegativeInteger(value);
  }
  return null;
}

function blipSchema(): XmlSchemaConstraints {
  const s = new XmlSchemaConstraints();

  
  s.addChildren(null, ["head"]);
  s.addChildren("head", ["timestamp"]);
  s.addChildren("timestamp", ["lmt"]);
  s.addAttrs("lmt", ["t"]);

  s.addChildren(null, ["body"]);
  lineContainer(s, "body");

  s.addAttrWithValues("line", "t", ["h1", "h2", "h3", "h4", "li"]);
  s.addAttrWithValues("line", "listyle", ["decimal"]);
  s.addAttrWithValues("line", "a", ["l", "r", "c", "j"]);
  s.addAttrWithValues("line", "d", ["l", "r"]);
  s.addAttrWithValues("line", "i", []);

  s.addChildren("image", ["caption"]);
  s.addAttrWithValues("image", "attachment", []);
  s.addAttrWithValues("image", "style", ["full"]);
  s.addChildren("image", ["gadget"]);
  oneLiner(s, "caption");
  s.addChildren("caption", ["reply"]);

  oneLiner(s, "label");
  oneLiner(s, "input");

  s.addAttrs("reply", ["id"]);

  containsFormElements(s, "body");
  lineContainer(s, "textarea");

  s.addAttrs("button", ["name"]);
  s.addChildren("button", ["caption", "events"]);
  s.addChildren("events", ["click"]);
  s.addAttrs("click", ["time", "clicker"]);
  s.addAttrs("check", ["name", "submit", "value"]);
  s.addAttrs("radiogroup", ["name", "submit", "value"]);
  s.addAttrs("password", ["name", "submit", "value"]);
  s.addAttrs("textarea", ["name", "submit", "value"]);
  s.addAttrs("input", ["name", "submit"]);
  s.addAttrs("radio", ["name", "group"]);
  s.addAttrs("click", ["time", "clicker"]);
  s.addAttrs("label", ["for"]);

  s.addChildren(
      "gadget",
      ["title", "thumbnail", "category", "state", "pref"],
  );
  s.addAttrs(
      "gadget",
      [
          "url",
          "title",
          "prefs",
          "state",
          "author",
          "height",
          "width",
          "id",
          "extension",
          "ifr",
          "snippet",
          // Wave apps: the bundle's `sha256:<hex>` and its attachment id.
          "app",
          "bundle",
      ],
  );
  for (const gadgetEl of ["category", "state", "pref"]) {
      s.addAttrs(gadgetEl, ["name"]);
  }
  for (const gadgetEl of ["title", "thumbnail", "state", "pref"]) {
      s.addAttrs(gadgetEl, ["value"]);
  }

  s.addChildren("profile", ["profile-field", "gadget"]);
  s.addAttrs("profile-field", ["name", "user-set"]);
  s.addAttrs("profile", ["avatar-url"]);
  s.containsBlipText(["profile-field"]);

  s.addChildren("mediasearch", ["result", "customsearch"]);
  s.addAttrs(
      "mediasearch",
      ["page", "corpora", "query", "selected", "pending", "lang"],
  );
  s.addAttrs(
      "result",
      [
          "thumbnail",
          "thumbwidth",
          "thumbheight",
          "content",
          "url",
          "dispurl",
          "title",
          "snippet",
          "num",
          "type",
          "disphtml",
      ],
  );
  s.addAttrs(
      "customsearch",
      [
          "name",
          "icon",
          "shortname",
          "resultrows",
          "resultcols",
          "addmethod",
      ],
  );

  s.addChildren("body", ["trustreq"]);
  s.containsBlipText(["trustreq"]);
  s.addChildren("trustreq", ["trwave"]);
  s.addAttrs("trustreq", ["from", "numberOfWaves", "userAction"]);
  s.addAttrs("trwave", ["messageCount", "lastModified"]);
  s.containsBlipText(["trwave"]);

  s.addChildren("body", ["blacklist"]);
  s.addAttrs("blacklist", ["address", "contacts"]);

  s.addChildren("body", ["invitation"]);
  s.addAttrs("invitation", ["remaining", "title", "invitedString"]);
  s.addChildren("invitation", ["invited"]);
  s.addAttrs("invited", ["address"]);

  s.addAttrWithValues("eqn", "format", ["tex"]);
  s.containsBlipText(["eqn"]);

  s.addChildren("body", ["settings"]);
  s.addAttrs("settings", ["name"]);
  s.addChildren(
      "settings",
      [
          "bool-setting",
          "radio-setting",
          "text-setting",
          "listbox-setting",
      ],
  );
  s.addAttrs("bool-setting", ["id", "live-value", "saved-value"]);
  s.addAttrs("radio-setting", ["id", "live-value", "saved-value"]);
  s.addAttrs("listbox-setting", ["id", "live-value", "saved-value"]);
  s.addAttrs("text-setting", ["id", "saved-value"]);
  oneLiner(s, "text-setting");

  s.addChildren("body", ["html"]);
  s.addChildren("html", ["data"]);
  s.containsBlipText(["data"]);

  s.addChildren("body", ["experimental"]);
  s.addAttrs("experimental", ["url"]);
  s.addChildren("experimental", ["namevaluepair", "part"]);
  s.addAttrs("part", ["id"]);
  lineContainer(s, "part");
  containsFormElements(s, "part");
  s.addAttrs("namevaluepair", ["name"]);
  s.addAttrs("namevaluepair", ["value"]);

  s.addChildren("body", ["translation"]);
  s.addChildren("translation", ["stanza"]);
  lineContainer(s, "stanza");
  s.addAttrs("stanza", ["lang", "users"]);

  s.addChildren("body", ["extension_installer"]);
  s.addAttrs("extension_installer", ["manifest", "img", "installed"]);

  s.addChildren("body", ["ext-settings"]);
  s.addAttrs("ext-settings", ["manifest", "enabled"]);

  s.addChildren("body", ["gadget-settings"]);
  s.addAttrs("gadget-settings", ["url", "prefs"]);

  s.addAttrs("img", ["alt", "height", "width", "src"]);

  s.addChildren("body", ["quote"]);
  lineContainer(s, "quote");

    s.setValueCheck(blipValueCheck);
  return s;
}

/**
 * Schema of a blip document (`BLIP_SCHEMA_CONSTRAINTS`,
 * `DefaultDocumentSchema`): an optional `head`, and a `body` of lines of
 * text with inline elements. A `body` (like `textarea` and the other line
 * containers) must start with a `line` element.
 */
export const BLIP_SCHEMA_CONSTRAINTS: XmlSchemaConstraints = blipSchema();

function manifestValueCheck(tag: string, attr: string, value: string): boolean | null {
  if (tag === "conversation") {
    switch (attr) {
      case "anchorWavelet": {
        const id = WAVELET_ID.fromAttribute(value);
        return id !== null && isConversational(id);
      }
      case "anchorManifestOffset":
      case "anchorVersion":
      case "anchorOffset":
        return isNonNegativeInteger(value);
      case "anchorBlip":
        return idUtil.isBlipId(value);
      default:
        return null;
    }
  }
  if (tag === "blip" && attr === "id") {
    return idUtil.isBlipId(value);
  }
  return null;
}

function manifestSchema(): XmlSchemaConstraints {
  const s = new XmlSchemaConstraints();
  s.addChildren(null, ["conversation"]);
  s.addAttrs("conversation", [
    "anchorWavelet",
    "anchorManifestOffset",
    "anchorVersion",
    "anchorBlip",
    "anchorOffset",
    "sort",
  ]);
  s.addChildren("conversation", ["blip"]);
  s.addAttrs("blip", ["id"]);
  s.addAttrWithValues("blip", "deleted", BOOLEAN_VALUES);
  s.addChildren("blip", ["thread"]);
  s.addChildren("blip", ["peer"]);
  s.addAttrs("thread", ["id"]);
  s.addAttrWithValues("thread", "inline", BOOLEAN_VALUES);
  s.addChildren("thread", ["blip"]);
  s.addAttrs("peer", ["id"]);
  s.setValueCheck(manifestValueCheck);
  return s;
}

/**
 * Schema of the conversation manifest, the `conversation` document
 * (`MANIFEST_SCHEMA_CONSTRAINTS`): a `conversation` element of nested
 * `blip` and `thread` elements. Blip ids must be blip ids, and the anchor
 * wavelet a conversational wavelet id in the form `domain/id`.
 */
export const MANIFEST_SCHEMA_CONSTRAINTS: XmlSchemaConstraints = manifestSchema();

/**
 * The schemas of the documents of conversational wavelets: blips and the
 * manifest.
 */
export const ConversationSchemas: SchemaProvider = {
  schemaForId(waveletId: WaveletId, documentId: string): DocumentSchema | null {
    if (!isConversational(waveletId)) {
      return null;
    }
    if (idUtil.isBlipId(documentId)) {
      return BLIP_SCHEMA_CONSTRAINTS;
    }
    return documentId === MANIFEST_DOCUMENT_ID ? MANIFEST_SCHEMA_CONSTRAINTS : null;
  },
};
