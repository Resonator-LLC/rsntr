// Consumers of operation components.
//
// In the Rust port `OpSink` lives in `op.rs`; it is a separate module here
// (re-exported from `op.ts`) only to keep the module graph free of
// evaluation-time cycles.
//
// Derived from Apache Wave (`EvaluatingDocOpCursor`), licensed under the
// Apache License, Version 2.0.

import type { AnnotationBoundaryMap } from "./annotation";
import type { Attributes, AttributesUpdate } from "./attributes";
import type { DocOpComponent } from "./op";

/**
 * A consumer of operation components (the role of the original's
 * `EvaluatingDocOpCursor`). Normalizers are sinks that wrap another sink.
 */
export abstract class OpSink {
  abstract push(component: DocOpComponent): void;

  /** Flushes any buffered state and returns the collected components. */
  abstract finish(): DocOpComponent[];

  annotationBoundary(boundary: AnnotationBoundaryMap): void {
    this.push({ type: "annotationBoundary", boundary });
  }

  characters(characters: string): void {
    this.push({ type: "characters", characters });
  }

  elementStart(tag: string, attributes: Attributes): void {
    this.push({ type: "elementStart", tag, attributes });
  }

  elementEnd(): void {
    this.push({ type: "elementEnd" });
  }

  retain(itemCount: number): void {
    this.push({ type: "retain", itemCount });
  }

  deleteCharacters(characters: string): void {
    this.push({ type: "deleteCharacters", characters });
  }

  deleteElementStart(tag: string, attributes: Attributes): void {
    this.push({ type: "deleteElementStart", tag, attributes });
  }

  deleteElementEnd(): void {
    this.push({ type: "deleteElementEnd" });
  }

  replaceAttributes(oldAttributes: Attributes, newAttributes: Attributes): void {
    this.push({ type: "replaceAttributes", oldAttributes, newAttributes });
  }

  updateAttributes(update: AttributesUpdate): void {
    this.push({ type: "updateAttributes", update });
  }
}

/** A sink that collects components as given (Rust's `Vec<DocOpComponent>`). */
export class BufferSink extends OpSink {
  private readonly components: DocOpComponent[] = [];

  push(component: DocOpComponent): void {
    this.components.push(component);
  }

  finish(): DocOpComponent[] {
    return this.components;
  }
}
