// Helpers on operations (`DocOpUtil` in the original): lengths, debug
// formatting, normalization and the canonical XML form of a document.
//
// Derived from Apache Wave, licensed under the Apache License, Version 2.0.

import type { AnnotationBoundaryMap } from "./annotation";
import type { Attributes, AttributesUpdate } from "./attributes";
import { OperationError } from "./error";
import { newNormalizer } from "./normalize";
import { DocInitialization, DocOp, componentTypeName } from "./op";
import { saturatingAdd } from "./utf16";

/** Name of the XML processing instruction used for annotations. */
export const PI_TARGET = "a";

/**
 * Views an operation as a document initialization. Fails if it contains
 * anything but insertions and annotation boundaries.
 */
export function asInitialization(op: DocOp): DocInitialization {
  return DocInitialization.fromOp(op);
}

function literalString(s: string | null): string {
  if (s === null) {
    return "null";
  }
  return `"${s.replaceAll("\\", "\\\\").replaceAll('"', '\\"')}"`;
}

/** Compact single-line rendering of an operation, for logs and tests. */
export function toConciseString(op: DocOp): string {
  let b = "";
  for (const c of op.components) {
    switch (c.type) {
      case "deleteCharacters":
        b += `--${literalString(c.characters)}; `;
        break;
      case "deleteElementEnd":
        b += "x>; ";
        break;
      case "deleteElementStart":
        b += `x< ${c.tag} ${attributesToConciseString(c.attributes)}; `;
        break;
      case "replaceAttributes":
        b += `r@ ${attributesToConciseString(c.oldAttributes)} ${attributesToConciseString(c.newAttributes)}; `;
        break;
      case "retain":
        b += `__${c.itemCount}; `;
        break;
      case "updateAttributes":
        b += `u@ ${attributesUpdateToConciseString(c.update)}; `;
        break;
      case "annotationBoundary":
        b += `|| ${annotationBoundaryToConciseString(c.boundary)}; `;
        break;
      case "characters":
        b += `++${literalString(c.characters)}; `;
        break;
      case "elementEnd":
        b += ">>; ";
        break;
      case "elementStart":
        b += `<< ${c.tag} ${attributesToConciseString(c.attributes)}; `;
        break;
    }
  }
  return b;
}

export function attributesToConciseString(attributes: Attributes): string {
  if (attributes.isEmpty()) {
    return "{}";
  }
  const entries = attributes.entries.map((a) => `${a.name}=${literalString(a.value)}`);
  return `{ ${entries.join(", ")} }`;
}

export function attributesUpdateToConciseString(update: AttributesUpdate): string {
  if (update.isEmpty()) {
    return "{}";
  }
  const entries = update.updates.map(
    (u) => `${u.name}: ${literalString(u.oldValue)} -> ${literalString(u.newValue)}`,
  );
  return `{ ${entries.join(", ")} }`;
}

export function annotationBoundaryToConciseString(map: AnnotationBoundaryMap): string {
  if (map.isEmpty()) {
    return "{}";
  }
  const entries = map.endKeys.map((key) => literalString(key));
  for (const c of map.changes) {
    entries.push(
      `${literalString(c.key)}: ${literalString(c.oldValue)} -> ${literalString(c.newValue)}`,
    );
  }
  return `{ ${entries.join(", ")} }`;
}

/**
 * The minimal, normalized XML representation of a document, with annotations
 * written as `<?a ...?>` processing instructions. Two documents are equal
 * iff their XML strings are equal.
 *
 * Fails if the initialization closes an element that was never opened.
 */
