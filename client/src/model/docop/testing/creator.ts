// Constructors for simple single-purpose operations (`DocOpCreator`).
//
// `size` is always the length of the document the operation applies to. The
// functions throw if the arguments describe an ill-formed operation.
//
// Derived from Apache Wave, licensed under the Apache License, Version 2.0.

import { AnnotationBoundaryMap } from "../annotation";
import { type Attributes, AttributesUpdate } from "../attributes";
import { Component, DocOp, type DocOpComponent } from "../op";

/**
 * Builds an operation from components, dropping empty retains and character
 * runs.
 */
function build(components: DocOpComponent[]): DocOp {
  return DocOp.create(
    components.filter((c) => {
      switch (c.type) {
        case "retain":
          return c.itemCount !== 0;
        case "characters":
        case "deleteCharacters":
          return c.characters.length > 0;
        default:
          return true;
      }
    }),
  );
}

export function insertCharacters(size: number, location: number, characters: string): DocOp {
  return build([
    Component.retain(location),
    Component.characters(characters),
    Component.retain(size - location),
  ]);
}

export function insertElement(
  size: number,
  location: number,
  tag: string,
  attributes: Attributes,
): DocOp {
  return build([
    Component.retain(location),
    Component.elementStart(tag, attributes),
    Component.elementEnd(),
    Component.retain(size - location),
  ]);
}

export function deleteCharacters(size: number, location: number, characters: string): DocOp {
  return build([
    Component.retain(location),
    Component.deleteCharacters(characters),
    Component.retain(size - location - characters.length),
  ]);
}

export function deleteElement(
  size: number,
  location: number,
  tag: string,
  attributes: Attributes,
): DocOp {
  return build([
    Component.retain(location),
    Component.deleteElementStart(tag, attributes),
    Component.deleteElementEnd(),
    Component.retain(size - location - 2),
  ]);
}

export function replaceAttributes(
  size: number,
  location: number,
  oldAttributes: Attributes,
  newAttributes: Attributes,
): DocOp {
  return build([
    Component.retain(location),
    Component.replaceAttributes(oldAttributes, newAttributes),
    Component.retain(size - location - 1),
  ]);
}

export function setAttribute(
  size: number,
  location: number,
  name: string,
  oldValue: string | null,
  newValue: string | null,
): DocOp {
  return build([
    Component.retain(location),
    Component.updateAttributes(AttributesUpdate.single(name, oldValue, newValue)),
    Component.retain(size - location - 1),
  ]);
}

export function setAnnotation(
  size: number,
  start: number,
  end: number,
  key: string,
  oldValue: string | null,
  newValue: string | null,
): DocOp {
  const components = [Component.retain(start)];
  if (end > start) {
    components.push(
      Component.annotationBoundary(AnnotationBoundaryMap.changesOnly([[key, oldValue, newValue]])),
      Component.retain(end - start),
      Component.annotationBoundary(AnnotationBoundaryMap.ends([key])),
    );
  }
  components.push(Component.retain(size - end));
  return build(components);
}

export function identity(size: number): DocOp {
  return build([Component.retain(size)]);
}