export function toXmlString(op: DocInitialization): string {
  let b = "";
  const currentAnnotations = new Map<string, string>();
  const tags: string[] = [];
  let elementPart: string | null = null;
  const closeElementPart = (): void => {
    if (elementPart !== null) {
      b += elementPart + ">";
      elementPart = null;
    }
  };
  for (const c of op.components) {
    switch (c.type) {
      case "annotationBoundary": {
        // null: the annotation ends or is set to null here.
        const changes = new Map<string, string | null>();
        for (const change of c.boundary.changes) {
          const value = change.newValue;
          if ((currentAnnotations.get(change.key) ?? null) !== value) {
            if (value !== null) {
              currentAnnotations.set(change.key, value);
            } else {
              currentAnnotations.delete(change.key);
            }
            changes.set(change.key, value);
          }
        }
        for (const key of c.boundary.endKeys) {
          if (currentAnnotations.delete(key)) {
            changes.set(key, null);
          }
        }
        if (changes.size === 0) {
          break;
        }
        closeElementPart();
        b += "<?" + PI_TARGET;
        for (const key of [...changes.keys()].sort()) {
          const value = changes.get(key) ?? null;
          b += ` "${xmlTextEscape(annotationEscape(key))}"`;
          if (value !== null) {
            b += `="${xmlTextEscape(annotationEscape(value))}"`;
          }
        }
        b += "?>";
        break;
      }
      case "characters":
        closeElementPart();
        b += xmlTextEscape(c.characters);
        break;
      case "elementStart": {
        closeElementPart();
        let part = `<${c.tag}`;
        if (!c.attributes.isEmpty()) {
          part += " " + attributeString(c.attributes);
        }
        elementPart = part;
        tags.push(c.tag);
        break;
      }
      case "elementEnd": {
        const tag = tags.pop();
        if (tag === undefined) {
          throw OperationError.invalidArgument("to_xml_string: element end without element start");
        }
        if (elementPart !== null) {
          b += elementPart + "/>";
          elementPart = null;
        } else {
          b += `</${tag}>`;
        }
        break;
      }
      default:
        // Unreachable for an initialization built through the public API.
        throw OperationError.notAnInitialization(componentTypeName(c));
    }
  }
  return b;
}

/** Renders attributes as `name="value"` pairs separated by spaces. */
export function attributeString(attributes: Attributes): string {
  return attributes.entries.map((a) => `${a.name}="${xmlAttrEscape(a.value)}"`).join(" ");
}

/** Escapes double quotes only, like the original. */
export function xmlAttrEscape(value: string): string {
  return value.replaceAll('"', "&quot;");
}

export function xmlTextEscape(text: string): string {
  return text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
}

export function xmlTextUnescape(escaped: string): string {
  return escaped.replaceAll("&gt;", ">").replaceAll("&lt;", "<").replaceAll("&amp;", "&");
}

export function annotationEscape(value: string): string {
  return value.replaceAll("\\", "\\\\").replaceAll('"', '\\"').replaceAll("?", "\\q");
}

export function annotationUnescape(escaped: string): string {
  return escaped.replaceAll("\\q", "?").replaceAll('\\"', '"').replaceAll("\\\\", "\\");
}

/**
 * Normalizes an operation: merges adjacent ranges and annotation boundaries
 * and drops no-op components. Fails if the result is ill-formed.
 */
export function normalize(op: DocOp): DocOp {
  const normalizer = newNormalizer();
  for (const c of op.components) {
    normalizer.push(c);
  }
  return DocOp.create(normalizer.finish());
}

/** Normalizes a document initialization. */
export function normalizeInitialization(op: DocInitialization): DocInitialization {
  return asInitialization(normalize(op));
}

// Counts that are not positive numbers add nothing (in Rust they cannot
// occur).
const count = (n: number): number => (n > 0 ? n : 0);

/** Number of items of the document that an operation applies to. */
export function initialDocumentLength(op: DocOp): number {
  let length = 0;
  for (const c of op.components) {
    switch (c.type) {
      case "deleteCharacters":
        length = saturatingAdd(length, c.characters.length);
        break;
      case "deleteElementEnd":
      case "deleteElementStart":
      case "replaceAttributes":
      case "updateAttributes":
        length = saturatingAdd(length, 1);
        break;
      case "retain":
        length = saturatingAdd(length, count(c.itemCount));
        break;
      default:
        break;
    }
  }
  return length;
}

/** Number of items of the document that an operation produces. */
export function resultingDocumentLength(op: DocOp): number {
  let length = 0;
  for (const c of op.components) {
    switch (c.type) {
      case "replaceAttributes":
      case "updateAttributes":
      case "elementEnd":
      case "elementStart":
        length = saturatingAdd(length, 1);
        break;
      case "retain":
        length = saturatingAdd(length, count(c.itemCount));
        break;
      case "characters":
        length = saturatingAdd(length, c.characters.length);
        break;
      default:
        break;
    }
  }
  return length;
}
